//! InstCombine as a round of the mid-end pipeline: what it extracts from its
//! own output must leave that output alone.

use tir::{builtin::ModuleOp, func::FuncOp, parse::ir::parse_ir, Context, Operation, PassManager};

/// A counted loop with an early exit, as the frontend and `promote` leave it.
/// Its `do` region forwards the carried values and nothing else: the spelling
/// of a port read there is the port itself, and a literal built for it has no
/// reader to take it.
const EARLY_EXIT_LOOP: &str = r#"
module {
  %63 = func.func @f(%0: !i32) -> !i32 {
    %4 = constant {value = 0} : !i32
    %5 = constant {value = 0} : !i1
    %6 = constant {value = 0} : !i32
    %13 = constant {value = 0} : !i32
    %44, %45, %46, %68 = scf.while iter_args(%14 = %4, %15 = %5, %16 = %6, %64 = %13) -> !i32, !i1, !i32, !i32 {
      %19 = cmpi %64, %0 {predicate = "slt"} : !i1
      %37, %38, %39, %40, %66 = scf.if %19 -> !i32, !i1, !i32, !i1, !i32 {
        %20 = constant {value = 6} : !i32
        %22 = cmpi %64, %20 {predicate = "eq"} : !i1
        %31, %32, %33, %34, %65 = scf.if %22 -> !i32, !i1, !i32, !i1, !i32 {
          %23 = constant {value = 0} : !i1
          %24 = constant {value = 1} : !i32
          %25 = constant {value = 0} : !i1
          %26 = constant {value = 1} : !i32
          scf.yield %24, %23, %26, %25, %64
        }
         else {
          %27 = constant {value = 1} : !i32
          %29 = addi %64, %27 : !i32
          %30 = constant {value = 1} : !i1
          scf.yield %14, %15, %16, %30, %29
        }
        scf.yield %31, %32, %33, %34, %65
      }
       else {
        %35 = constant {value = 0} : !i1
        %36 = constant {value = 0} : !i32
        scf.yield %14, %15, %36, %35, %64
      }
      scf.condition %40, %37, %38, %39, %66
    }
     do(%41, %42, %43, %67) {
      scf.yield %41, %42, %43, %67
    }
    func.return %44
  }
  module_end
}
"#;

#[test]
fn a_round_of_instcombine_reaches_a_fixpoint() {
    let context = Context::with_default_dialects();
    let module = parse_ir::<ModuleOp>(&context, EARLY_EXIT_LOOP).expect("the fixture parses");

    let mut round = PassManager::new();
    round
        .fixpoint(8)
        .nest::<FuncOp>()
        .add_pass(tir::passes::InstCombinePass::new());
    round
        .run(&context, context.get_op(module.id()))
        .expect("the round simplifies");
    let settled = context.op_version(module.id());

    let mut once = PassManager::new();
    once.nest::<FuncOp>()
        .add_pass(tir::passes::InstCombinePass::new());
    once.run(&context, context.get_op(module.id()))
        .expect("a settled function has nothing left to simplify");

    assert_eq!(
        context.op_version(module.id()),
        settled,
        "a fixpoint that ran to its cap left the function still moving"
    );
}
