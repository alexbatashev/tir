use std::collections::HashMap;

use chumsky::{input::ValueInput, prelude::*, recursive};

use crate::{
    Span, Spanned,
    ast::{self, *},
    lexer::Token,
};

pub fn parse<'src>(
    source: &'src str,
    tokens: &'src [Spanned<Token>],
) -> (Option<File>, Vec<Rich<'src, Token<'src>, Span>>) {
    file()
        .then_ignore(end())
        .parse(tokens.map((source.len()..source.len()).into(), |(t, s)| (t, s)))
        .into_output_errors()
}

/// Parse single translation unit
fn file<'src, I>() -> impl Parser<'src, I, File, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    choice((
        isa_def().map(Item::Isa),
        register_class_def().map(Item::RegisterClass),
        template_def().map(Item::Template),
        instruction_def().map(Item::Instruction),
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
fn isa_def<'src, I>() -> impl Parser<'src, I, Isa, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    just(Token::KwIsa)
        .ignore_then(ident())
        .then(isa_requirements())
        .then_ignore(just(Token::LBrace))
        .then(parameter().repeated().collect())
        .then_ignore(just(Token::RBrace))
        .map(|((name, requires), parameters)| Isa {
            name,
            requires,
            parameters,
        })
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
-> impl Parser<'src, I, RegisterClass, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    just(Token::KwRegClass)
        .ignore_then(ident)
        .then(for_isas())
        .then(
            choice((
                parameter().map(RegClassBody::Param),
                register_class_registers().map(RegClassBody::Registers),
            ))
            .repeated()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((name, for_isas), body)| {
            let parameters = body
                .iter()
                .filter_map(|b| match b {
                    RegClassBody::Param(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();

            let registers = body
                .iter()
                .find_map(|b| {
                    if let RegClassBody::Registers(r) = b {
                        Some(r.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            RegisterClass {
                name,
                for_isas,
                parameters,
                registers,
            }
        })
        .labelled("register class definition")
}

enum RegClassBody {
    Param((String, (ast::Type, Option<ast::Expr>))),
    Registers(Vec<RegisterDef>),
}

fn template_def<'src, I>()
-> impl Parser<'src, I, Template, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };

    just(Token::KwTemplate)
        .ignore_then(ident)
        .then(for_isas().or_not())
        .then(just(Token::Colon).ignore_then(ident.clone()).or_not())
        .then(
            choice((
                parameter().map(TemplateOrInstBody::Param),
                instruction_operands().map(TemplateOrInstBody::Operands),
                encoding().map(TemplateOrInstBody::Encoding),
                asm().map(TemplateOrInstBody::Asm),
            ))
            .repeated()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|(((name, for_isas), parent_template), body)| {
            let params = body
                .iter()
                .filter_map(|b| match b {
                    TemplateOrInstBody::Param(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();

            let operands = body
                .iter()
                .find_map(|b| {
                    if let TemplateOrInstBody::Operands(o) = b {
                        Some(o.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            let encoding = body
                .iter()
                .find_map(|b| {
                    if let TemplateOrInstBody::Encoding(e) = b {
                        Some(e.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            let asm = body.iter().find_map(|b| {
                if let TemplateOrInstBody::Asm(a) = b {
                    Some(a.clone())
                } else {
                    None
                }
            });

            Template {
                name,
                for_isas: for_isas.unwrap_or_default(),
                parent_template,
                params,
                operands,
                encoding,
                asm,
            }
        })
}

fn instruction_def<'src, I>()
-> impl Parser<'src, I, Instruction, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };

    just(Token::KwInstruction)
        .ignore_then(ident)
        .then(for_isas().or_not())
        .then(just(Token::Colon).ignore_then(ident.clone()).or_not())
        .then(
            choice((
                parameter().map(TemplateOrInstBody::Param),
                instruction_operands().map(TemplateOrInstBody::Operands),
                encoding().map(TemplateOrInstBody::Encoding),
                asm().map(TemplateOrInstBody::Asm),
                behavior().map(TemplateOrInstBody::Behavior),
            ))
            .repeated()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|(((name, for_isas), parent_template), body)| {
            let params = body
                .iter()
                .filter_map(|b| match b {
                    TemplateOrInstBody::Param(p) => Some(p.clone()),
                    _ => None,
                })
                .collect();

            let operands = body
                .iter()
                .find_map(|b| {
                    if let TemplateOrInstBody::Operands(o) = b {
                        Some(o.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            let encoding = body
                .iter()
                .find_map(|b| {
                    if let TemplateOrInstBody::Encoding(e) = b {
                        Some(e.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            let asm = body.iter().find_map(|b| {
                if let TemplateOrInstBody::Asm(a) = b {
                    Some(a.clone())
                } else {
                    None
                }
            });

            let behavior = body
                .iter()
                .find_map(|b| {
                    if let TemplateOrInstBody::Behavior(a) = b {
                        Some(a.clone())
                    } else {
                        None
                    }
                })
                .unwrap();

            Instruction {
                name,
                for_isas: for_isas.unwrap_or_default(),
                parent_template,
                params,
                operands,
                encoding,
                asm,
                behavior,
            }
        })
        .labelled("instruction definition")
}

enum TemplateOrInstBody {
    Param((String, (ast::Type, Option<ast::Expr>))),
    Operands(HashMap<String, Type>),
    Encoding(Vec<EncodingArm>),
    Asm(Expr),
    Behavior(Expr),
}

fn asm<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    just(Token::KwAsm).ignore_then(expr())
}

fn behavior<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    just(Token::KwBehavior).ignore_then(expr())
}

fn encoding<'src, I>()
-> impl Parser<'src, I, Vec<EncodingArm>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let num = select! { Token::Number(i) => i.parse::<u16>().unwrap() };

    let single_bit = num
        .clone()
        .then_ignore(arrow())
        .then(inline_expr())
        .map(|(start, value)| EncodingArm {
            start,
            end: None,
            value,
        });
    let range = num
        .clone()
        .then_ignore(range_op())
        .then(num)
        .then_ignore(arrow())
        .then(inline_expr())
        .map(|((start, end), value)| EncodingArm {
            start,
            end: Some(end),
            value,
        });
    just(Token::KwEncoding)
        .ignored()
        .then(
            choice((single_bit, range))
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect()
                .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        )
        .map(|((), arms)| arms)
}

fn arrow<'src, I>() -> impl Parser<'src, I, (), extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    just(Token::Equals)
        .ignored()
        .then_ignore(just(Token::RAngle))
        .to(())
        .labelled("arrow operator")
}

fn range_op<'src, I>() -> impl Parser<'src, I, (), extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    just(Token::Dot)
        .ignored()
        .then_ignore(just(Token::Dot))
        .to(())
        .labelled("range operator")
}

fn parameter<'src, I>() -> impl Parser<
    'src,
    I,
    (String, (ast::Type, Option<ast::Expr>)),
    extra::Err<Rich<'src, Token<'src>, Span>>,
>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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

fn instruction_operands<'src, I>()
-> impl Parser<'src, I, HashMap<String, ast::Type>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(i) => i.to_string() };
    let single_operand = ident.clone().then_ignore(just(Token::Colon)).then(type_());
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

fn isa_requirements<'src, I>()
-> impl Parser<'src, I, Option<IsaRequirement>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };
    let single_isa =
        select! { Token::Identifier(ident) => IsaRequirement::Single(ident.to_string()) };
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

fn for_isas<'src, I>()
-> impl Parser<'src, I, Vec<String>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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
-> impl Parser<'src, I, HashMap<String, Expr>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(ident) => ident.to_string() };

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
-> impl Parser<'src, I, Vec<RegisterDef>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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
-> impl Parser<'src, I, RegisterDef, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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

fn ident<'src, I>() -> impl Parser<'src, I, String, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    any().filter(is_ident).map(|t| t.as_ident().to_string())
}

fn register_traits<'src, I>()
-> impl Parser<'src, I, Vec<RegisterTrait>, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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
-> impl Parser<'src, I, RegisterDef, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
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

fn inline_expr<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    recursive(|expr| {
        let val = select! {
            Token::Identifier(i) => Ident::new(i.to_string()).into(),
            Token::Number(n) => LitInt::new(n.to_string()).into(),
            Token::StringLit(s) => LitStr::new(s.to_string()).into(),
        }
        .labelled("value");

        let num = select! {
          Token::Number(n) => n.parse::<u16>().unwrap(),
        };

        let ident = select! { Token::Identifier(i) => i.to_string() };

        let atom = val
            .or(expr
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen)))
            .boxed();

        let access =
            atom.clone()
                .foldl_with(just(Token::Dot).then(ident).repeated(), |a, (_, b), _| {
                    Expr::Field(Field {
                        base: Box::new(a),
                        member: b,
                    })
                });

        let slice = access
            .clone()
            .or(atom.clone())
            .then(
                num.clone()
                    .then_ignore(range_op())
                    .then(num.clone())
                    .delimited_by(just(Token::LBracket), just(Token::RBracket)),
            )
            .map(|(base, (start, end))| {
                Expr::Slice(Slice {
                    base: Box::new(base),
                    start,
                    end,
                })
            })
            .boxed();

        let index = access
            .clone()
            .or(atom.clone())
            .then(num.delimited_by(just(Token::LBracket), just(Token::RBracket)))
            .map(|(base, index)| {
                Expr::IndexAccess(IndexAccess {
                    base: Box::new(base),
                    index,
                })
            })
            .boxed();

        let items = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>();

        let call = atom.clone().foldl(
            items
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .repeated(),
            |base, arguments| {
                Expr::Call(Call {
                    base: Box::new(base),
                    arguments,
                })
            },
        );

        let binary_op = |a, (op, b)| {
            Expr::Binary(Binary {
                lhs: Box::new(a),
                rhs: Box::new(b),
                op,
            })
        };

        let basic = slice.or(index).or(access).or(call).or(atom);

        let op = just(Token::Asterisk)
            .to(BinOp::Mul)
            .or(just(Token::ForwardSlash).to(BinOp::Div));
        let product = basic.clone().foldl(op.then(expr).repeated(), binary_op);

        let op = just(Token::Plus)
            .to(BinOp::Add)
            .or(just(Token::Dash).to(BinOp::Sub));
        let sum = product
            .clone()
            .foldl(op.then(product).repeated(), binary_op);

        sum
    })
}

fn expr<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! {Token::Identifier(i) => i.to_string()};

    recursive(|expr| {
        let assign = ident
            .clone()
            .then_ignore(just(Token::Equals))
            .then(expr.clone().or(inline_expr()))
            .map(|(dest, value)| {
                Expr::Assign(Assign {
                    dest,
                    value: Box::new(value),
                })
            })
            .labelled("assignment");
        let stmt = expr.clone().or(assign).or(inline_expr());

        let block = stmt
            .separated_by(just(Token::Semicolon).to(()).or(empty().to(())))
            .collect()
            .then(just(Token::Semicolon).or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(|(stmts, sc)| {
                Block {
                    stmts,
                    last_expr_return: sc.is_none(),
                }
                .into()
            })
            .boxed()
            .recover_with(via_parser(nested_delimiters(
                Token::LBrace,
                Token::RBrace,
                [
                    (Token::LParen, Token::RParen),
                    (Token::LBracket, Token::RBracket),
                ],
                |_| Expr::Invalid,
            )));

        let if_ = recursive(|if_| {
            just(Token::KwIf)
                .ignore_then(inline_expr())
                .then(block.clone())
                .then(
                    just(Token::KwElse)
                        .ignore_then(block.clone().or(if_))
                        .or_not(),
                )
                .map(|((cond, a), b)| {
                    Expr::If(If {
                        cond: Box::new(cond),
                        then: Box::new(a),
                        else_: b.map(Box::new),
                    })
                })
                .boxed()
        });

        block.clone().or(if_)
    })
}

fn type_<'src, I>() -> impl Parser<'src, I, ast::Type, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let num = select! { Token::Number(n) => n };

    let ident = select! { Token::Identifier(i) => i.to_string() };

    let bits = just(Token::Identifier("bits"))
        .ignored()
        .then_ignore(just(Token::LAngle))
        .then(num.try_map_with(|n, e| {
            n.parse::<u16>()
                .map_err(|_| Rich::custom(e.span(), "Expected unsigned integer"))
        }))
        .then_ignore(just(Token::RAngle))
        .map(|((), bits)| Type::Bits(bits));
    choice((
        just(Token::Identifier("String")).to(ast::Type::String),
        just(Token::Identifier("Integer")).to(ast::Type::Integer),
        bits,
        ident.map(ast::Type::Struct),
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
