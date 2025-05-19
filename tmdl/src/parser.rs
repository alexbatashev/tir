use std::collections::HashMap;

use chumsky::{input::ValueInput, prelude::*};

use crate::{
    Span, Spanned,
    ast::{self, *},
    lexer::Token,
};

pub fn parse<'src>(
    source: &'src str,
    tokens: &'src [Spanned<Token>],
) -> (Option<File>, Vec<Rich<'src, Token, Span>>) {
    file()
        .then_ignore(end())
        .parse(tokens.map((source.len()..source.len()).into(), |(t, s)| (t, s)))
        .into_output_errors()
}

/// Parse single translation unit
fn file<'src, I>() -> impl Parser<'src, I, File, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    choice((
        isa_def().map(Item::Isa),
        register_class_def().map(Item::RegisterClass),
        template_def().map(Item::Template),
    ))
    .repeated()
    .at_least(0)
    .collect()
    .map(|items| File { items })
}

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
        .then(ident())
        .then(isa_requirements())
        .then_ignore(just(Token::LBrace))
        .then(
            isa_parameter()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect(),
        )
        .then_ignore(just(Token::RBrace))
        .map(
            |(((_kw, name), requires), parameters): (
                ((Token, String), Option<IsaRequirement>),
                HashMap<String, Expr>,
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
/// register_class GPR for TestIsa {
///   parameters {
///     width = self.XLEN,
///     encoding_len = 5,
///   }
///   registers {
///     x0("zero") => { traits = [hardwired_zero] },
///     x1("ra") => { traits = [return_address, caller_saved] },
///     x2..x31("r{}") => { traits = [ callee_saved ] },
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
        .then(ident)
        .then(for_isas())
        .then_ignore(just(Token::LBrace))
        .then(register_class_parameters())
        .then(register_class_registers())
        .then_ignore(just(Token::RBrace))
        .map(
            |(((((), name), for_isas), parameters), registers)| RegisterClass {
                name,
                for_isas,
                parameters,
                registers,
            },
        )
        .labelled("register class definition")
}

fn template_def<'src, I>() -> impl Parser<'src, I, Template, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };

    just(Token::KwTemplate)
        .ignored()
        .then(ident)
        .then(for_isas().or_not())
        .then(
            choice((
                parameter().map(TemplateBody::Param),
                operands().map(TemplateBody::Operands),
            ))
            .repeated()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((((), name), for_isas), body)| {
            let params = body
                .iter()
                .filter_map(|b| match b {
                    TemplateBody::Param(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();

            let operands = body
                .iter()
                .find_map(|b| {
                    if let TemplateBody::Operands(o) = b {
                        Some(o.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            Template {
                name,
                for_isas: for_isas.unwrap_or_default(),
                params,
                operands,
                encoding: vec![],
            }
        })
}

enum TemplateBody {
    Param((String, (ast::Type, Option<ast::Expr>))),
    Operands(HashMap<String, String>),
}

fn parameter<'src, I>()
-> impl Parser<'src, I, (String, (ast::Type, Option<ast::Expr>)), extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    just(Token::KwParam)
        .ignored()
        .then(ident.clone())
        .then_ignore(just(Token::Colon))
        .then(type_())
        .then(just(Token::Equals).then(inline_expr()).or_not())
        .then_ignore(just(Token::Semicolon))
        .map(|((((), name), ty), expr)| {
            let expr = expr.map(|e| e.1);
            (name, (ty, expr))
        })
}

fn operands<'src, I>()
-> impl Parser<'src, I, HashMap<String, String>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(i) => i.to_string() };
    let single_operand = ident.clone().then_ignore(just(Token::Colon)).then(ident);
    just(Token::KwOperands)
        .ignored()
        .then(
            single_operand
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect()
                .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((), operands)| operands)
}

fn isa_parameter<'src, I>()
-> impl Parser<'src, I, (String, Expr), extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(i) => i };
    let number = select! { Token::Number(i) => i };

    ident
        .then_ignore(just(Token::Equals))
        .then(number)
        .map(|(ident, number)| (ident, Expr::Lit(Lit::Int(LitInt::new(number)))))
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
        .separated_by(just(Token::Pipe))
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBracket), just(Token::RBracket))
        .map(|any| IsaRequirement::Any(any));
    let all = ident
        .clone()
        .separated_by(just(Token::Comma))
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBracket), just(Token::RBracket))
        .map(|all| IsaRequirement::All(all));
    just(Token::KwRequires)
        .ignored()
        .then(choice((single_isa, any, all)))
        .or_not()
        .map(|isa| isa.map(|(_, isa)| isa))
}

fn for_isas<'src, I>() -> impl Parser<'src, I, Vec<String>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    just(Token::KwFor)
        .ignored()
        .then(
            ident
                .separated_by(just(Token::Comma))
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LBracket), just(Token::RBracket)),
        )
        .map(|(_, isas)| isas)
        .or_not()
        .map(|isas_opt| isas_opt.unwrap_or_default())
}

fn register_class_parameters<'src, I>()
-> impl Parser<'src, I, HashMap<String, Expr>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.clone() };

    let single_parameter = ident
        .clone()
        .then_ignore(just(Token::Equals))
        .then(inline_expr());
    just(Token::KwParameters)
        .ignored()
        .then(
            single_parameter
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<HashMap<String, Expr>>()
                .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((), v)| v)
        .labelled("register class parameters")
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
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect()
                .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((), v)| v)
        .labelled("register class registers")
}

fn single_register<'src, I>()
-> impl Parser<'src, I, RegisterDef, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    let alias = just(Token::LParen)
        .ignored()
        .then(select! { Token::StringLit(s) => s.to_string() })
        .then_ignore(just(Token::RParen))
        .map(|(_, alias)| Some(alias))
        .or_not()
        .map(|o| o.flatten());

    let reg_traits = register_traits();

    let single = ident
        .then(alias)
        .then_ignore(just(Token::Equals).then_ignore(just(Token::RAngle)))
        .then_ignore(just(Token::LBrace))
        .then(reg_traits)
        .then_ignore(just(Token::RBrace))
        .map(|((name, alias), traits)| {
            RegisterDef::Single(Register {
                name,
                alias,
                traits,
                subregisters: Vec::new(),
            })
        });

    let range = register_range();

    choice((range, single)).labelled("register")
}

fn ident<'src, I>() -> impl Parser<'src, I, String, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    any().filter(is_ident).map(|t| t.as_ident().to_string())
}

fn register_traits<'src, I>()
-> impl Parser<'src, I, Vec<RegisterTrait>, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    just(Token::Identifier("traits".into()))
        .then_ignore(just(Token::Equals))
        .then_ignore(just(Token::LBracket))
        .ignore_then(
            select! { Token::Identifier(t) => t.to_string() }
                .separated_by(just(Token::Comma))
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(Token::RBracket))
        .map(|traits| {
            traits
                .into_iter()
                .filter_map(|t| match t.as_str() {
                    "hardwired_zero" => Some(RegisterTrait::HardwiredZero),
                    "return_address" => Some(RegisterTrait::ReturnAddress),
                    "caller_saved" => Some(RegisterTrait::CallerSaved),
                    "callee_saved" => Some(RegisterTrait::CalleeSaved),
                    "stack_pointer" => Some(RegisterTrait::StackPointer),
                    _ => None,
                })
                .collect()
        })
}

fn register_range<'src, I>()
-> impl Parser<'src, I, RegisterDef, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    let alias_pattern = just(Token::LParen)
        .ignored()
        .then(select! { Token::StringLit(s) => s.to_string() })
        .then_ignore(just(Token::RParen))
        .map(|(_, alias)| Some(alias))
        .or_not()
        .map(|o| o.flatten());

    let reg_traits = register_traits();

    ident
        .then_ignore(just(Token::Dot).then_ignore(just(Token::Dot)))
        .then(ident)
        .then(alias_pattern)
        .then_ignore(just(Token::Equals).then_ignore(just(Token::RAngle)))
        .then_ignore(just(Token::LBrace))
        .then(reg_traits)
        .then_ignore(just(Token::RBrace))
        .map(|(((start, end), alias_pattern), traits)| {
            RegisterDef::Range(RegisterRange {
                start,
                end,
                alias_pattern,
                traits,
            })
        })
}

fn inline_expr<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    recursive(|expr| {
        let val = select! {
            Token::Identifier(i) => Ident::new(i).into(),
            Token::Number(n) => LitInt::new(n).into(),
        }
        .labelled("value");

        let atom = val
            .or(expr
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen)))
            .boxed();

        let ident = select! {Token::Identifier(i) => i};

        let access =
            atom.clone()
                .foldl_with(just(Token::Dot).then(ident).repeated(), |a, (op, b), e| {
                    Expr::Field(Field {
                        base: Box::new(a),
                        member: b,
                    })
                });

        access.or(atom)
    })
}

fn type_<'src, I>() -> impl Parser<'src, I, ast::Type, extra::Err<Rich<'src, Token, Span>>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let num = select! { Token::Number(n) => n };

    let bits = just(Token::Identifier("bits".to_string()))
        .ignored()
        .then_ignore(just(Token::LAngle))
        .then(num.try_map_with(|n, e| {
            n.parse::<u16>()
                .map_err(|_| Rich::custom(e.span(), "Expected unsigned integer"))
        }))
        .then_ignore(just(Token::RAngle))
        .map(|((), bits)| Type::Bits(bits));
    choice((
        just(Token::Identifier("String".to_string())).to(ast::Type::String),
        just(Token::Identifier("Integer".to_string())).to(ast::Type::Integer),
        bits,
    ))
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
