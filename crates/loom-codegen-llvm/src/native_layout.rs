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

/// Parameter and result layouts for a function supported by the private native ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeSignatureShape {
    parameters: Vec<NativeLayout>,
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
    /// native ABI. This deliberately preserves the current monomorphic scalar-`Int` slice.
    #[must_use]
    pub(crate) fn for_supported_function(function: &Function) -> Option<Self> {
        if function.is_async
            || function.type_parameters != 0
            || function.receiver.is_some()
            || !function.witness_params.is_empty()
            || function.return_ty != Type::Int
            || !function
                .params
                .iter()
                .all(|parameter| parameter.ty == Type::Int)
        {
            return None;
        }

        let int = NativeLayout::Scalar(NativeScalar::Int);
        Some(Self {
            parameters: vec![int.clone(); function.params.len()],
            result: int,
        })
    }

    #[must_use]
    pub(crate) fn parameters(&self) -> &[NativeLayout] {
        &self.parameters
    }

    #[must_use]
    pub(crate) const fn result(&self) -> &NativeLayout {
        &self.result
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

    use super::{NativeEffectAbi, NativeLayout, NativeScalar, NativeSignatureShape};

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
    fn selects_the_production_scalar_int_shape() {
        let shape = NativeSignatureShape::for_supported_function(&scalar_int_function())
            .expect("scalar Int function should have a private native shape");
        let int = NativeLayout::Scalar(NativeScalar::Int);
        assert_eq!(shape.parameters(), std::slice::from_ref(&int));
        assert_eq!(shape.result(), &int);

        let signature = shape.clone().with_effect(NativeEffectAbi::PureNoFault);
        assert_eq!(signature.shape(), &shape);
        assert_eq!(signature.effect(), NativeEffectAbi::PureNoFault);
        assert_eq!(
            shape.with_effect(NativeEffectAbi::RuntimeStatus).effect(),
            NativeEffectAbi::RuntimeStatus
        );
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
        let mut bool_parameter = base.clone();
        bool_parameter.params[0].ty = Type::Bool;
        let mut bool_result = base.clone();
        bool_result.return_ty = Type::Bool;
        let mut record_parameter = base;
        record_parameter.params[0].ty = Type::Nominal(TypeId(0), Vec::new());

        for function in [
            asynchronous,
            generic,
            receiver,
            witnessed,
            bool_parameter,
            bool_result,
            record_parameter,
        ] {
            assert!(
                NativeSignatureShape::for_supported_function(&function).is_none(),
                "unsupported function shape was selected: {}",
                function.name
            );
        }
    }
}
