use loom_codegen_ir::{
    BYTES_TYPE_ID, BuildErrorCode, Effects, Origin, ProgramBuilder, Repr, RepresentationPlan,
    ScalarRepr, Signature, TargetLayout,
};
use loom_mir::{FunctionId, Type, TypeId};

#[test]
fn direct_representation_catalog_is_canonical() {
    let target = TargetLayout::new(64).expect("64-bit pointers are valid");
    let first = RepresentationPlan::direct(target);
    let second = RepresentationPlan::direct(target);

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

#[test]
fn managed_lists_are_distinct_direct_pointers_and_64_bit_only() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let integers = Type::List(Box::new(Type::Int));
    let nested = Type::List(Box::new(integers.clone()));
    let integer_list = builder
        .add_managed_list_type(integers.clone())
        .expect("List[Int]");
    let nested_list = builder
        .add_managed_list_type(nested.clone())
        .expect("List[List[Int]]");
    assert_ne!(integer_list, nested_list);
    for (semantic, ty) in [(integers, integer_list), (nested, nested_list)] {
        let value = builder
            .representations()
            .value_type(ty)
            .expect("List value type");
        assert_eq!(value.semantic(), &semantic);
        assert_eq!(
            builder.representations().repr(value.repr()),
            Some(&Repr::ManagedPointer)
        );
    }
    assert_eq!(
        builder
            .add_managed_list_type(Type::List(Box::new(Type::Int)))
            .expect_err("duplicate List registration")
            .code(),
        BuildErrorCode::InvalidListType
    );

    let mut narrow = ProgramBuilder::new(TargetLayout::new(32).expect("target"));
    assert_eq!(
        narrow
            .add_managed_list_type(Type::List(Box::new(Type::Int)))
            .expect_err("32-bit List must fail closed")
            .code(),
        BuildErrorCode::InvalidListType
    );
}

#[test]
fn managed_bytes_registration_is_exact_canonical_and_64_bit_only() {
    let semantic = Type::Nominal(BYTES_TYPE_ID, Vec::new());
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let bytes = builder
        .add_managed_bytes_type(semantic.clone())
        .expect("canonical Bytes");
    let value_type = builder
        .representations()
        .value_type(bytes)
        .expect("Bytes value type");
    assert_eq!(value_type.semantic(), &semantic);
    assert_eq!(
        builder.representations().repr(value_type.repr()),
        Some(&Repr::ManagedPointer)
    );
    assert!(builder.representations().is_managed_bytes_type(bytes));
    builder
        .add_tuple_type(std::slice::from_ref(&semantic))
        .expect("Bytes is one managed-pointer aggregate leaf");
    assert_eq!(
        builder
            .add_managed_bytes_type(semantic.clone())
            .expect_err("duplicate Bytes")
            .code(),
        BuildErrorCode::InvalidBytesType
    );

    let mut wrong = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    assert_eq!(
        wrong
            .add_managed_bytes_type(Type::Nominal(TypeId(12), Vec::new()))
            .expect_err("noncanonical nominal")
            .code(),
        BuildErrorCode::InvalidBytesType
    );
    assert_eq!(
        wrong
            .add_managed_bytes_type(Type::Nominal(BYTES_TYPE_ID, vec![Type::Int]))
            .expect_err("open Bytes")
            .code(),
        BuildErrorCode::InvalidBytesType
    );

    let mut narrow = ProgramBuilder::new(TargetLayout::new(32).expect("target"));
    assert_eq!(
        narrow
            .add_managed_bytes_type(semantic)
            .expect_err("32-bit Bytes")
            .code(),
        BuildErrorCode::InvalidBytesType
    );
}

#[test]
fn immortal_text_is_explicit_64_bit_only_and_not_a_product_leaf() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    assert_eq!(builder.type_id(&Type::Text), None);
    let text = builder
        .add_immortal_text_type()
        .expect("register immortal Text");
    let planned = builder
        .representations()
        .value_type(text)
        .expect("Text value type");
    assert_eq!(planned.semantic(), &Type::Text);
    assert_eq!(
        builder.representations().repr(planned.repr()),
        Some(&Repr::ImmortalText)
    );
    assert_eq!(
        builder
            .add_immortal_text_type()
            .expect_err("duplicate Text registration must fail")
            .code(),
        BuildErrorCode::InvalidTextType
    );
    assert_eq!(
        builder
            .add_tuple_type(&[Type::Text])
            .expect_err("immortal-only Text cannot enter a direct product")
            .code(),
        BuildErrorCode::InvalidProductType
    );

    let mut narrow = ProgramBuilder::new(TargetLayout::new(32).expect("target"));
    assert_eq!(
        narrow
            .add_immortal_text_type()
            .expect_err("the native Text object ABI is 64-bit only")
            .code(),
        BuildErrorCode::InvalidTextType
    );
}

#[test]
fn managed_text_enters_nested_products_and_sums_but_not_transparent_carriers() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    builder
        .add_managed_text_type()
        .expect("register managed-capable Text");
    let inner = Type::Tuple(vec![Type::Text, Type::Int]);
    builder
        .add_tuple_type(&[Type::Text, Type::Int])
        .expect("managed Text tuple");
    let record = Type::Nominal(TypeId(90), Vec::new());
    let outer = builder
        .add_pod_record_type(record.clone(), std::slice::from_ref(&inner))
        .expect("nested managed product");
    assert!(matches!(
        builder
            .representations()
            .value_type(outer)
            .and_then(|ty| builder.representations().repr(ty.repr())),
        Some(Repr::Product(_))
    ));

    let sum = builder
        .add_sum_type(
            Type::Nominal(TypeId(91), Vec::new()),
            &[Box::from([record.clone()])],
        )
        .expect("closed sum may carry exact managed leaves");
    assert!(matches!(
        builder
            .representations()
            .value_type(sum)
            .and_then(|ty| builder.representations().repr(ty.repr())),
        Some(Repr::Sum(_))
    ));
    assert_eq!(
        builder
            .add_transparent_type(Type::Nominal(TypeId(92), Vec::new()), &record)
            .expect_err("transparent managed-product carriers remain unsupported")
            .code(),
        BuildErrorCode::InvalidValueType
    );
}

#[test]
fn tuple_builder_requires_child_first_unique_predeclaration() {
    let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
    let inner = Type::Tuple(vec![Type::Int, Type::Bool]);
    let unregistered = builder
        .add_tuple_type(std::slice::from_ref(&inner))
        .expect_err("a tuple child must be registered first");
    assert_eq!(unregistered.code(), BuildErrorCode::InvalidProductType);

    builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect("register inner tuple");
    let duplicate = builder
        .add_tuple_type(&[Type::Int, Type::Bool])
        .expect_err("structurally equal tuples have one canonical registration");
    assert_eq!(duplicate.code(), BuildErrorCode::InvalidProductType);

    let unit = builder.type_id(&Type::Unit).expect("Unit");
    builder
        .declare_function(
            Origin::synthetic(FunctionId(0)),
            "declared",
            Signature::new(Vec::new(), unit),
            Effects::NONE,
        )
        .expect("declare function");
    let late = builder
        .add_tuple_type(&[inner, Type::Float])
        .expect_err("representations are fixed before function signatures");
    assert_eq!(late.code(), BuildErrorCode::InvalidProductType);
}
