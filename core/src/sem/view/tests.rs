use super::View;
use crate::builtin::ModuleOp;
use crate::parse::ir::parse_ir;
use crate::sem::SymKind;
use crate::sem::seed::{SeedGraph, TermKind, TermPayload, seed};
use crate::{Context, OpId, Operation};

fn view_of(src: &str) -> (Context, View) {
    let (context, graph) = seed_of(src);
    (context, View::new(graph))
}

fn seed_of(src: &str) -> (Context, SeedGraph) {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, src).expect("parse module");
    let function = first_function(&context, module.id());
    let graph = seed(&context, function);
    (context, graph)
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

const TWO_ADDS: &str = "module {
  func @f(%0: !i32, %1: !i32) -> !i32 {
    %2 = addi %0, %0 : !i32
    %3 = addi %1, %1 : !i32
    %4 = addi %2, %3 : !i32
    return %4
  }
  module_end
}";

#[test]
fn congruence_merges_terms_whose_operands_became_equal() {
    let (_, mut view) = view_of(TWO_ADDS);

    let terms = view.graph();
    let (left, right) = (terms.term_at(0), terms.term_at(1));
    view.union(left, right);
    view.saturate();

    assert_eq!(
        view.dump(),
        "c0: t0 t1
c2: t2 t3
c4: t4
"
    );
}

#[test]
fn a_reader_of_a_merged_class_is_found_through_the_class_it_joined() {
    let (_, mut view) = view_of(TWO_ADDS);

    let (left, right) = (view.graph().term_at(0), view.graph().term_at(1));
    let (left_add, right_add) = (view.graph().term_at(2), view.graph().term_at(3));
    view.union(left, right);

    assert_eq!(view.readers(left), vec![left_add, right_add]);
}

#[test]
fn a_term_past_the_node_limit_is_not_added() {
    let (_, graph) = seed_of(TWO_ADDS);
    let limit = graph.len();
    let mut view = View::with_node_limit(graph, limit);

    let operand = view.graph().term_at(0);
    let added = view.add(
        TermKind::Op(SymKind::Not),
        TermPayload::None,
        None,
        &[operand],
    );

    assert_eq!(added, None);
    assert_eq!(view.graph().len(), limit);
}

#[test]
fn saturating_twice_reads_the_same_classes() {
    let (_, mut view) = view_of(TWO_ADDS);
    view.union(view.graph().term_at(0), view.graph().term_at(1));

    view.saturate();
    let once = view.dump();
    view.saturate();

    assert_eq!(view.dump(), once);
}

#[test]
fn a_loop_carried_cycle_saturates() {
    let (_, mut view) = view_of(
        "module {
  func @f(%0: !index, %1: !index, %2: !index, %3: !i32) -> !i32 {
    %4 = scf.for %0, %1, %2 iter_args(%5 = %3) -> !i32 {
      %6 = addi %5, %5 : !i32
      scf.yield %6
    }
    return %4
  }
  module_end
}",
    );

    view.saturate();

    assert_eq!(view.classes().len(), view.graph().len());
}
