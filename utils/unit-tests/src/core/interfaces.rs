//! The generic views spec-05 gives functions, calls, globals and leaf ops:
//! what a consumer reads without knowing the concrete op.

use tir::{
    builtin::ModuleOp, parse::ir::parse_ir, Apply, Callable, Context, Global, OpId, Operation,
    Speculatable,
};

const MODULE: &str = r#"module {
  %counter = global @counter align 4 bytes [1, 0, 0, 0]
  %fn_puts = func.declare @puts(!i32) -> !i32
  %fn_main = func.func @main(%0: !i32) -> !i32 {
    %1 = func.call %fn_puts(%0 : !i32) -> !i32
    %2 = addi %1, %0 : !i32
    %3 = divsi %2, %0 : !i32
    func.return %3
  }
  module_end
}"#;

fn module_ops(context: &Context, module: OpId) -> Vec<OpId> {
    context
        .get_region(context.get_op(module).regions()[0])
        .iter(context.clone())
        .next()
        .expect("module body")
        .op_ids()
}

#[test]
fn a_function_and_a_declaration_are_callable() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, MODULE).expect("parse");
    let ops = module_ops(&context, module.id());
    let i32_ty = tir::builtin::IntegerType::new(&context, 32);

    let declare = context
        .get_op(ops[1])
        .as_interface::<dyn Callable>()
        .expect("func.declare is callable");
    assert!(declare.body().is_none());
    assert_eq!(declare.params(), vec![i32_ty]);
    assert_eq!(declare.result(), i32_ty);
    assert_eq!(declare.value(), context.get_op(ops[1]).results()[0]);

    let func = context
        .get_op(ops[2])
        .as_interface::<dyn Callable>()
        .expect("func.func is callable");
    assert_eq!(func.body(), Some(context.get_op(ops[2]).regions()[0]));
    assert_eq!(func.params(), vec![i32_ty]);
}

#[test]
fn a_call_applies_its_callee_to_a_range_of_operands() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, MODULE).expect("parse");
    let ops = module_ops(&context, module.id());
    let body = context.get_op(ops[2]).regions()[0];
    let call = context.get_region(body).op_ids()[0];

    let apply = context
        .get_op(call)
        .as_interface::<dyn Apply>()
        .expect("func.call applies");
    assert_eq!(apply.callee(), context.get_op(ops[1]).results()[0]);
    assert_eq!(apply.args(), 1..2);
}

#[test]
fn a_global_publishes_its_address_and_initializer() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, MODULE).expect("parse");
    let ops = module_ops(&context, module.id());

    let global = context
        .get_op(ops[0])
        .as_interface::<dyn Global>()
        .expect("global is a data object");
    assert_eq!(global.address(), context.get_op(ops[0]).results()[0]);
    assert_eq!(global.initializer(), Some(vec![1, 0, 0, 0]));
}

#[test]
fn arithmetic_is_speculatable_and_division_is_not() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, MODULE).expect("parse");
    let ops = module_ops(&context, module.id());
    let body = context
        .get_region(context.get_op(ops[2]).regions()[0])
        .op_ids();

    assert!(context.get_op(body[1]).has_interface::<dyn Speculatable>());
    assert!(!context.get_op(body[2]).has_interface::<dyn Speculatable>());
    assert!(!context.get_op(body[0]).has_interface::<dyn Speculatable>());
}

#[test]
fn a_zero_filled_global_has_a_zero_image() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(
        &context,
        "module {\n  %s = global private @s size 3 align 1\n  module_end\n}",
    )
    .expect("parse");
    let ops = module_ops(&context, module.id());

    let global = context
        .get_op(ops[0])
        .as_interface::<dyn Global>()
        .expect("global is a data object");
    assert_eq!(global.initializer(), Some(vec![0, 0, 0]));
}
