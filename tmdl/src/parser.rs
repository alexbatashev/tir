use chumsky::{input::ValueInput, prelude::*};

use crate::{
    Span, Spanned, Type,
    ast::{self, *},
    lexer::Token,
};

pub fn parse<'src>(
    source: &'src str,
    tokens: &'src [Spanned<Token>],
    file_name: &str,
) -> (Option<File>, Vec<Rich<'src, Token<'src>, Span>>) {
    file(file_name)
        .then_ignore(end())
        .parse(tokens.map((source.len()..source.len()).into(), |(t, s)| (t, s)))
        .into_output_errors()
}

/// Parse single translation unit
fn file<'src, I>(
    file_name: &str,
) -> impl Parser<'src, I, File, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let fname = file_name.to_string();
    choice((
        isa_def().map(Item::Isa),
        register_class_def().map(Item::RegisterClass),
        template_def().map(Item::Template),
        instruction_def().map(Item::Instruction),
    ))
    .repeated()
    .at_least(0)
    .collect()
    .map(move |items| File {
        items,
        file_name: fname.clone(),
    })
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
        .map_with(|((name, requires), parameters), e| Isa {
            name,
            requires,
            parameters,
            span: e.span(),
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
        .map_with(|((name, for_isas), body), e| {
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
                span: e.span(),
            }
        })
        .labelled("register class definition")
}

enum RegClassBody {
    Param((String, (Type, Option<ast::Expr>))),
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
        .map_with(|(((name, for_isas), parent_template), body), e| {
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
                span: e.span(),
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
        .map_with(|(((name, for_isas), parent_template), body), e| {
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
                span: e.span(),
            }
        })
        .labelled("instruction definition")
}

enum TemplateOrInstBody {
    Param((String, (Type, Option<ast::Expr>))),
    Operands(Vec<(String, Type)>),
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
        .then_ignore(just(Token::FatArrow))
        .then(inline_expr())
        .map_with(|(start, value), e| EncodingArm {
            start,
            end: None,
            value,
            span: e.span(),
        });
    let range = num
        .clone()
        .then_ignore(just(Token::Range))
        .then(num)
        .then_ignore(just(Token::FatArrow))
        .then(inline_expr())
        .map_with(|((start, end), value), e| EncodingArm {
            start,
            end: Some(end),
            value,
            span: e.span(),
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

fn parameter<'src, I>()
-> impl Parser<'src, I, (String, (Type, Option<ast::Expr>)), extra::Err<Rich<'src, Token<'src>, Span>>>
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
-> impl Parser<'src, I, Vec<(String, Type)>, extra::Err<Rich<'src, Token<'src>, Span>>>
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
        .then_ignore(just(Token::FatArrow))
        .then_ignore(just(Token::LBrace))
        .then(reg_traits)
        .then_ignore(just(Token::RBrace))
        .map_with(|((name, alias), traits), e| {
            RegisterDef::Single(Register {
                name,
                alias,
                traits,
                subregisters: Vec::new(),
                span: e.span(),
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
                    "program_counter" => Some(RegisterTrait::ProgramCounter),
                    "global_pointer" => Some(RegisterTrait::GlobalPointer),
                    "thread_pointer" => Some(RegisterTrait::ThreadPointer),
                    "argument" => Some(RegisterTrait::Argument),
                    "return_value" => Some(RegisterTrait::ReturnValue),
                    "temporary" => Some(RegisterTrait::Temporary),
                    "saved" => Some(RegisterTrait::Saved),
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
        .then_ignore(just(Token::Range))
        .then(ident)
        .then(alias_pattern)
        .then_ignore(just(Token::FatArrow))
        .then_ignore(just(Token::LBrace))
        .then(reg_traits)
        .then_ignore(just(Token::RBrace))
        .map_with(|(((start, end), alias_pattern), traits), e| {
            RegisterDef::Range(RegisterRange {
                start,
                end,
                alias_pattern,
                traits,
                span: e.span(),
            })
        })
}

fn inline_expr<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    recursive(|expr| {
        fn builtin_from_ident(name: &str) -> Option<BuiltinFunction> {
            match name {
                "clamp" => Some(BuiltinFunction::Clamp),
                "extract" => Some(BuiltinFunction::Extract),
                "log2Ceil" => Some(BuiltinFunction::Log2Ceil),
                "sext" => Some(BuiltinFunction::SExt),
                "zext" => Some(BuiltinFunction::ZExt),
                "load" => Some(BuiltinFunction::Load),
                "store" => Some(BuiltinFunction::Store),
                _ => None,
            }
        }

        let ident = select! { Token::Identifier(i) => i.to_string() };
        let scope = just(Token::Colon).then(just(Token::Colon));

        let ident_or_path = ident
            .clone()
            .then(scope.ignore_then(ident.clone()).or_not())
            .map_with(|(base, member), e| {
                if let Some(member) = member {
                    Expr::Path(Path {
                        base,
                        remainder: vec![member],
                        span: e.span(),
                    })
                } else if let Some(b) = builtin_from_ident(&base) {
                    Expr::BuiltinFunction(b)
                } else {
                    Ident::new(base, e.span()).into()
                }
            });

        let literal_or_ident = choice((
            ident_or_path,
            select! { Token::Number(n) => n.to_string() }
                .map_with(|n, e| LitInt::new(n, e.span()).into()),
            select! { Token::StringLit(s) => s.to_string() }
                .map_with(|s, e| LitStr::new(s, e.span()).into()),
        ))
        .labelled("value");

        let num = select! {
          Token::Number(n) => n.parse::<u16>().unwrap(),
        };

        let ident = select! { Token::Identifier(i) => i.to_string() };

        let atom = literal_or_ident
            .or(expr
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen)))
            .boxed();

        let items = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>();

        // Postfix chain: field access, slice, index, then call
        #[derive(Clone)]
        enum PostfixOp {
            Field(String, Span),
            Slice(u16, u16, Span),
            Index(u16, Span),
            Call(Vec<Expr>, Span),
        }

        let postfix_op = choice((
            // field: .ident
            just(Token::Dot)
                .then(ident.clone())
                .map_with(|(_, b), e| PostfixOp::Field(b, e.span())),
            // slice: [start..end]
            num.clone()
                .then_ignore(just(Token::Range))
                .then(num.clone())
                .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .map_with(|(start, end), e| PostfixOp::Slice(start, end, e.span())),
            // index: [idx]
            num.clone()
                .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .map_with(|index, e| PostfixOp::Index(index, e.span())),
            // call: (args)
            items
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .map_with(|arguments, e| PostfixOp::Call(arguments, e.span())),
        ));

        let basic = atom
            .clone()
            .foldl_with(postfix_op.repeated(), |base, op, _e| match op {
                PostfixOp::Field(member, span) => Expr::Field(Field {
                    base: Box::new(base),
                    member,
                    span,
                }),
                PostfixOp::Slice(start, end, span) => Expr::Slice(Slice {
                    base: Box::new(base),
                    start,
                    end,
                    span,
                }),
                PostfixOp::Index(index, span) => Expr::IndexAccess(IndexAccess {
                    base: Box::new(base),
                    index,
                    span,
                }),
                PostfixOp::Call(arguments, span) => Expr::Call(Call {
                    callee: Box::new(base),
                    arguments,
                    span,
                }),
            });

        let op = just(Token::Asterisk)
            .to(BinOp::Mul)
            .or(just(Token::Tilde)
                .then(just(Token::ForwardSlash))
                .to(BinOp::UnsignedDiv))
            .or(just(Token::ForwardSlash).to(BinOp::Div));
        let product = basic
            .clone()
            .foldl_with(op.then(expr).repeated(), |a, (op, b), e| {
                let sp = e.span();
                Expr::Binary(Binary {
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                    op,
                    span: sp,
                })
            });

        let op = choice((
            just(Token::Plus).to(BinOp::Add),
            just(Token::Dash).to(BinOp::Sub),
            just(Token::Pipe).to(BinOp::BitwiseOr),
            just(Token::Ampersand).to(BinOp::BitwiseAnd),
            just(Token::Hat).to(BinOp::BitwiseXor),
            just(Token::LAngle)
                .then(just(Token::LAngle))
                .to(BinOp::ShiftLeftLogical),
            // Prefer the longer operator first: >>> (arith) before >> (logical)
            just(Token::RAngle)
                .then(just(Token::RAngle))
                .then(just(Token::RAngle))
                .to(BinOp::ShiftRightArithmetic),
            just(Token::RAngle)
                .then(just(Token::RAngle))
                .to(BinOp::ShiftRightLogical),
        ));

        let arith = product
            .clone()
            .foldl_with(op.then(product).repeated(), |a, (op, b), e| {
                let sp = e.span();
                Expr::Binary(Binary {
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                    op,
                    span: sp,
                })
            });

        let cmp_op = choice((
            just(Token::Equals)
                .then(just(Token::Equals))
                .to(BinOp::Equal),
            just(Token::Bang)
                .then(just(Token::Equals))
                .to(BinOp::NotEqual),
            just(Token::Tilde)
                .then(just(Token::LAngle))
                .then(just(Token::Equals))
                .to(BinOp::UnsignedLessThenEqual),
            just(Token::Tilde)
                .then(just(Token::RAngle))
                .then(just(Token::Equals))
                .to(BinOp::UnsignedGreaterThanEqual),
            just(Token::Tilde)
                .then(just(Token::LAngle))
                .to(BinOp::UnsignedLessThan),
            just(Token::Tilde)
                .then(just(Token::RAngle))
                .to(BinOp::UnsignedGreaterThan),
            just(Token::LAngle)
                .then(just(Token::Equals))
                .to(BinOp::LessThenEqual),
            just(Token::RAngle)
                .then(just(Token::Equals))
                .to(BinOp::GreaterThanEqual),
            just(Token::LAngle).to(BinOp::LessThan),
            just(Token::RAngle).to(BinOp::GreaterThan),
        ));

        arith
            .clone()
            .foldl_with(cmp_op.then(arith).repeated(), |a, (op, b), e| {
                let sp = e.span();
                Expr::Binary(Binary {
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                    op,
                    span: sp,
                })
            })
            .labelled("inline expression")
    })
}

fn expr<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, Token<'src>, Span>>>
where
    I: ValueInput<'src, Token = Token<'src>, Span = Span>,
{
    let ident = select! { Token::Identifier(i) => i.to_string() };
    let scope = just(Token::Colon).then(just(Token::Colon));
    let assign_target = ident
        .clone()
        .then(scope.ignore_then(ident).or_not())
        .map_with(|(base, member), e| {
            if let Some(member) = member {
                Expr::Path(Path {
                    base,
                    remainder: vec![member],
                    span: e.span(),
                })
            } else {
                Expr::Ident(Ident::new(base, e.span()))
            }
        })
        .boxed();

    recursive(|expr| {
        let assign = assign_target
            .clone()
            .then_ignore(just(Token::Equals))
            .then(expr.clone().or(inline_expr()))
            .map_with(|(dest, value), e| {
                Expr::Assign(Assign {
                    dest: Box::new(dest),
                    value: Box::new(value),
                    span: e.span(),
                })
            })
            .labelled("assignment");
        let stmt = expr.clone().or(assign).or(inline_expr()).boxed();

        let block = stmt
            .separated_by(just(Token::Semicolon))
            .collect::<Vec<_>>()
            .then(just(Token::Semicolon).or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map_with(|(stmts, trailing_semicolon), e| {
                let last_expr_return = trailing_semicolon.is_none() && !stmts.is_empty();
                Block {
                    stmts,
                    last_expr_return,
                    span: e.span(),
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
                .map_with(|((cond, a), b), e| {
                    Expr::If(If {
                        cond: Box::new(cond),
                        then: Box::new(a),
                        else_: b.map(Box::new),
                        span: e.span(),
                    })
                })
                .boxed()
        });

        block.clone().or(if_)
    })
}

fn type_<'src, I>() -> impl Parser<'src, I, Type, extra::Err<Rich<'src, Token<'src>, Span>>>
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
        just(Token::Identifier("String")).to(Type::String),
        just(Token::Identifier("Integer")).to(Type::Integer),
        bits,
        ident.map(Type::Struct),
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

    use crate::{
        ast::{BinOp, Expr},
        lexer::lexer,
    };

    use super::{inline_expr, isa_def};

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

    #[test]
    fn inline_expr_parses_less_equal() {
        let code = "a <= b";
        let (tokens, mut _errors) = lexer().parse(code).into_output_errors();
        let tokens = tokens.unwrap();
        let parsed = inline_expr().then(end()).parse(
            tokens
                .as_slice()
                .map((code.len()..code.len()).into(), |(t, s)| (t, s)),
        );
        let expr = parsed.output().unwrap().0.clone();
        match expr {
            Expr::Binary(bin) => assert_eq!(bin.op, BinOp::LessThenEqual),
            _ => panic!("Expected binary expression"),
        }
    }

    #[test]
    fn inline_expr_parses_not_equal() {
        let code = "a != b";
        let (tokens, mut _errors) = lexer().parse(code).into_output_errors();
        let tokens = tokens.unwrap();
        let parsed = inline_expr().then(end()).parse(
            tokens
                .as_slice()
                .map((code.len()..code.len()).into(), |(t, s)| (t, s)),
        );
        let expr = parsed.output().unwrap().0.clone();
        match expr {
            Expr::Binary(bin) => assert_eq!(bin.op, BinOp::NotEqual),
            _ => panic!("Expected binary expression"),
        }
    }

    #[test]
    fn inline_expr_parses_unsigned_less_equal() {
        let code = "a ~<= b";
        let (tokens, mut _errors) = lexer().parse(code).into_output_errors();
        let tokens = tokens.unwrap();
        let parsed = inline_expr().then(end()).parse(
            tokens
                .as_slice()
                .map((code.len()..code.len()).into(), |(t, s)| (t, s)),
        );
        let expr = parsed.output().unwrap().0.clone();
        match expr {
            Expr::Binary(bin) => assert_eq!(bin.op, BinOp::UnsignedLessThenEqual),
            _ => panic!("Expected binary expression"),
        }
    }
}
