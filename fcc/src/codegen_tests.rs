#[cfg(test)]
mod tests {
    use crate::codegen::codegen;
    use crate::diagnostics::{Span, intern_file};
    use crate::lexer::Token;
    use crate::parser::parse;
    use crate::sema::analyze;
    use logos::Logos;
    use tir::attributes::AttributeValue;

    fn fcc_context() -> tir::Context {
        let context = tir::Context::with_default_dialects();
        context.register_dialect::<crate::cir::CirDialect>();
        context
    }

    fn lower(src: &str) -> (tir::Context, tir::builtin::ModuleOp) {
        let file = intern_file("<test>", src);
        let tokens: Vec<_> = Token::lexer(src)
            .spanned()
            .map(|(r, span)| (r.unwrap(), Span::new(file, span.start)))
            .collect();
        let options = Default::default();
        let unit = parse(&tokens, options).expect("parse");
        let unit = analyze(unit, options).expect("sema");
        let context = fcc_context();
        let module = codegen(&context, &unit).expect("codegen");
        (context, module)
    }

    fn codegen_error(src: &str) -> String {
        let file = intern_file("<test>", src);
        let tokens: Vec<_> = Token::lexer(src)
            .spanned()
            .map(|(r, span)| (r.unwrap(), Span::new(file, span.start)))
            .collect();
        let options = Default::default();
        let unit = parse(&tokens, options).expect("parse");
        let unit = analyze(unit, options).expect("sema");
        let context = fcc_context();
        let diagnostic = codegen(&context, &unit).err().expect("codegen rejects");
        let mut output = Vec::new();
        diagnostic.write(&mut output, false).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn compile(src: &str) -> String {
        let (_context, module) = lower(src);
        let mut out = String::new();
        let mut fmt = tir::IRFormatter::new(&mut out);
        tir::Operation::print(&module, &mut fmt).expect("print");
        out
    }

    fn compile_structs_lowered(src: &str) -> String {
        let (context, module) = lower(src);
        let mut passes = tir::PassManager::new();
        passes.add_pass(crate::passes::LowerCirStructsPass::new());
        passes
            .run(&context, context.get_op(tir::Operation::id(&module)))
            .expect("lower CIR structs");
        let mut out = String::new();
        let mut formatter = tir::IRFormatter::new(&mut out);
        tir::Operation::print(&module, &mut formatter).unwrap();
        out
    }

    #[test]
    fn prototype_of_defined_function_is_not_emitted_twice() {
        // A prototype and the definition it announces are the same symbol, so
        // only the definition may reach the module's symbol table.
        let (context, module) = lower("int f(int); int f(int x) { return x; }");
        tir::verify_op_tree(&context, tir::Operation::id(&module)).expect("module verifies");
    }

    #[test]
    fn tentative_global_definition_defines_one_symbol() {
        // A tentative definition and the definition that completes it name the
        // same object.
        let (context, module) = lower("int x; int x = 5;");
        tir::verify_op_tree(&context, tir::Operation::id(&module)).expect("module verifies");
    }

    #[test]
    fn global_and_function_cannot_share_a_name() {
        // A data symbol owns its name outright, so no overload may hide behind
        // it. C rejects this, so the conflict is built directly.
        let context = fcc_context();
        let module = tir::builtin::ops::module(&context, None).build();
        let mut builder = tir::IRBuilder::new(module.body());
        builder.insert(
            crate::cir::ZeroGlobalOpBuilder::new(&context)
                .attr("sym_name", AttributeValue::Str("x".to_string()))
                .attr("size", AttributeValue::UInt(4))
                .attr("align", AttributeValue::UInt(4))
                .build(),
        );
        let region = context.create_region();
        region.add_block(context.create_block(vec![]).id());
        let func = tir::builtin::ops::func(
            &context,
            "x",
            tir::builtin::UnitType::new(&context),
            Some(region.id()),
        )
        .build();
        tir::IRBuilder::new(func.body())
            .insert(tir::builtin::ops::r#return(&context, tir::Operand::none()).build());
        builder.insert(func);
        builder.insert(tir::builtin::ops::module_end(&context).build());

        tir::verify_op_tree(&context, tir::Operation::id(&module))
            .expect_err("a data symbol and a function cannot share a name");
    }

    /// Codegen behaviour is checked by the LIT tests under `fcc/checks/Codegen`.
    /// This Rust test covers the round-trip invariant, which is a property of
    /// the emitted IR rather than a textual match and so does not fit a
    /// FileCheck test.
    #[test]
    fn ir_roundtrips_through_parser() {
        // The emitted IR must parse back as a module and print identically.
        let ir = compile("int sum(int a, int b) { return a + b; }");

        let context = fcc_context();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&context, &ir)
            .expect("emitted IR should parse back");

        let mut buf = String::new();
        let mut fmt = tir::IRFormatter::new(&mut buf);
        tir::Operation::print(&module, &mut fmt).expect("print");
        assert_eq!(ir, buf);
    }

    #[test]
    fn cir_variadic_ir_roundtrips_through_parser() {
        let ir = compile(
            r#"int printf(const char *restrict format, ...);
int main(void) { printf("hello"); return 0; }"#,
        );

        let context = fcc_context();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&context, &ir)
            .expect("emitted CIR should parse back");

        let mut buf = String::new();
        let mut fmt = tir::IRFormatter::new(&mut buf);
        tir::Operation::print(&module, &mut fmt).expect("print");
        assert_eq!(ir, buf);
    }

    #[test]
    fn emits_struct_definition_and_type() {
        // A nested record is what still names a struct *type* in the IR: every
        // pointer is spelled opaquely, so only a field of struct type keeps the
        // `!cir.struct` spelling.
        let ir = compile(
            "struct Pair { char tag; int value; }; struct Boxed { struct Pair pair; }; int main(void) { struct Boxed boxed; return 0; }",
        );

        assert!(ir.contains("cir.define_struct"), "{ir}");
        assert!(ir.contains("!cir.struct<\"Pair\">"), "{ir}");
    }

    #[test]
    fn struct_ir_roundtrips_through_parser() {
        let ir = compile(
            "struct Pair { char tag; int value; }; int main(void) { struct Pair source; struct Pair destination; source.value = 1; destination = source; return destination.value; }",
        );
        let context = fcc_context();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&context, &ir)
            .expect("emitted struct CIR should parse");
        let mut parsed = String::new();
        let mut formatter = tir::IRFormatter::new(&mut parsed);
        tir::Operation::print(&module, &mut formatter).unwrap();

        assert_eq!(ir, parsed);
    }

    #[test]
    fn emits_member_address_and_scalar_load() {
        let ir = compile(
            "struct Pair { char tag; int value; }; int read(void) { struct Pair pair; return pair.value; }",
        );

        assert!(ir.contains("cir.get_member"), "{ir}");
        assert!(ir.contains("ptr.load"), "{ir}");
    }

    #[test]
    fn emits_scalar_member_store() {
        let ir = compile(
            "struct Pair { int value; }; int write(void) { struct Pair pair; pair.value = 7; return pair.value; }",
        );

        assert!(ir.matches("cir.get_member").count() >= 2, "{ir}");
        assert!(ir.contains("ptr.store"), "{ir}");
    }

    #[test]
    fn emits_whole_struct_copy() {
        let ir = compile(
            "struct Pair { int value; }; int copy(void) { struct Pair source; struct Pair destination; destination = source; return 0; }",
        );

        assert!(ir.contains("cir.copy_struct"), "{ir}");
    }

    #[test]
    fn lowers_member_access_to_pointer_addition() {
        let ir = compile_structs_lowered(
            "struct Pair { char tag; int value; }; int read(void) { struct Pair pair; return pair.value; }",
        );

        assert!(ir.contains("ptr.ptradd"), "{ir}");
        assert!(!ir.contains("cir.get_member"), "{ir}");
        assert!(!ir.contains("cir.define_struct"), "{ir}");
    }

    #[test]
    fn lowers_struct_copy_to_scalar_memory_operations() {
        let ir = compile_structs_lowered(
            "struct Pair { char tag; int value; }; int copy(void) { struct Pair source; struct Pair destination; destination = source; return 0; }",
        );

        assert!(!ir.contains("cir.copy_struct"), "{ir}");
        assert!(ir.matches("ptr.load").count() >= 2, "{ir}");
        assert!(ir.matches("ptr.store").count() >= 2, "{ir}");
    }

    #[test]
    fn cir_loop_ir_roundtrips_through_parser() {
        let ir = compile("int f(void) { int i = 0; while (i < 3) { i = i + 1; } return i; }");

        let context = fcc_context();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&context, &ir)
            .expect("emitted CIR loop should parse back");

        let mut buf = String::new();
        let mut fmt = tir::IRFormatter::new(&mut buf);
        tir::Operation::print(&module, &mut fmt).expect("print");
        assert_eq!(ir, buf);
    }

    fn lower_cir_control_flow(src: &str) -> String {
        let (context, module) = lower(src);
        let mut passes = tir::PassManager::new();
        let function_pipeline = passes.nest::<tir::builtin::FuncOp>();
        function_pipeline.add_pass(crate::passes::LowerCirControlFlowPass::new());
        function_pipeline.add_pass(tir::passes::Mem2RegPass::new());
        passes
            .run(&context, context.get_op(tir::Operation::id(&module)))
            .expect("lower CIR control flow");

        let mut lowered = String::new();
        let mut fmt = tir::IRFormatter::new(&mut lowered);
        tir::Operation::print(&module, &mut fmt).expect("print");
        // The mid-end takes structured control flow only: no C construct may
        // reach it as a branch, so every function this helper lowers is held to
        // that, not just the ones whose test says so.
        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        lowered
    }

    #[test]
    fn invariant_cir_while_lowers_to_canonical_scf() {
        let lowered = lower_cir_control_flow("int f(void) { while (1) {} return 0; }");

        assert!(lowered.contains("scf.while"), "{lowered}");
        assert!(!lowered.contains("cir.while"));
        assert!(!lowered.contains("scf.while") || !lowered.contains(" cond {"));
    }

    #[test]
    fn single_block_changing_cir_while_lowers_to_scf() {
        let lowered = lower_cir_control_flow(
            "int f(void) { int i = 0; while (i < 3) { i = i + 1; } return i; }",
        );

        assert!(!lowered.contains("cir.while"));
        assert!(lowered.contains("scf.while"));
        assert!(lowered.contains("scf.condition"));
        assert!(!lowered.contains("cond_br"));
    }

    #[test]
    fn early_return_lowers_to_structured_control_flow() {
        let lowered = lower_cir_control_flow("int f(int x) { if (x) return 1; return 2; }");

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.if"), "{lowered}");
    }

    #[test]
    fn return_inside_loop_leaves_the_loop_structurally() {
        let lowered = lower_cir_control_flow(
            "int f(int n) { int i = 0; while (i < n) { if (i == 3) return i; i = i + 1; } return 0; }",
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.while"), "{lowered}");
        assert!(lowered.contains("scf.break"), "{lowered}");
    }

    #[test]
    fn unreachable_return_after_break_emits_no_operations() {
        let ir = compile("int f(int c, int x) { while (c) { break; if (x) return 1; } return 0; }");

        assert_eq!(ir.matches("cir.break").count(), 1, "{ir}");
    }

    #[test]
    fn do_while_lowers_to_scf() {
        let lowered = lower_cir_control_flow(
            "int f(int n) { int t = 0; do { t = t + 1; if (t == 3) { return 3; } } while (t < n); return t; }",
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.while"), "{lowered}");
    }

    #[test]
    fn structured_break_preserves_token_scope_in_scf() {
        let lowered =
            lower_cir_control_flow("int f(int stop) { while (1) { if (stop) break; } return 0; }");

        assert!(!lowered.contains("cir.while"));
        assert!(lowered.contains("scf.while"), "{lowered}");
        assert!(lowered.contains("scf.break"), "{lowered}");
        assert!(!lowered.contains("cond_br"), "{lowered}");
    }

    /// The mid-end must see structured control flow only. After the
    /// CIR lowering the remaining sources of branches are `goto` and a
    /// `continue` inside a `for`, whose step has no structured spelling.
    #[test]
    fn c_constructs_lower_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int n, int m) {
                   int total = 0;
                   int i;
                   if (n < 0 && m > 0) {
                       return -1;
                   }
                   for (i = 0; i < n; i = i + 1) {
                       switch (i & 1) {
                       case 0:
                           total = total + (m ? i : -i);
                           break;
                       default:
                           if (total > 100) {
                               return total;
                           }
                           total = total - 2;
                           break;
                       }
                       do {
                           total = total - 1;
                       } while (total > n);
                   }
                   while (total < 0 || n > m) {
                       total = total + 1;
                       if (total == 7) {
                           return 7;
                       }
                   }
                   return total;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
    }

    /// `break` and `continue` in a `for`, and `continue` in a `do`, are the loop
    /// exits whose C meaning does not match the `scf.while` body they land in:
    /// the step and the trailing condition must still run on a `continue`.
    #[test]
    fn loop_control_constructs_lower_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int n) {
                   int total = 0;
                   int i;
                   int j;
                   for (i = 0; i < n; i = i + 1) {
                       if (i == 1) {
                           continue;
                       }
                       if (i == 7) {
                           break;
                       }
                       total = total + i;
                   }
                   j = 0;
                   do {
                       j = j + 1;
                       if (j == 2) {
                           continue;
                       }
                       total = total + j;
                   } while (j < n);
                   for (i = 0; i < n; i = i + 1) {
                       while (total > 0) {
                           total = total - 1;
                           if (total == 3) {
                               continue;
                           }
                           if (total == 1) {
                               break;
                           }
                       }
                       if (i == 2) {
                           continue;
                       }
                       total = total + 1;
                   }
                   return total;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.break"), "{lowered}");
    }

    /// A `goto` whose label follows it skips the statements in between, which
    /// the pending-jump variable expresses without leaving the region.
    #[test]
    fn forward_goto_lowers_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            "int f(int c) { int x = 0; if (c) goto done; x = 1; done: return x; }",
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(!lowered.contains("cir.goto"), "{lowered}");
        assert!(!lowered.contains("cir.label"), "{lowered}");
    }

    /// Two `goto`s to one label share the pending-jump slot, so the label needs
    /// no second resume point.
    #[test]
    fn two_gotos_to_one_label_lower_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int a, int b) {
                   int x = 0;
                   if (a) goto done;
                   x = 1;
                   if (b) goto done;
                   x = 2;
               done:
                   return x;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
    }

    /// A `goto` out of nested loops leaves each of them structurally, exactly
    /// as an early `return` does.
    #[test]
    fn goto_out_of_nested_loops_lowers_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int n) {
                   int i = 0;
                   int j = 0;
                   while (i < n) {
                       for (j = 0; j < n; j = j + 1) {
                           if (i * j > 6) goto done;
                       }
                       i = i + 1;
                   }
               done:
                   return i * 100 + j;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.break"), "{lowered}");
    }

    /// A loop written out of a `goto` becomes the dispatch loop the label's
    /// segment runs in.
    #[test]
    fn backward_goto_lowers_to_a_dispatch_loop() {
        let lowered = lower_cir_control_flow(
            r#"int f(int n) {
                   int i = 0;
               again:
                   i = i + 1;
                   if (i < n) goto again;
                   return i;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.while"), "{lowered}");
    }

    /// C allows a jump past a declaration into a compound statement; the block
    /// is spliced into the sequence that encloses it, which puts its label on
    /// the same footing as any other.
    #[test]
    fn goto_into_a_compound_statement_lowers_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int c) {
                   if (c) goto inside;
                   {
                       int x = 2;
                       inside:
                       c = c + 1;
                   }
                   return c;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
    }

    /// A `goto` into a loop body enters the loop without evaluating its
    /// condition, and the loop runs normally from there.
    #[test]
    fn goto_into_a_loop_body_lowers_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int enter) {
                   int value = 0;
                   int total = 0;
                   if (enter) goto inside;
                   while (value < 2) {
                       total = total + 10;
                   inside:
                       total = total + 1;
                       value = value + 1;
                   }
                   return total;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
    }

    /// A `goto` out of a `switch` case stops the switch, whose later items must
    /// not run even when they are reached by fallthrough.
    #[test]
    fn goto_out_of_a_switch_lowers_without_unstructured_branches() {
        let lowered = lower_cir_control_flow(
            r#"int f(int n) {
                   int total = 0;
                   switch (n) {
                   case 0:
                       total = 1;
                       goto done;
                   case 1:
                       total = 2;
                       break;
                   default:
                       total = 3;
                   }
                   total = total + 10;
               done:
                   return total;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
    }

    /// A `switch` body is flattened into guarded items rather than lowered as a
    /// statement sequence, so a label inside one is no resume point. That is the
    /// one `goto` fcc turns down, and it says so instead of branching.
    #[test]
    fn goto_to_a_label_inside_a_switch_body_is_rejected() {
        let error = codegen_error(
            r#"int f(int n) {
                   if (n) goto inside;
                   switch (n) {
                   case 0:
                   inside:
                       return 1;
                   }
                   return 0;
               }"#,
        );

        assert!(
            error.contains("goto to a label inside a switch body"),
            "{error}"
        );
    }

    #[test]
    fn break_terminated_do_body_lowers_without_unstructured_branches() {
        // The condition must not be inlined: break leaves before it is evaluated.
        let lowered = lower_cir_control_flow(
            r#"int bump(void);
               int f(int n) {
                   int total = 0;
                   do {
                       total = total + n;
                       break;
                   } while (bump());
                   return total;
               }"#,
        );

        assert!(!lowered.contains("cond_br"), "{lowered}");
        assert!(!lowered.contains("br ^"), "{lowered}");
        assert!(lowered.contains("scf.break"), "{lowered}");
        assert!(!lowered.contains("call @bump"), "{lowered}");
    }

    #[test]
    fn multiblock_cir_while_lowers_directly_to_cfg() {
        let context = fcc_context();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(
            &context,
            r#"module {
  func @f(%0: !i1) {
    cir.while %1 cond {
      cir.condition %0
    } body {
      br ^bb1
      ^bb1:
      cir.yield
    }
    return
  }
  module_end
}
"#,
        )
        .expect("parse multiblock CIR");
        let mut passes = tir::PassManager::new();
        passes
            .nest::<tir::builtin::FuncOp>()
            .add_pass(crate::passes::LowerCirControlFlowPass::new());
        passes
            .run(&context, context.get_op(tir::Operation::id(&module)))
            .expect("lower multiblock CIR");

        let mut lowered = String::new();
        let mut fmt = tir::IRFormatter::new(&mut lowered);
        tir::Operation::print(&module, &mut fmt).expect("print");
        assert!(!lowered.contains("cir.while"));
        assert!(!lowered.contains("scf.while"));
        assert!(lowered.contains("cond_br"));
        let roundtrip_context = fcc_context();
        tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&roundtrip_context, &lowered)
            .expect("lowered multiblock CFG should parse");
    }
}
