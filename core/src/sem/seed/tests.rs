use super::{SeedGraph, TermKind, seed};
use crate::builtin::ModuleOp;
use crate::parse::ir::parse_ir;
use crate::sem::SymKind;
use crate::{Context, LoopLike, OpId, Operation};

/// Seed the first function of `src`.
fn seed_first_function(src: &str) -> (Context, OpId, SeedGraph) {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, src).expect("parse module");
    let function = first_function(&context, module.id());
    let graph = seed(&context, function);
    (context, function, graph)
}

/// Seed the first function of `src` and render the term graph.
fn seed_dump(src: &str) -> String {
    let (context, _, graph) = seed_first_function(src);
    graph.dump(&context)
}

fn first_function(context: &Context, module: OpId) -> OpId {
    for region in context.get_op(module).regions.clone() {
        for block in context.get_region(region).iter(context.clone()) {
            for op in block.op_ids() {
                if context.get_op(op).is::<crate::builtin::FuncOp>() {
                    return op;
                }
            }
        }
    }
    panic!("the module declares a function");
}

#[test]
fn straight_line_operations_seed_their_semantic_terms() {
    let dump = seed_dump(
        "module {
  func @f(%0: !i32, %1: !i32) -> !i32 {
    %2 = addi %0, %1 : !i32
    %3 = constant {value = 7} : !i32
    %4 = muli %2, %3 : !i32
    return %4
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i32
t1 = anchor %1 : !i32
t2 = Add t0 t1 : !i32
t3 = const 7:i32
t4 = Mul t2 t3 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t2
  %3 -> t3
  %4 -> t4
"
    );
}

#[test]
fn a_conditional_seeds_one_choice_over_its_arms() {
    let dump = seed_dump(
        "module {
  func @f(%0: !i1, %1: !i32, %2: !i32) -> !i32 {
    %3 = scf.if %0 args(%1) -> !i32 (%4) {
      %5 = addi %4, %2 : !i32
      scf.yield %5
    } else (%6) {
      scf.yield %6
    }
    return %3
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i1
t1 = anchor %1 : !i32
t2 = anchor %2 : !i32
t3 = Add t1 t2 : !i32
t4 = If t0 t3 t1 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t2
  %3 -> t1
  %4 -> t3
  %5 -> t1
  %6 -> t4
"
    );
}

#[test]
fn a_switch_seeds_a_chain_of_case_comparisons() {
    let dump = seed_dump(
        "module {
  func @f(%0: !i32, %1: !i32) -> !i32 {
    %2 = scf.switch %0 -> !i32 case 1 {
      scf.yield %1
    }
    case 7 {
      scf.yield %0
    }
    default {
      scf.yield %1
    }
    return %2
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i32
t1 = anchor %1 : !i32
t2 = const 7:i32
t3 = Eq t0 t2
t4 = If t3 t0 t1 : !i32
t5 = const 1:i32
t6 = Eq t0 t5
t7 = If t6 t1 t4 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t7
"
    );
}

/// The first operation of `function` reading as a loop.
fn first_loop(context: &Context, function: OpId) -> Box<dyn LoopLike> {
    for region in context.get_op(function).regions.clone() {
        for block in context.get_region(region).iter(context.clone()) {
            for op in block.op_ids() {
                if let Some(loop_like) = context.get_op(op).as_interface::<dyn LoopLike>() {
                    return loop_like;
                }
            }
        }
    }
    panic!("the function holds a loop");
}

#[test]
fn a_loop_seeds_its_body_argument_apart_from_its_result() {
    let source = "module {
  func @f(%0: !index, %1: !index, %2: !index, %3: !i32) -> !i32 {
    %4 = scf.for %0, %1, %2 iter_args(%5 = %3) -> !i32 {
      %6 = addi %5, %5 : !i32
      scf.yield %6
    }
    return %4
  }
  module_end
}";
    let (context, function, graph) = seed_first_function(source);

    let loop_like = first_loop(&context, function);
    let carried = graph
        .term_of(loop_like.carried_args()[0])
        .expect("the body argument is seeded");
    let final_value = graph
        .term_of(loop_like.finals()[0])
        .expect("the loop result is seeded");
    assert_ne!(carried, final_value);
    assert_eq!(graph.kind(carried), TermKind::Op(SymKind::Theta));
    assert_ne!(graph.kind(final_value), TermKind::Op(SymKind::Theta));

    assert_eq!(
        graph.dump(&context),
        "t0 = anchor %0 : !index
t1 = anchor %1 : !index
t2 = anchor %2 : !index
t3 = anchor %3 : !i32
t4 = Lt t0 t1
t5 = Theta t3 t6 : !i32
t6 = Add t5 t5 : !i32
t7 = loop_exit t5 : !i32
t8 = If t4 t7 t3 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t2
  %3 -> t3
  %4 -> t5
  %5 -> t6
  %6 -> t8
"
    );
}

#[test]
fn a_while_body_argument_reads_the_value_its_condition_forwards() {
    let dump = seed_dump(
        "module {
  func @f(%0: !i1, %1: !i32) -> !i32 {
    %2 = scf.while iter_args(%3 = %1) -> !i32 {
      %4 = addi %3, %3 : !i32
      scf.condition %0, %4
    } do(%5) {
      %6 = muli %5, %5 : !i32
      scf.yield %6
    }
    return %2
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i1
t1 = anchor %1 : !i32
t2 = Add t1 t1 : !i32
t3 = Theta t1 t5 : !i32
t4 = Add t3 t3 : !i32
t5 = Mul t4 t4 : !i32
t6 = loop_exit t4 : !i32
t7 = If t0 t6 t2 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t3
  %3 -> t4
  %4 -> t4
  %5 -> t5
  %6 -> t7
"
    );
}

#[test]
fn state_operands_thread_memory_terms_like_any_other_edge() {
    let dump = seed_dump(
        "module {
  func @f(%0: !i32) -> !i32 {
    %1 = entry_state : !state
    %2 = ptr.alloca {size = 4, align = 4} : !ptr.p<!i32> state(-> %3)
    ptr.store %0, %2 state(%3 -> %4)
    %5 = ptr.load %2 : !i32 state(%4 -> %6)
    return %5 state(%1)
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i32
t1 = opaque #0
t2 = opaque #1
t3 = project 0 t2 : !ptr.p<!i32>
t4 = project 1 t2 : !state
t5 = const 4:i3
t6 = const 0:i1
t7 = StoreMemory t3 t5 t0 t6 t4
t8 = LoadMemory t3 t5 t6 t7 : !i32
t9 = project 1 t8 : !state
values:
  %0 -> t0
  %1 -> t1
  %2 -> t3
  %3 -> t4
  %4 -> t7
  %5 -> t8
  %6 -> t9
"
    );
}

#[test]
fn a_block_argument_anchors_instead_of_merging_its_predecessors() {
    let dump = seed_dump(
        "module {
  func @f(%c: !i1, %x: !i32) -> !i32 {
    %0 = addi %x, %x : !i32
    cond_br %c, ^bb1(%0 : !i32), ^bb2
  ^bb1(%a: !i32):
    %1 = addi %a, %0 : !i32
    return %1
  ^bb2:
    return %0
  }
  module_end
}",
    );

    assert_eq!(
        dump,
        "t0 = anchor %0 : !i1
t1 = anchor %1 : !i32
t2 = Add t1 t1 : !i32
t3 = anchor %3 : !i32
t4 = Add t2 t3 : !i32
values:
  %0 -> t0
  %1 -> t1
  %2 -> t2
  %3 -> t3
  %4 -> t4
"
    );
}

#[test]
fn seeding_one_program_twice_builds_the_same_graph() {
    let source = "module {
  func @f(%0: !i1, %1: !i32, %2: !index) -> !i32 {
    %3 = scf.for %2, %2, %2 iter_args(%4 = %1) -> !i32 {
      %5 = scf.if %0 args(%4) -> !i32 (%6) {
        %7 = addi %6, %1 : !i32
        scf.yield %7
      } else (%8) {
        scf.yield %8
      }
      scf.yield %5
    }
    return %3
  }
  module_end
}";
    let (context, function, graph) = seed_first_function(source);

    let again = seed(&context, function);

    assert_eq!(graph.dump(&context), again.dump(&context));
    assert_eq!(graph.dump(&context), seed_dump(source));
}
