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

    /// Codegen behaviour is checked by the LIT tests under `fcc/checks/Codegen`.
    /// This Rust test covers the round-trip invariant, which is a property of
    /// the emitted IR rather than a textual match and so does not fit a
    /// FileCheck test.
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
}
