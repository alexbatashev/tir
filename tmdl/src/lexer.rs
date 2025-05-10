use chumsky::prelude::*;

use crate::{Span, Spanned};

// Token definition
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Whitespace(String),
    Comment(String),
    Identifier(String),
    Number(String),
    StringLit(String),

    /// `=`
    Equals,

    /// `+`
    Plus,
    /// `-`
    Dash,
    /// `/`
    ForwardSlash,
    /// `*`
    Asterisk,

    /// `.`
    Dot,
    /// `,`
    Comma,
    /// `\`
    BackSlash,

    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `<`
    LAngle,
    /// `>`
    RAngle,

    /// `|`
    Pipe,

    /// `isa`
    KwIsa,
    /// `requires`
    KwRequires,
    /// `register_class`
    KwRegClass,
    /// `for`
    KwFor,
    /// `self`
    KwSelf,
    /// `registers`
    KwRegisters,
    /// `parameters`
    KwParameters,
}

impl Token {
    pub fn as_ident(&self) -> &str {
        if let Token::Identifier(ident) = self {
            ident.as_str()
        } else {
            unreachable!()
        }
    }
}

pub(crate) fn lexer<'src>()
-> impl Parser<'src, &'src str, Vec<Spanned<Token>>, extra::Err<Rich<'src, char, Span>>> {
    let num = text::int(10)
        .then(just('.').then(text::digits(10)).or_not())
        .to_slice()
        .map(|n: &str| Token::Number(n.to_owned()));

    let whitespace = one_of(" \t\n\r")
        .repeated()
        .at_least(1)
        .to_slice()
        .map(|w: &str| Token::Whitespace(w.to_string()));

    let control = choice((
        just("{").to(Token::LBrace),
        just("}").to(Token::RBrace),
        just("[").to(Token::LBracket),
        just("]").to(Token::RBracket),
        just("(").to(Token::LParen),
        just(")").to(Token::RParen),
        just("<").to(Token::LAngle),
        just(">").to(Token::RAngle),
        just(",").to(Token::Comma),
    ));

    let op = choice((
        just("=").to(Token::Equals),
        just("+").to(Token::Plus),
        just("*").to(Token::Asterisk),
        just("/").to(Token::ForwardSlash),
        just("|").to(Token::Pipe),
        just(".").to(Token::Dot),
    ));

    let ident = text::ascii::ident().map(|ident: &str| match ident {
        "isa" => Token::KwIsa,
        "requires" => Token::KwRequires,
        "for" => Token::KwFor,
        "registers" => Token::KwRegisters,
        "register_class" => Token::KwRegClass,
        "parameters" => Token::KwParameters,
        _ => Token::Identifier(ident.to_owned()),
    });

    let token = whitespace.or(num).or(control).or(op).or(ident);

    token
        .map_with(|tok, e| (tok, e.span()))
        .recover_with(skip_then_retry_until(any().ignored(), end()))
        .repeated()
        .collect()
}

#[cfg(test)]
mod test {
    use chumsky::Parser;

    use super::lexer;

    #[test]
    fn smoke_lexer() {
        let input = "
          isa RV32I {
              XLEN = 32
          }
       ";

        let parser = lexer();
        let result = parser.parse(input);

        println!("{:#?}", result);
    }
}
