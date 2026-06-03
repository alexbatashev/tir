#[cfg(test)]
mod tests {
    use crate::codegen::codegen;
    use crate::parser::parse;
    use logos::Logos;

    use crate::lexer::Token;

    fn compile(src: &str) -> String {
        let tokens: Vec<Token> = Token::lexer(src).map(|r| r.unwrap()).collect();
        let unit = parse(&tokens).expect("parse");
        codegen(&unit).expect("codegen")
    }

    #[test]
    fn sum_of_two_numbers() {
        let ir = compile("int sum(int a, int b) { return a + b; }");

        // The function and its memory-based parameter handling are present.
        assert!(ir.contains("module"));
        assert!(ir.contains("func @sum"));
        assert!(ir.contains("ptr.alloca"));
        assert!(ir.contains("ptr.store"));
        assert!(ir.contains("ptr.load"));
        assert!(ir.contains("addi"));
        assert!(ir.contains("return"));
        // Two parameters => two stack slots.
        assert_eq!(ir.matches("ptr.alloca").count(), 2);
        assert_eq!(ir.matches("ptr.store").count(), 2);
        assert_eq!(ir.matches("ptr.load").count(), 2);
    }

    #[test]
    fn ir_roundtrips_through_parser() {
        // The emitted IR must parse back as a module and print identically.
        let ir = compile("int sum(int a, int b) { return a + b; }");

        let context = tir::Context::with_default_dialects();
        let module = tir::parse::ir::parse_ir::<tir::builtin::ModuleOp>(&context, &ir)
            .expect("emitted IR should parse back");

        let mut buf = String::new();
        let mut fmt = tir::IRFormatter::new(&mut buf);
        tir::Operation::print(&module, &mut fmt).expect("print");
        assert_eq!(ir, buf);
    }

    #[test]
    fn local_variable_and_arithmetic() {
        let ir = compile("int f(int a, int b) { int c = a * b; return c + 1; }");
        // params a, b and local c => three slots.
        assert_eq!(ir.matches("ptr.alloca").count(), 3);
        assert!(ir.contains("muli"));
        assert!(ir.contains("constant"));
        assert!(ir.contains("addi"));
    }

    #[test]
    fn void_function() {
        let ir = compile("void nop(void) { return; }");
        assert!(ir.contains("func @nop"));
        assert!(ir.contains("return"));
        assert_eq!(ir.matches("ptr.alloca").count(), 0);
    }
}
