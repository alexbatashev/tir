use logos::Logos;

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"\s*")]
pub enum Token<'src> {
    // Punctuation
    #[token(",")]
    Comma,

    #[token(".section")]
    Section,
    #[token(".text")]
    Text,
    #[token(".data")]
    Data,
    #[token(".global")]
    Global,

    #[regex("[a-zA-Z_][a-zA-Z0-9_\\.]+:", |n| { let n = n.slice(); &n[0..n.len() - 1] })]
    Label(&'src str),

    #[regex("[a-zA-Z_][a-zA-Z0-9_\\.]+", |name| name.slice())]
    Ident(&'src str),
}

pub fn lex<'src>(source: &'src str) -> Result<Vec<Token<'src>>, ()> {
    let lexer = Token::lexer(source);
    lexer.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use crate::lexer::lex;

    #[test]
    fn asm_smoke() {
        let program = "
.text
.global _start
    _start:
    inst1 r1, r2, r3
    ret
";

        let tokens = lex(program);
        assert!(tokens.is_ok());

        insta::assert_debug_snapshot!(tokens.unwrap());
    }
}
