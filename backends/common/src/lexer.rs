use logos::Logos;

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"\s*")]
// Line comments: `#` (GNU as / RISC-V), `//` (ARM). Skipping them lets a `.S`
// test file carry lit `RUN:`/`CHECK:` directives without confusing the lexer.
#[logos(skip r"#[^\n]*")]
#[logos(skip r"//[^\n]*")]
pub enum Token<'src> {
    // Punctuation
    #[token(",")]
    Comma,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,

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

    #[regex("-?[0-9]+", |num| num.slice())]
    DecNumber(&'src str),

    #[regex("-?0[xX][0-9a-fA-F]+", |num| num.slice())]
    HexNumber(&'src str),
}

pub fn lex<'src>(source: &'src str) -> Result<Vec<Token<'src>>, ()> {
    let mut lexer = Token::lexer(source);

    let mut tokens = vec![];

    while let Some(token) = lexer.next() {
        match token {
            Ok(token) => tokens.push(token),
            // FIXME: technically, lexers are not supposed to fail. Need to decide whether to throw an error or just panic.
            Err(_) => panic!("Error at {:?}", lexer.span()),
        }
    }

    Ok(tokens)
}

impl<'src> tir::parse::tokens::TokenLike<'src> for Token<'src> {
    fn as_ident(&self) -> Option<&'src str> {
        match self {
            Token::Ident(s) | Token::Label(s) => Some(s),
            _ => None,
        }
    }

    fn is_symbol(&self, sym: tir::parse::tokens::Symbol) -> bool {
        match (self, sym) {
            (Token::Comma, tir::parse::tokens::Symbol::Comma) => true,
            _ => false,
        }
    }
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
