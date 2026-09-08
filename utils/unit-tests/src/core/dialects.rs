//! Dialect types that have no textual form of their own to check: what a
//! `ptr.p` remembers about its pointee, and how deeply it interns.

use tir::{builtin::IntegerType, ptr::PtrType, Context};

#[test]
fn opaque_and_typed_pointers_are_distinct_and_interned() {
    let context = Context::with_default_dialects();

    let opaque = PtrType::opaque(&context);
    let i32_ty = IntegerType::new(&context, 32);
    let typed = PtrType::typed(&context, i32_ty);

    // Typed pointer remembers its pointee.
    let data = context.get_type_data(typed);
    let ptr = (data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<PtrType>()
        .unwrap();
    assert_eq!(ptr.pointee(&context), Some(i32_ty));

    // An opaque pointer carries no pointee.
    let opaque_data = context.get_type_data(opaque);
    let opaque_ptr = (opaque_data.as_ref() as &dyn std::any::Any)
        .downcast_ref::<PtrType>()
        .unwrap();
    assert_eq!(opaque_ptr.pointee(&context), None);

    // Typed and opaque pointers are distinct, identical ones are interned.
    assert_ne!(opaque, typed);
    assert_eq!(PtrType::typed(&context, i32_ty), typed);
}

#[test]
fn deeply_nested_pointers_are_interned() {
    let context = Context::with_default_dialects();
    let build = |depth| {
        let mut ty = IntegerType::new(&context, 32);
        for _ in 0..depth {
            ty = PtrType::typed(&context, ty);
        }
        ty
    };

    assert_eq!(build(10_000), build(10_000));
}
