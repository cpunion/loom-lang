use loom_codegen_ir::{Repr, RepresentationPlan, ScalarRepr, TargetLayout};
use loom_mir::Type;

#[test]
fn scalar_representation_catalog_is_canonical() {
    let target = TargetLayout::new(64).expect("64-bit pointers are valid");
    let first = RepresentationPlan::scalar(target);
    let second = RepresentationPlan::scalar(target);

    assert_ne!(first, second, "plans from distinct programs are branded");
    assert_eq!(first.target().pointer_bits(), 64);
    assert_eq!(
        first.reprs(),
        [
            Repr::Uninhabited,
            Repr::Zst,
            Repr::Scalar(ScalarRepr::I1),
            Repr::Scalar(ScalarRepr::I64),
            Repr::Scalar(ScalarRepr::F64),
        ]
    );

    let expected = [
        (Type::Never, Repr::Uninhabited),
        (Type::Unit, Repr::Zst),
        (Type::Bool, Repr::Scalar(ScalarRepr::I1)),
        (Type::Int, Repr::Scalar(ScalarRepr::I64)),
        (Type::Float, Repr::Scalar(ScalarRepr::F64)),
    ];
    for (semantic, repr) in expected {
        let ty = first
            .type_id(&semantic)
            .expect("foundation scalar has a canonical type");
        let planned = first.value_type(ty).expect("planned type exists");
        assert_eq!(planned.semantic(), &semantic);
        assert_eq!(first.repr(planned.repr()), Some(&repr));
        assert_eq!(first.type_id(&semantic), Some(ty));

        let other_ty = second
            .type_id(&semantic)
            .expect("second foundation scalar exists");
        assert_ne!(ty, other_ty);
        assert_eq!(ty.raw(), other_ty.raw());
        let other_planned = second.value_type(other_ty).expect("second type exists");
        assert_ne!(planned.repr(), other_planned.repr());
        assert_eq!(planned.repr().raw(), other_planned.repr().raw());
        assert_eq!(first.value_type(other_ty), None);
        assert_eq!(first.repr(other_planned.repr()), None);
    }

    assert_eq!(first.type_id(&Type::Text), None);
    assert_eq!(first.type_id(&Type::List(Box::new(Type::Int))), None);
}

#[test]
fn target_pointer_width_is_validated_at_the_boundary() {
    assert!(TargetLayout::new(32).is_ok());
    assert!(TargetLayout::new(64).is_ok());
    assert!(TargetLayout::new(0).is_err());
    assert!(TargetLayout::new(7).is_err());
    assert!(TargetLayout::new(136).is_err());
}
