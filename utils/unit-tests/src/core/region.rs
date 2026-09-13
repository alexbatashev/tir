//! Unordered regions built through the context rather than parsed: what a
//! producer of the form will do once one exists.

use tir::{
    builtin::{self, ops},
    func,
    interp::{self, Value},
    passes::destructure::{destructure, CfgEdges, OperationGroup},
    scf, Context, OpId, Operation,
};

use super::fixtures;

/// `module { %f = func.func @quad(%p: !i32) { %a = addi %p, %p; %b = addi %a, %a; -> %b } }`
fn quad(context: &Context) -> (OpId, OpId) {
    let i32_ty = builtin::IntegerType::new(context, 32);
    let port = context.create_value(i32_ty, None);
    let double = ops::addi(context, port.id(), port.id(), i32_ty).build();
    let quad = ops::addi(context, double.result(), double.result(), i32_ty).build();
    let body = context.create_nodes_region(
        vec![port],
        vec![double.id(), quad.id()],
        vec![quad.result()],
    );
    let function = func::ops::lambda(context, "quad", i32_ty, &body).build();
    let module = builtin::ops::module(context, None).build();
    module.body().append(function.id());
    module
        .body()
        .append_op(builtin::ModuleEndOpBuilder::new(context).build());
    (module.id(), function.id())
}

#[test]
fn an_unordered_function_body_verifies_and_runs() {
    let context = Context::with_default_dialects();
    let (module, function) = quad(&context);

    tir::verify_op_tree(&context, module).expect("an unordered body verifies");

    let results = interp::run_function(
        &context,
        function,
        vec![Value::Int(tir::utils::APInt::new(32, 5))],
    )
    .expect("an unordered body runs");
    assert_eq!(results[0].to_i64(), Some(20));
}

#[test]
fn copying_an_unordered_region_copies_its_ports_and_operations() {
    let context = Context::with_default_dialects();
    let (_, function) = quad(&context);
    let body = context.get_op(function).regions()[0];

    let copy = context.get_region(tir::clone_region_with_mapping(
        &context,
        body,
        &Default::default(),
    ));

    assert!(copy.is_nodes());
    assert_eq!(copy.op_ids().len(), 2);
    let original = context.get_region(body);
    assert_ne!(copy.ports()[0].id(), original.ports()[0].id());
    assert_ne!(copy.results()[0], original.results()[0]);
}

#[test]
fn erasing_the_owner_reclaims_an_unordered_region() {
    let context = Context::with_default_dialects();
    let (_, function) = quad(&context);
    let body = context.get_region(context.get_op(function).regions()[0]);
    let held = body.op_ids();
    let port = body.ports()[0].id();

    context
        .erase_op(&tir::OperationRef::new(context.get_op(function)))
        .expect("the function leaves the module");

    for op in held {
        assert!(
            !context.has_operation(op),
            "the region's operations go with it"
        );
    }
    assert!(!context.has_value(port), "so do the region's ports");
    assert!(!context.is_region_port(port), "and their def-site index");
}

#[test]
fn destruction_keeps_an_ordered_operation_group_with_its_middle_result() {
    let context = Context::with_default_dialects();
    let i32_ty = builtin::IntegerType::new(&context, 32);
    let port = context.create_value(i32_ty, None);
    let constant = ops::constant(&context, 3, i32_ty).build();
    let first = ops::addi(&context, port.id(), port.id(), i32_ty).build();
    let middle = ops::addi(&context, first.result(), constant.result(), i32_ty).build();
    let last = ops::addi(&context, middle.result(), port.id(), i32_ty).build();
    let body = context.create_nodes_region(
        vec![port],
        vec![constant.id(), first.id(), middle.id(), last.id()],
        vec![middle.result()],
    );
    let function = func::ops::lambda(&context, "grouped", i32_ty, &body).build();
    let module = builtin::ops::module(&context, None).build();
    module.body().append(function.id());
    module
        .body()
        .append_op(builtin::ModuleEndOpBuilder::new(&context).build());

    destructure(
        &context,
        body.id(),
        &CfgEdges { context: &context },
        &[OperationGroup {
            members: vec![first.id(), middle.id(), last.id()],
        }],
    )
    .expect("the grouped body destructures");

    let block = context
        .parent_block(middle.id())
        .expect("middle is retained");
    assert_eq!(context.parent_block(first.id()), Some(block));
    assert_eq!(context.parent_block(last.id()), Some(block));
    let order = context.get_block(block).op_ids();
    let constant_position = order
        .iter()
        .position(|candidate| *candidate == constant.id())
        .expect("external constant is retained");
    let positions = [first.id(), middle.id(), last.id()].map(|member| {
        order
            .iter()
            .position(|candidate| *candidate == member)
            .expect("group member is retained")
    });
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    assert!(constant_position < positions[1]);
    assert_eq!(
        context.get_value(middle.result()).defining_op(),
        Some(middle.id())
    );

    let result = interp::run_function(
        &context,
        function.id(),
        vec![Value::Int(tir::utils::APInt::new(32, 5))],
    )
    .expect("the destructured function runs");
    assert_eq!(result[0].to_i64(), Some(13));
}

#[test]
fn destruction_keeps_an_operation_group_in_the_loop_exit_cone() {
    let (context, _, function, body) = fixtures::parse_function(
        r#"module {
  %fn_main = func.func @main(%0: !i32) -> !i32 {
    %1 = constant {value = 0} : !i1
    %2 = scf.loop (%3 = %0) {
      -> %1, %3, %3
    }
    -> %2
  }
  module_end
}"#,
    );
    let loop_op = context
        .get_region(body)
        .op_ids()
        .into_iter()
        .find(|&op| context.get_op(op).is::<scf::LoopOp>())
        .expect("the fixture has a loop");
    let loop_body = context.get_op(loop_op).regions()[0];
    let region = context.get_region(loop_body);
    let predicate = region.results()[0];
    let port = region.ports()[0].id();
    let i32_ty = builtin::IntegerType::new(&context, 32);
    let constant = ops::constant(&context, 3, i32_ty).build();
    let first = ops::addi(&context, port, port, i32_ty).build();
    let middle = ops::addi(&context, first.result(), constant.result(), i32_ty).build();
    let last = ops::addi(&context, middle.result(), port, i32_ty).build();
    for op in [constant.id(), first.id(), middle.id(), last.id()] {
        context.add(loop_body, op);
    }
    context.set_region_results(loop_body, vec![predicate, port, middle.result()]);

    let structure = destructure(
        &context,
        body,
        &CfgEdges { context: &context },
        &[OperationGroup {
            members: vec![first.id(), middle.id(), last.id()],
        }],
    )
    .expect("the loop destructures");

    let loop_blocks = structure.loops[0];
    let exit = context
        .parent_block(middle.id())
        .expect("the middle member is retained");
    assert_eq!(context.parent_block(first.id()), Some(exit));
    assert_eq!(context.parent_block(last.id()), Some(exit));
    assert_ne!(exit, loop_blocks.header);
    assert_ne!(exit, loop_blocks.continue_);
    let order = context.get_block(exit).op_ids();
    let positions = [first.id(), middle.id(), last.id()].map(|member| {
        order
            .iter()
            .position(|candidate| *candidate == member)
            .expect("group member is retained")
    });
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    assert_eq!(
        context.get_value(middle.result()).defining_op(),
        Some(middle.id())
    );

    let result = interp::run_function(
        &context,
        function.id(),
        vec![Value::Int(tir::utils::APInt::new(32, 5))],
    )
    .expect("the destructured function runs");
    assert_eq!(result[0].to_i64(), Some(13));
}
