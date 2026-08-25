//! Compiler-private native value layouts which the production emitter can lower today.
//!
//! This module is intentionally narrower than the set of layouts which could theoretically be
//! native. Returning a signature from [`NativeSignatureShape::for_supported_function`] is a
//! promise shared by runtime-requirement analysis and LLVM emission: the call does not need the
//! universal `Value` ABI. New layouts must therefore be added here only in the same change which
//! teaches every consumer how to lower them.

use loom_mir::{Function, Type};

/// A scalar with a compiler-private, unboxed native representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeScalar {
    Int,
}

/// The native representation of one source-language value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeLayout {
    Scalar(NativeScalar),
}

/// Parameter and result layouts for a function supported by the private native ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeSignatureShape {
    parameters: Vec<NativeLayout>,
    result: NativeLayout,
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
            parameters: vec![int; function.params.len()],
            result: int,
        })
    }

    #[must_use]
    pub(crate) fn parameters(&self) -> &[NativeLayout] {
        &self.parameters
    }

    #[must_use]
    pub(crate) const fn result(&self) -> NativeLayout {
        self.result
    }
}

#[cfg(test)]
#[allow(clippy::default_trait_access)]
mod tests {
    use std::collections::BTreeMap;

    use loom_mir::{
        Block, CallPlan, ConceptId, Function, FunctionId, LocalDecl, LocalId, Receiver, Type,
        WitnessParam,
    };

    use super::{NativeLayout, NativeScalar, NativeSignatureShape};

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

    #[test]
    fn selects_the_production_scalar_int_shape() {
        let shape = NativeSignatureShape::for_supported_function(&scalar_int_function())
            .expect("scalar Int function should have a private native shape");
        let int = NativeLayout::Scalar(NativeScalar::Int);
        assert_eq!(shape.parameters(), &[int]);
        assert_eq!(shape.result(), int);
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
        let mut bool_result = base;
        bool_result.return_ty = Type::Bool;

        for function in [
            asynchronous,
            generic,
            receiver,
            witnessed,
            bool_parameter,
            bool_result,
        ] {
            assert!(
                NativeSignatureShape::for_supported_function(&function).is_none(),
                "unsupported function shape was selected: {}",
                function.name
            );
        }
    }
}
