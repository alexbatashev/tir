use std::collections::HashMap;

use chumsky::{input::ValueInput, prelude::*};

use crate::{Span, ast::*, lexer::Token};

/// Parse isa definition.
/// Example:
///
/// ```tmdl
/// isa RV32I {
///   XLEN = 32,
/// }
/// ```
fn isa_def<'src, I>() -> impl Parser<'src, I, Isa, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    just(Token::KwIsa)
        .then_ignore(any().filter(is_trivia).repeated())
        .then(ident())
        .then(isa_requirements())
        .then_ignore(just(Token::LBrace).padded_by(trivia()))
        .then(isa_parameter().separated_by(just(Token::Comma)).collect())
        .then_ignore(just(Token::RBrace).padded_by(trivia()))
        .map(
            |(((_kw, name), requires), parameters): (
                ((Token, String), Option<IsaRequirement>),
                HashMap<String, i32>,
            )| Isa {
                name,
                requires,
                parameters,
            },
        )
        .labelled("ISA definition")
}

/// Register class definition
///
/// Example:
/// ```tmdl
/// register_class GPR for RV32I {
///   parameters {
///     width = self.XLEN,
///     encoding_len = 5,
///   }
///   registers {
///     x0("zero") => { traits = [hardwired_zero] },
///     x1("ra") => { traits = [return_address, caller_saved] },
///   }
/// }
/// ```
fn register_class_def<'src, I>()
-> impl Parser<'src, I, RegisterClass, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    just(Token::KwRegClass)
        .ignored()
        .then(ident.clone().padded_by(trivia()))
        .then_ignore(just(Token::LBrace))
        .then(register_class_parameters())
        .then(register_class_registers())
        .then_ignore(just(Token::RBrace).padded_by(trivia()))
        .map(|((((), name), parameters), registers)| RegisterClass {
            name,
            for_isas: Vec::new(),
            parameters,
            registers,
        })
}

fn isa_parameter<'src, I>()
-> impl Parser<'src, I, (String, i32), extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(i) => i };
    let number = select! { Token::Number(i) => i };

    ident
        .then_ignore(just(Token::Equals).padded_by(trivia()))
        .then(number)
        .try_map(|(ident, number), span| {
            number
                .parse::<i32>()
                .map(|n| (ident, n))
                .map_err(|e| Rich::custom(span, e))
        })
}

fn isa_requirements<'src, I>()
-> impl Parser<'src, I, Option<IsaRequirement>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    let single_isa = select! { Token::Identifier(ident) => IsaRequirement::Single(ident) };
    let any = ident
        .clone()
        .separated_by(just(Token::Pipe).padded_by(trivia()))
        .collect::<Vec<_>>()
        .delimited_by(
            just(Token::LBracket).padded_by(trivia()),
            just(Token::RBracket).padded_by(trivia()),
        )
        .map(|any| IsaRequirement::Any(any));
    let all = ident
        .clone()
        .separated_by(just(Token::Comma).padded_by(trivia()))
        .collect::<Vec<_>>()
        .delimited_by(
            just(Token::LBracket).padded_by(trivia()),
            just(Token::RBracket).padded_by(trivia()),
        )
        .map(|all| IsaRequirement::All(all));
    just(Token::KwRequires)
        .ignored()
        .then(choice((single_isa, any, all)))
        .or_not()
        .map(|isa| isa.map(|(_, isa)| isa))
}

fn register_class_parameters<'src, I>()
-> impl Parser<'src, I, HashMap<String, i32>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.clone() };
    let number = select! { Token::Number(num) => num.clone() };

    let single_parameter = ident
        .clone()
        .then_ignore(just(Token::Equals).padded_by(trivia()))
        .then(number)
        .try_map(|(name, value), span| {
            value
                .parse::<i32>()
                .map(|n| (name, n))
                .map_err(|e| Rich::custom(span, e))
        });
    just(Token::KwParameters)
        .ignored()
        .then(
            single_parameter
                .separated_by(just(Token::Comma).padded_by(trivia()))
                .collect::<HashMap<String, i32>>()
                .delimited_by(
                    just(Token::LBrace).padded_by(trivia()),
                    just(Token::RBrace).padded_by(trivia()),
                ),
        )
        .map(|((), v)| v)
}

fn register_class_registers<'src, I>()
-> impl Parser<'src, I, Vec<RegisterDef>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    just(Token::KwRegisters)
        .ignored()
        .then(
            single_register()
                .separated_by(just(Token::Comma).padded_by(trivia()))
                .collect()
                .delimited_by(
                    just(Token::LBrace).padded_by(trivia()),
                    just(Token::RBrace).padded_by(trivia()),
                ),
        )
        .map(|((), v)| v)
}

fn single_register<'src, I>()
-> impl Parser<'src, I, RegisterDef, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    todo()
}

fn ident<'src, I>() -> impl Parser<'src, I, String, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    any().filter(is_ident).map(|t| t.as_ident().to_string())
}

fn trivia<'src, I>() -> impl Parser<'src, I, (), extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    any().filter(is_trivia).repeated().at_least(0).to(())
}

fn is_trivia(token: &Token) -> bool {
    match token {
        Token::Whitespace(_) | Token::Comment(_) => true,
        _ => false,
    }
}

fn is_ident(token: &Token) -> bool {
    if let Token::Identifier(_) = token {
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use chumsky::Parser;
    use chumsky::prelude::*;

    use crate::lexer::lexer;

    use super::isa_def;

    #[test]
    fn smoke_isa() {
        let code = "isa RV32I {}";
        let (tokens, mut _errors) = lexer().parse(code).into_output_errors();

        let tokens = tokens.unwrap();
        let isa = isa_def().then(end()).parse(
            tokens
                .as_slice()
                .map((code.len()..code.len()).into(), |(t, s)| (t, s)),
        );

        println!("{:?}", isa);
        assert!(isa.has_output());
    }
}
