//! Compiler-private native value layouts recognized by the production emitter.
//!
//! Layout classification only describes a physical shape. It does not by itself authorize an
//! allocation removal or opt a function into the private native ABI. Returning a signature from
//! [`NativeSignatureShape::for_supported_function`] is the stronger promise shared by
//! runtime-requirement analysis and LLVM emission: the call does not need the universal `Value`
//! ABI.

use loom_mir::{Function, Program, Type, TypeDefKind, TypeId};

/// A scalar with a compiler-private, unboxed native representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeScalar {
    Unit,
    Bool,
    Int,
    Float,
}

impl NativeScalar {
    #[must_use]
    const fn for_type(ty: &Type) -> Option<Self> {
        match ty {
            Type::Unit => Some(Self::Unit),
            Type::Bool => Some(Self::Bool),
            Type::Int => Some(Self::Int),
            Type::Float => Some(Self::Float),
            Type::Never
            | Type::Text
            | Type::Tuple(_)
            | Type::List(_)
            | Type::Nominal(_, _)
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Task(_)
            | Type::TaskOutcome(_)
            | Type::View { .. }
            | Type::Error => None,
        }
    }
}

/// A monomorphic record whose fields are direct primitive scalars and which has no invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativePodRecord {
    nominal: TypeId,
    fields: Vec<NativeScalar>,
}

impl NativePodRecord {
    #[must_use]
    pub(crate) const fn nominal(&self) -> TypeId {
        self.nominal
    }

    #[must_use]
    pub(crate) fn fields(&self) -> &[NativeScalar] {
        &self.fields
    }
}

/// The native representation of one source-language value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativeLayout {
    Scalar(NativeScalar),
    PodRecord(NativePodRecord),
}

impl NativeLayout {
    /// Classifies the physical shape only. Consumers must separately prove that a particular
    /// storage or calling-convention optimization is safe.
    #[must_use]
    pub(crate) fn classify(program: &Program, ty: &Type) -> Option<Self> {
        if let Some(scalar) = NativeScalar::for_type(ty) {
            return Some(Self::Scalar(scalar));
        }

        let Type::Nominal(nominal, arguments) = ty else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let definition = program.type_def(*nominal)?;
        if definition.type_parameters != 0 {
            return None;
        }
        let TypeDefKind::Record {
            fields,
            invariant: None,
        } = &definition.kind
        else {
            return None;
        };
        let fields = fields
            .iter()
            .map(|field| NativeScalar::for_type(&field.ty))
            .collect::<Option<Vec<_>>>()?;
        Some(Self::PodRecord(NativePodRecord {
            nominal: *nominal,
            fields,
        }))
    }
}

/// How one source parameter crosses the compiler-private native ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativePassMode {
    Value,
    InOut,
}

/// Physical layout and passing mode for one compiler-private parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeParameterLayout {
    layout: NativeLayout,
    mode: NativePassMode,
}

impl NativeParameterLayout {
    #[must_use]
    pub(crate) const fn layout(&self) -> &NativeLayout {
        &self.layout
    }

    #[must_use]
    pub(crate) const fn mode(&self) -> NativePassMode {
        self.mode
    }
}

/// Parameter and result layouts for a function supported by the private native ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeSignatureShape {
    parameters: Vec<NativeParameterLayout>,
    result: NativeLayout,
}

/// Runtime/status convention paired with one compiler-private native signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeEffectAbi {
    PureNoFault,
    RuntimeStatus,
}

/// A production-supported native shape with its closed-world runtime effect ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeSignature {
    shape: NativeSignatureShape,
    effect: NativeEffectAbi,
}

impl NativeSignatureShape {
    /// Selects only functions which the production LLVM emitter fully lowers through a private
    /// native ABI. POD records are deliberately restricted to a mutable receiver and/or result:
    /// every other aggregate boundary keeps using the universal `Value` ABI.
    #[must_use]
    pub(crate) fn for_supported_function(program: &Program, function: &Function) -> Option<Self> {
        if function.is_async || function.type_parameters != 0 || !function.witness_params.is_empty()
        {
            return None;
        }

        let mut parameters = Vec::with_capacity(function.params.len());
        for (index, parameter) in function.params.iter().enumerate() {
            let (layout, mode) =
                if index == 0 && function.receiver == Some(loom_mir::Receiver::Mutable) {
                    let layout = NativeLayout::classify(program, &parameter.ty)?;
                    if !matches!(layout, NativeLayout::PodRecord(_)) {
                        return None;
                    }
                    (layout, NativePassMode::InOut)
                } else {
                    (
                        NativeLayout::Scalar(NativeScalar::for_type(&parameter.ty)?),
                        NativePassMode::Value,
                    )
                };
            parameters.push(NativeParameterLayout { layout, mode });
        }
        if function.receiver == Some(loom_mir::Receiver::Readonly)
            || (function.receiver.is_some()
                && !matches!(parameters.first(), Some(parameter) if parameter.mode == NativePassMode::InOut))
        {
            return None;
        }

        let result = NativeLayout::classify(program, &function.return_ty)?;
        let uses_pod = matches!(result, NativeLayout::PodRecord(_))
            || parameters
                .iter()
                .any(|parameter| matches!(parameter.layout, NativeLayout::PodRecord(_)));
        if uses_pod
            && (function.call_plan.receiver_invariant.is_some()
                || !function.call_plan.requires.is_empty()
                || !function.call_plan.ensures.is_empty())
        {
            return None;
        }
        Some(Self { parameters, result })
    }

    #[must_use]
    pub(crate) fn parameters(&self) -> &[NativeParameterLayout] {
        &self.parameters
    }

    #[must_use]
    pub(crate) const fn result(&self) -> &NativeLayout {
        &self.result
    }

    #[must_use]
    pub(crate) fn uses_pod(&self) -> bool {
        matches!(self.result, NativeLayout::PodRecord(_))
            || self
                .parameters
                .iter()
                .any(|parameter| matches!(parameter.layout, NativeLayout::PodRecord(_)))
    }

    #[must_use]
    pub(crate) fn with_effect(self, effect: NativeEffectAbi) -> NativeSignature {
        NativeSignature {
            shape: self,
            effect,
        }
    }
}

impl NativeSignature {
    #[must_use]
    pub(crate) const fn shape(&self) -> &NativeSignatureShape {
        &self.shape
    }

    #[must_use]
    pub(crate) const fn effect(&self) -> NativeEffectAbi {
        self.effect
    }
}

#[cfg(test)]
#[allow(clippy::default_trait_access)]
mod tests {
    use std::collections::BTreeMap;

    use loom_mir::{
        Block, CallPlan, ConceptId, Constant, Contract, ContractExpr, ContractExprKind, FieldDef,
        Function, FunctionId, LocalDecl, LocalId, Program, Receiver, Type, TypeDef, TypeDefKind,
        TypeId, WitnessParam,
    };

    use super::{
        NativeEffectAbi, NativeLayout, NativePassMode, NativeScalar, NativeSignatureShape,
    };

    fn scalar_int_function() -> Function {
        Function {
            id: FunctionId(0),
            name: "identity".into(),
            span: Default::default(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: vec![LocalDecl {
                id: LocalId(0),
                name: "value".into(),
                ty: Type::Int,
                mutable: false,
                span: Default::default(),
            }],
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Int,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: None,
                span: Default::default(),
            },
            call_plan: CallPlan::default(),
        }
    }

    fn record_type(
        id: u32,
        type_parameters: u32,
        fields: Vec<Type>,
        invariant: Option<Contract>,
    ) -> TypeDef {
        TypeDef {
            id: TypeId(id),
            name: format!("Record{id}"),
            span: Default::default(),
            type_parameters,
            kind: TypeDefKind::Record {
                fields: fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| FieldDef {
                        name: format!("field{index}"),
                        ty,
                        span: Default::default(),
                    })
                    .collect(),
                invariant,
            },
        }
    }

    fn true_contract() -> Contract {
        Contract {
            code: "true".into(),
            span: Default::default(),
            expression: ContractExpr {
                kind: ContractExprKind::Constant(Constant::Bool(true)),
                span: Default::default(),
            },
        }
    }

    #[test]
    fn classifies_supported_scalars_and_direct_primitive_pod_records() {
        let mut program = Program::default();
        program.types.push(record_type(
            0,
            0,
            vec![Type::Unit, Type::Bool, Type::Int, Type::Float],
            None,
        ));

        for (ty, expected) in [
            (Type::Unit, NativeScalar::Unit),
            (Type::Bool, NativeScalar::Bool),
            (Type::Int, NativeScalar::Int),
            (Type::Float, NativeScalar::Float),
        ] {
            assert_eq!(
                NativeLayout::classify(&program, &ty),
                Some(NativeLayout::Scalar(expected))
            );
        }

        let layout = NativeLayout::classify(&program, &Type::Nominal(TypeId(0), Vec::new()))
            .expect("direct primitive record should have a native layout");
        let NativeLayout::PodRecord(record) = layout else {
            panic!("record was classified as a scalar");
        };
        assert_eq!(record.nominal(), TypeId(0));
        assert_eq!(
            record.fields(),
            &[
                NativeScalar::Unit,
                NativeScalar::Bool,
                NativeScalar::Int,
                NativeScalar::Float,
            ]
        );
    }

    #[test]
    fn rejects_non_monomorphic_invariant_nested_and_non_primitive_records() {
        let program = Program {
            types: vec![
                record_type(0, 0, vec![Type::Int], None),
                record_type(1, 1, vec![Type::Int], None),
                record_type(2, 0, vec![Type::Int], Some(true_contract())),
                record_type(3, 0, vec![Type::Nominal(TypeId(0), Vec::new())], None),
                record_type(4, 0, vec![Type::Text], None),
                TypeDef {
                    id: TypeId(5),
                    name: "Choice".into(),
                    span: Default::default(),
                    type_parameters: 0,
                    kind: TypeDefKind::Enum {
                        variants: Vec::new(),
                    },
                },
            ],
            ..Program::default()
        };

        for ty in [
            Type::Nominal(TypeId(0), vec![Type::Int]),
            Type::Nominal(TypeId(1), Vec::new()),
            Type::Nominal(TypeId(2), Vec::new()),
            Type::Nominal(TypeId(3), Vec::new()),
            Type::Nominal(TypeId(4), Vec::new()),
            Type::Nominal(TypeId(5), Vec::new()),
            Type::Nominal(TypeId(99), Vec::new()),
            Type::Text,
        ] {
            assert_eq!(NativeLayout::classify(&program, &ty), None, "{ty:?}");
        }
    }

    #[test]
    fn selects_production_primitive_scalar_shapes() {
        let program = Program::default();
        let shape = NativeSignatureShape::for_supported_function(&program, &scalar_int_function())
            .expect("scalar Int function should have a private native shape");
        let int = NativeLayout::Scalar(NativeScalar::Int);
        assert_eq!(shape.parameters()[0].layout(), &int);
        assert_eq!(shape.parameters()[0].mode(), NativePassMode::Value);
        assert_eq!(shape.result(), &int);

        let signature = shape.clone().with_effect(NativeEffectAbi::PureNoFault);
        assert_eq!(signature.shape(), &shape);
        assert_eq!(signature.effect(), NativeEffectAbi::PureNoFault);
        assert_eq!(
            shape.with_effect(NativeEffectAbi::RuntimeStatus).effect(),
            NativeEffectAbi::RuntimeStatus
        );

        for scalar in [Type::Unit, Type::Bool, Type::Float] {
            let mut function = scalar_int_function();
            function.params[0].ty = scalar.clone();
            function.return_ty = scalar.clone();
            let shape = NativeSignatureShape::for_supported_function(&program, &function)
                .unwrap_or_else(|| panic!("{scalar:?} should have a native scalar shape"));
            assert_eq!(shape.parameters().len(), 1);
            assert_eq!(shape.parameters()[0].layout(), shape.result());
        }
    }

    #[test]
    fn selects_only_representation_safe_pod_receiver_and_result_boundaries() {
        let mut program = Program {
            types: vec![record_type(0, 0, vec![Type::Int, Type::Bool], None)],
            ..Program::default()
        };
        let record = Type::Nominal(TypeId(0), Vec::new());

        let mut method = scalar_int_function();
        method.name = "update".into();
        method.receiver = Some(Receiver::Mutable);
        method.params[0].name = "self".into();
        method.params[0].ty = record.clone();
        method.params.push(LocalDecl {
            id: LocalId(1),
            name: "value".into(),
            ty: Type::Int,
            mutable: false,
            span: Default::default(),
        });
        method.return_ty = Type::Unit;
        let shape = NativeSignatureShape::for_supported_function(&program, &method)
            .expect("mutable POD receiver should have a private inout ABI");
        assert!(shape.uses_pod());
        assert_eq!(shape.parameters()[0].mode(), NativePassMode::InOut);
        assert!(matches!(
            shape.parameters()[0].layout(),
            NativeLayout::PodRecord(_)
        ));
        assert_eq!(shape.parameters()[1].mode(), NativePassMode::Value);

        let mut producer = scalar_int_function();
        producer.params.clear();
        producer.return_ty = record.clone();
        let shape = NativeSignatureShape::for_supported_function(&program, &producer)
            .expect("POD return should have a private result ABI");
        assert!(matches!(shape.result(), NativeLayout::PodRecord(_)));

        method.receiver = Some(Receiver::Readonly);
        assert!(NativeSignatureShape::for_supported_function(&program, &method).is_none());
        method.receiver = Some(Receiver::Mutable);
        method.call_plan.requires.push(true_contract());
        assert!(NativeSignatureShape::for_supported_function(&program, &method).is_none());

        let mut ordinary_parameter = scalar_int_function();
        ordinary_parameter.params[0].ty = record;
        assert!(
            NativeSignatureShape::for_supported_function(&program, &ordinary_parameter).is_none()
        );

        program.types[0] = record_type(0, 0, vec![Type::Int], Some(true_contract()));
        assert!(NativeSignatureShape::for_supported_function(&program, &producer).is_none());
    }

    #[test]
    fn rejects_shapes_not_supported_by_the_production_emitter() {
        let base = scalar_int_function();

        let mut asynchronous = base.clone();
        asynchronous.is_async = true;
        let mut generic = base.clone();
        generic.type_parameters = 1;
        let mut receiver = base.clone();
        receiver.receiver = Some(Receiver::Readonly);
        let mut witnessed = base.clone();
        witnessed.witness_params.push(WitnessParam {
            target: Type::Int,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
            span: Default::default(),
        });
        let mut record_parameter = base;
        record_parameter.params[0].ty = Type::Nominal(TypeId(0), Vec::new());

        for function in [asynchronous, generic, receiver, witnessed, record_parameter] {
            assert!(
                NativeSignatureShape::for_supported_function(&Program::default(), &function)
                    .is_none(),
                "unsupported function shape was selected: {}",
                function.name
            );
        }
    }
}
