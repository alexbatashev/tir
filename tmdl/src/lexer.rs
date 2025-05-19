use core::fmt;
use std::fmt::Write;

use chumsky::prelude::*;

use crate::{Span, Spanned};

// Token definition
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
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
    /// `:`
    Colon,
    /// `;`
    Semicolon,
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
    /// `registers`
    KwRegisters,
    /// `parameters`
    KwParameters,
    /// `template`
    KwTemplate,
    /// `instruction`
    KwInstruction,
    /// `param`
    KwParam,
    /// `operands`
    KwOperands,
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

pub fn lex<'src>(source: &'src str) -> (Vec<Spanned<Token>>, Vec<Rich<'src, char, Span>>) {
    let (tokens, errors) = lexer().parse(source).into_output_errors();

    (tokens.unwrap_or_default(), errors)
}

pub(crate) fn lexer<'src>()
-> impl Parser<'src, &'src str, Vec<Spanned<Token>>, extra::Err<Rich<'src, char, Span>>> {
    let num = text::int(10)
        .then(just('.').then(text::digits(10)).or_not())
        .to_slice()
        .map(|n: &str| Token::Number(n.to_owned()));

    let str_ = just('"')
        .ignore_then(none_of('"').repeated().to_slice())
        .then_ignore(just('"'))
        .map(|s: &str| Token::StringLit(s.to_string()));

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
        just(":").to(Token::Colon),
        just(";").to(Token::Semicolon),
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
        "template" => Token::KwTemplate,
        "instruction" => Token::KwInstruction,
        "param" => Token::KwParam,
        "operands" => Token::KwOperands,
        _ => Token::Identifier(ident.to_owned()),
    });

    let token = str_.or(num).or(control).or(op).or(ident);

    token
        .padded()
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

impl fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Dot => f.write_str("."),
            Token::Asterisk => f.write_str("*"),
            Token::Identifier(i) => f.write_str(i),
            Token::LBrace => f.write_str("{"),
            Token::RBrace => f.write_str("}"),
            Token::KwParameters => f.write_str("parameters"),
            Token::Comment(s) => write!(f, "#{}", s),
            Token::Number(n) => write!(f, "{}", n),
            Token::StringLit(s) => write!(f, "\"{}\"", s),
            Token::Equals => f.write_str("="),
            Token::Plus => f.write_str("+"),
            Token::Dash => f.write_str("-"),
            Token::Colon => f.write_char(':'),
            Token::Semicolon => f.write_char(';'),
            Token::ForwardSlash => f.write_str("/"),
            Token::BackSlash => f.write_str("\\"),
            Token::Comma => f.write_str(","),
            Token::LBracket => f.write_str("["),
            Token::RBracket => f.write_str("]"),
            Token::LParen => f.write_str("("),
            Token::RParen => f.write_str(")"),
            Token::LAngle => f.write_str("<"),
            Token::RAngle => f.write_str(">"),
            Token::Pipe => f.write_str("|"),
            Token::KwIsa => f.write_str("isa"),
            Token::KwRequires => f.write_str("requires"),
            Token::KwRegClass => f.write_str("register_class"),
            Token::KwFor => f.write_str("for"),
            Token::KwRegisters => f.write_str("registers"),
            Token::KwTemplate => f.write_str("template"),
            Token::KwInstruction => f.write_str("instruction"),
            Token::KwParam => f.write_str("param"),
            Token::KwOperands => f.write_str("operands"),
        }
    }
}
