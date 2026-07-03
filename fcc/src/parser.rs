//! A [`chumsky`]-based parser turning a token stream into the [`crate::ast`], in the
//! same style as the TMDL compiler's parser (combinators over a token slice
//! with `Rich` errors).
//!
//! Types are still limited to `int`/`void`, but the statement and expression
//! grammar covers a useful C89/C99 subset: `if`/`else`, `while`, `do`/`while`,
//! `for`, `break`, `continue`, compound blocks and expression statements;
//! arithmetic (`+ - * / %`), relational and equality operators, logical
//! `&& || !`, unary minus, parentheses and function calls.
//!
//! Nodes are appended straight into the [`Ast`] DAG carried as parser state.
//! Because combinators run bottom-up, every child is added before its parent,
//! which is exactly the post-order layout the DAG requires.

use chumsky::input::{MapExtra, ValueInput};
use chumsky::inspector::SimpleState;
use chumsky::prelude::*;

use tir::graph::{Dag, MutDag, NodeId};

use crate::ast::*;
use crate::diagnostics::{Diagnostic, FileId, UnexpectedEof, UnexpectedToken};
use crate::lexer::Token;

/// Index-based span over the token slice (we parse already-lexed tokens, so
/// byte offsets are not available — token indices are the natural span).
type Span = SimpleSpan<usize>;
type Extra<'src> = extra::Full<Rich<'src, Token, Span>, SimpleState<ParseState>, ()>;

/// Parser state: the tree under construction plus the byte span of every input
/// token, so each node can record where its construct starts in the source.
struct ParseState {
    ast: Ast,
    spans: Vec<crate::diagnostics::Span>,
}

impl ParseState {
    /// Append a node, spanning it at the byte position of token index `tok`
    /// (the first token of the construct being reduced).
    fn add(&mut self, kind: AstKind, tok: usize) -> NodeId {
        let span = self
            .spans
            .get(tok)
            .copied()
            .unwrap_or(crate::diagnostics::Span::new(FileId::default(), 0));
        self.ast.add_node(AstNode::new(kind, span))
    }
}

/// Parse a stream of tokens, each paired with its byte [`crate::diagnostics::Span`]
/// in the source. Whitespace tokens are dropped first; on failure each parser
/// error is turned into a [`Diagnostic`] whose label points back at the source.
pub fn parse(tokens: &[(Token, crate::diagnostics::Span)]) -> Result<Ast, Vec<Diagnostic>> {
    let mut filtered = Vec::with_capacity(tokens.len());
    let mut byte_spans = Vec::with_capacity(tokens.len());
    for (tok, span) in tokens {
        if !matches!(tok, Token::Whitespace(_) | Token::Comment(_)) {
            filtered.push(tok.clone());
            byte_spans.push(*span);
        }
    }

    let mut state = SimpleState(ParseState {
        ast: Ast::new(),
        spans: byte_spans.clone(),
    });
    let (out, errors) = translation_unit()
        .parse_with_state(filtered.as_slice(), &mut state)
        .into_output_errors();

    match out {
        Some(_) if errors.is_empty() => Ok(state.0.ast),
        _ => Err(errors
            .into_iter()
            .map(|e| rich_to_diagnostic(&e, &byte_spans))
            .collect()),
    }
}

/// Convert a chumsky [`Rich`] error (spanned over token indices) into a
/// [`Diagnostic`] spanned at the offending token's source position. An error
/// past the final token (`found` is `None`) is reported at the last token.
fn rich_to_diagnostic(
    err: &Rich<'_, Token, Span>,
    byte_spans: &[crate::diagnostics::Span],
) -> Diagnostic {
    let index = err.span().into_range().start;
    let span = byte_spans
        .get(index)
        .or_else(|| byte_spans.last())
        .copied()
        .unwrap_or(crate::diagnostics::Span::new(
            crate::diagnostics::FileId::default(),
            0,
        ));
    let reason = err.reason().to_string();

    if err.found().is_none() {
        UnexpectedEof::new(span, reason).into()
    } else {
        UnexpectedToken::new(span, reason).into()
    }
}

fn ctype<'src, I>() -> impl Parser<'src, I, CType, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let qualifier = choice((
        just(Token::KwConst).to(true),
        just(Token::KwRestrict).to(false),
        just(Token::KwVolatile).to(false),
    ));
    let builtin_atom = select! {
        Token::KwInt => CType::Int,
        Token::KwVoid => CType::Void,
        Token::KwChar => CType::Char,
        Token::KwLong => CType::Int,
        Token::KwShort => CType::Int,
        Token::KwSigned => CType::Int,
        Token::KwUnsigned => CType::Int,
    };
    let builtin = builtin_atom
        .repeated()
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|atoms| {
            if atoms.iter().any(|ty| matches!(ty, CType::Void)) {
                CType::Void
            } else if atoms.iter().any(|ty| matches!(ty, CType::Char)) {
                CType::Char
            } else {
                CType::Int
            }
        });
    let named = select! { Token::Identifier(name) => CType::Named(name) };
    let base = choice((builtin, named));
    let pointer = just(Token::Star)
        .ignore_then(qualifier.clone().repeated().ignored())
        .to(());

    qualifier
        .repeated()
        .collect::<Vec<_>>()
        .then(base)
        .then(pointer.repeated().collect::<Vec<_>>())
        .map(|((qualifiers, mut ty), pointers)| {
            if qualifiers.iter().any(|&is_const| is_const) {
                ty = CType::Const(Box::new(ty));
            }
            for _ in pointers {
                ty = CType::Pointer(Box::new(ty));
            }
            ty
        })
}

fn ident<'src, I>() -> impl Parser<'src, I, String, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    select! { Token::Identifier(name) => name }
}

fn expr<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    recursive(|expr| {
        let literal = select! { Token::IntegerLiteral(n) => n.to_i64() }.map_with(
            |n, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Int, tok);
                st.ast.set_leaf_data(id, AstLeaf::Int(n));
                id
            },
        );
        let string = select! { Token::StringLiteral(s) => s }.map_with(
            |s, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::String, tok);
                st.ast.set_leaf_data(id, AstLeaf::String(s));
                id
            },
        );

        // A call must be tried before a bare identifier so `f(x)` is not read as
        // the variable `f`.
        let call = ident()
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .collect::<Vec<NodeId>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen)),
            )
            .map_with(|(name, args), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Call, tok);
                st.ast.set_leaf_data(id, AstLeaf::Call(name));
                for arg in args {
                    st.ast.add_edge(id, arg);
                }
                id
            });

        let var = ident().map_with(|name, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            let st = &mut e.state().0;
            let id = st.add(AstKind::Var, tok);
            st.ast.set_leaf_data(id, AstLeaf::Var(name));
            id
        });

        let primary = choice((
            literal,
            string,
            call,
            var,
            expr.delimited_by(just(Token::LParen), just(Token::RParen)),
        ));

        // Prefix unary operators (`-`, `!`), applied right-to-left so the
        // innermost operator wraps the operand first.
        let unary = choice((
            just(Token::Minus).to(AstKind::Neg),
            just(Token::Bang).to(AstKind::Not),
        ))
        .repeated()
        .collect::<Vec<AstKind>>()
        .then(primary)
        .map_with(
            |(ops, operand), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                ops.into_iter()
                    .rev()
                    .fold(operand, |child, op| unary(st, op, child, tok))
            },
        );

        // Precedence ladder, tightest first. Every operator is left-associative.
        let product = binop(
            unary,
            choice((
                just(Token::Star).to(AstKind::Mul),
                just(Token::Slash).to(AstKind::Div),
                just(Token::Percent).to(AstKind::Mod),
            )),
        );
        let sum = binop(
            product,
            choice((
                just(Token::Plus).to(AstKind::Add),
                just(Token::Minus).to(AstKind::Sub),
            )),
        );
        let relational = binop(
            sum,
            choice((
                just(Token::Le).to(AstKind::Le),
                just(Token::Ge).to(AstKind::Ge),
                just(Token::Lt).to(AstKind::Lt),
                just(Token::Gt).to(AstKind::Gt),
            )),
        );
        let equality = binop(
            relational,
            choice((
                just(Token::EqEq).to(AstKind::Eq),
                just(Token::BangEq).to(AstKind::Ne),
            )),
        );
        let logical_and = binop(equality, just(Token::AmpAmp).to(AstKind::LogAnd));
        binop(logical_and, just(Token::PipePipe).to(AstKind::LogOr))
    })
}

/// A left-associative binary-operator level: a `child` operand followed by any
/// number of `op child` tails, folded into nested operator nodes. Operands are
/// already in the DAG by the time the fold runs, so each operator node is
/// appended after them.
fn binop<'src, I, C, O>(child: C, op: O) -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
    C: Parser<'src, I, NodeId, Extra<'src>> + Clone + 'src,
    O: Parser<'src, I, AstKind, Extra<'src>> + Clone + 'src,
{
    child
        .clone()
        .then(
            op.then(child)
                .repeated()
                .collect::<Vec<(AstKind, NodeId)>>(),
        )
        .map_with(
            |(first, rest), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                rest.into_iter()
                    .fold(first, |lhs, (op, rhs)| binary(st, op, lhs, rhs, tok))
            },
        )
        // Type-erase each precedence level. Without this the levels nest into a
        // single concrete combinator type whose drop-glue symbol grows to
        // megabytes and overflows the macOS linker's symbol-name limit.
        .boxed()
}

fn binary(st: &mut ParseState, op: AstKind, lhs: NodeId, rhs: NodeId, tok: usize) -> NodeId {
    let id = st.add(op, tok);
    st.ast.add_edge(id, lhs);
    st.ast.add_edge(id, rhs);
    id
}

fn unary(st: &mut ParseState, op: AstKind, operand: NodeId, tok: usize) -> NodeId {
    let id = st.add(op, tok);
    st.ast.add_edge(id, operand);
    id
}

/// Build an `int x = init` declaration node (without the trailing `;`, so the
/// same body serves both a declaration statement and a `for` init clause).
fn decl_body<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    ctype()
        .then(ident())
        .then(just(Token::Assign).ignore_then(expr()).or_not())
        .map_with(
            |((ty, name), init), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Decl, tok);
                st.ast.set_leaf_data(id, AstLeaf::Decl { name, ty });
                if let Some(init) = init {
                    st.ast.add_edge(id, init);
                }
                id
            },
        )
}

/// Build an `x = value` assignment node (without the trailing `;`).
fn assign_body<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    ident()
        .then_ignore(just(Token::Assign))
        .then(expr())
        .map_with(
            |(name, value), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Assign, tok);
                st.ast.set_leaf_data(id, AstLeaf::Assign(name));
                st.ast.add_edge(id, value);
                id
            },
        )
}

fn empty_node<'src, I>(e: &mut MapExtra<'src, '_, I, Extra<'src>>) -> NodeId
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let tok = e.span().start;
    e.state().0.add(AstKind::Empty, tok)
}

fn stmt<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    recursive(|stmt| {
        let semi = just(Token::Semicolon);

        let block = stmt
            .clone()
            .repeated()
            .collect::<Vec<NodeId>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map_with(|stmts, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Block, tok);
                for s in stmts {
                    st.ast.add_edge(id, s);
                }
                id
            });

        let ret = just(Token::KwReturn)
            .ignore_then(expr().or_not())
            .then_ignore(semi.clone())
            .map_with(|value, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::Return, tok);
                if let Some(value) = value {
                    st.ast.add_edge(id, value);
                }
                id
            });

        let decl = decl_body().then_ignore(semi.clone());
        let assign = assign_body().then_ignore(semi.clone());

        let cond = expr().delimited_by(just(Token::LParen), just(Token::RParen));

        let if_stmt = just(Token::KwIf)
            .ignore_then(cond.clone())
            .then(stmt.clone())
            .then(just(Token::KwElse).ignore_then(stmt.clone()).or_not())
            .map_with(
                |((c, then), els), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                    let tok = e.span().start;
                    let st = &mut e.state().0;
                    let id = st.add(AstKind::If, tok);
                    st.ast.add_edge(id, c);
                    st.ast.add_edge(id, then);
                    if let Some(els) = els {
                        st.ast.add_edge(id, els);
                    }
                    id
                },
            );

        let while_stmt = just(Token::KwWhile)
            .ignore_then(cond.clone())
            .then(stmt.clone())
            .map_with(|(c, body), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::While, tok);
                st.ast.add_edge(id, c);
                st.ast.add_edge(id, body);
                id
            });

        let do_while = just(Token::KwDo)
            .ignore_then(stmt.clone())
            .then_ignore(just(Token::KwWhile))
            .then(cond.clone())
            .then_ignore(semi.clone())
            .map_with(|(body, c), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::DoWhile, tok);
                st.ast.add_edge(id, body);
                st.ast.add_edge(id, c);
                id
            });

        // Each `for` clause may be omitted; an omitted clause becomes an
        // `Empty` node so the node always has exactly four children.
        let for_init = choice((decl_body(), assign_body()))
            .or_not()
            .map_with(|c, e| c.unwrap_or_else(|| empty_node(e)));
        let for_cond = expr()
            .or_not()
            .map_with(|c, e| c.unwrap_or_else(|| empty_node(e)));
        let for_step = choice((assign_body(), expr()))
            .or_not()
            .map_with(|c, e| c.unwrap_or_else(|| empty_node(e)));

        let for_stmt = just(Token::KwFor)
            .ignore_then(
                for_init
                    .then_ignore(semi.clone())
                    .then(for_cond)
                    .then_ignore(semi.clone())
                    .then(for_step)
                    .delimited_by(just(Token::LParen), just(Token::RParen)),
            )
            .then(stmt.clone())
            .map_with(
                |(((init, c), step), body), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                    let tok = e.span().start;
                    let st = &mut e.state().0;
                    let id = st.add(AstKind::For, tok);
                    st.ast.add_edge(id, init);
                    st.ast.add_edge(id, c);
                    st.ast.add_edge(id, step);
                    st.ast.add_edge(id, body);
                    id
                },
            );

        let break_stmt = just(Token::KwBreak).then_ignore(semi.clone()).map_with(
            |_, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                e.state().0.add(AstKind::Break, tok)
            },
        );
        let continue_stmt = just(Token::KwContinue).then_ignore(semi.clone()).map_with(
            |_, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                e.state().0.add(AstKind::Continue, tok)
            },
        );

        let null_stmt = semi
            .clone()
            .map_with(|_, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                e.state().0.add(AstKind::Empty, tok)
            });

        let expr_stmt = expr().then_ignore(semi).map_with(
            |value, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
                let tok = e.span().start;
                let st = &mut e.state().0;
                let id = st.add(AstKind::ExprStmt, tok);
                st.ast.add_edge(id, value);
                id
            },
        );

        // Declarations start with a type keyword and control flow with its own
        // keyword, so they are unambiguous. An assignment is tried before an
        // expression statement because the latter would also accept the left
        // operand of an assignment on its own.
        choice((
            block,
            decl,
            ret,
            if_stmt,
            while_stmt,
            do_while,
            for_stmt,
            break_stmt,
            continue_stmt,
            null_stmt,
            assign,
            expr_stmt,
        ))
    })
}

fn function<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let param = ctype().then(ident().or_not()).map_with(
        |(ty, name), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            let st = &mut e.state().0;
            let id = st.add(AstKind::Param, tok);
            st.ast.set_leaf_data(
                id,
                AstLeaf::Param {
                    name: name.unwrap_or_default(),
                    ty,
                },
            );
            id
        },
    );
    let varargs =
        just(Token::Ellipsis).map_with(|_, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            e.state().0.add(AstKind::VarArgs, tok)
        });

    let params = choice((param, varargs))
        .separated_by(just(Token::Comma))
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen));
    let storage = choice((
        just(Token::KwExtern).ignored(),
        just(Token::KwStatic).ignored(),
        just(Token::KwInline).ignored(),
    ))
    .repeated()
    .ignored();

    let body = stmt()
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace));

    let header = storage.ignore_then(ctype().then(ident()).then(params));
    let definition = header.clone().then(body).map_with(
        |(((ret, name), params), body), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            let st = &mut e.state().0;
            let id = st.add(AstKind::Function, tok);
            st.ast.set_leaf_data(id, AstLeaf::Function { name, ret });
            let params = params
                .into_iter()
                .filter(|&param| !is_void_param(&st.ast, param))
                .collect::<Vec<_>>();
            for child in params.into_iter().chain(body) {
                st.ast.add_edge(id, child);
            }
            id
        },
    );
    let prototype = header.then_ignore(just(Token::Semicolon)).map_with(
        |((ret, name), params), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            let st = &mut e.state().0;
            let id = st.add(AstKind::Prototype, tok);
            st.ast.set_leaf_data(id, AstLeaf::Function { name, ret });
            let params = params
                .into_iter()
                .filter(|&param| !is_void_param(&st.ast, param))
                .collect::<Vec<_>>();
            for param in params {
                st.ast.add_edge(id, param);
            }
            id
        },
    );

    choice((definition, prototype))
}

fn is_void_param(ast: &Ast, param: NodeId) -> bool {
    matches!(
        ast.get_leaf_data(param),
        Some(AstLeaf::Param {
            name,
            ty: CType::Void,
        }) if name.is_empty()
    )
}

struct DeclParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    attrs: Vec<String>,
}

struct DeclSpecs {
    ty: CType,
    storage: Vec<Token>,
    record: Option<NodeId>,
}

struct Declarator {
    name: String,
    ty: CType,
}

impl<'a> DeclParser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            pos: 0,
            attrs: Vec::new(),
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, tok: &Token) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Token) -> Result<(), String> {
        self.eat(tok)
            .then_some(())
            .ok_or_else(|| format!("expected {tok}"))
    }

    fn is_done(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn parse_specs(&mut self, st: &mut ParseState, tok: usize) -> Result<DeclSpecs, String> {
        let mut storage = Vec::new();
        let mut qualifiers = Vec::new();
        let mut spec_tokens = Vec::new();
        let mut record = None;
        let mut ty = None;

        loop {
            match self.peek() {
                Some(Token::KwTypedef | Token::KwExtern | Token::KwStatic | Token::KwInline) => {
                    storage.push(self.next().unwrap());
                }
                Some(Token::KwConst | Token::KwVolatile | Token::KwRestrict) => {
                    qualifiers.push(self.next().unwrap());
                }
                Some(Token::KwStruct) => {
                    self.next();
                    let (record_ty, record_node) =
                        self.parse_record(st, tok, RecordKind::Struct)?;
                    ty = Some(record_ty);
                    record = record_node;
                    break;
                }
                Some(Token::KwUnion) => {
                    self.next();
                    let (record_ty, record_node) = self.parse_record(st, tok, RecordKind::Union)?;
                    ty = Some(record_ty);
                    record = record_node;
                    break;
                }
                Some(Token::KwEnum) => {
                    self.next();
                    let name = match self.peek() {
                        Some(Token::Identifier(_)) => match self.next().unwrap() {
                            Token::Identifier(name) => Some(name),
                            _ => unreachable!(),
                        },
                        _ => None,
                    };
                    if self.eat(&Token::LBrace) {
                        self.skip_balanced(Token::LBrace, Token::RBrace)?;
                    }
                    ty = Some(CType::Enum(name));
                    break;
                }
                Some(
                    Token::KwVoid
                    | Token::KwBool
                    | Token::KwChar
                    | Token::KwShort
                    | Token::KwInt
                    | Token::KwLong
                    | Token::KwSigned
                    | Token::KwUnsigned
                    | Token::KwFloat
                    | Token::KwDouble,
                ) => spec_tokens.push(self.next().unwrap()),
                Some(Token::Identifier(name)) if is_decl_attr_name(name) => {
                    let attr = self.parse_attr()?;
                    self.attrs.push(attr);
                }
                Some(Token::Identifier(_)) if spec_tokens.is_empty() && ty.is_none() => {
                    if let Token::Identifier(name) = self.next().unwrap() {
                        ty = Some(CType::Named(name));
                    }
                    break;
                }
                _ => break,
            }
        }

        if ty.is_none() && !spec_tokens.is_empty() {
            ty = Some(builtin_type(&spec_tokens));
        }

        let mut ty = ty.ok_or_else(|| "expected declaration type".to_string())?;
        for qualifier in qualifiers.into_iter().rev() {
            ty = match qualifier {
                Token::KwConst => CType::Const(Box::new(ty)),
                Token::KwVolatile => CType::Volatile(Box::new(ty)),
                Token::KwRestrict => CType::Restrict(Box::new(ty)),
                _ => ty,
            };
        }
        if !self.attrs.is_empty() {
            ty = CType::Attributed(Box::new(ty), std::mem::take(&mut self.attrs));
        }
        Ok(DeclSpecs {
            ty,
            storage,
            record,
        })
    }

    fn parse_record(
        &mut self,
        st: &mut ParseState,
        tok: usize,
        kind: RecordKind,
    ) -> Result<(CType, Option<NodeId>), String> {
        let name = match self.peek() {
            Some(Token::Identifier(_)) => match self.next().unwrap() {
                Token::Identifier(name) => Some(name),
                _ => unreachable!(),
            },
            _ => None,
        };
        let mut record = None;
        if self.eat(&Token::LBrace) {
            let mut fields = Vec::new();
            while !self.eat(&Token::RBrace) {
                if self.is_done() {
                    return Err("unterminated record declaration".to_string());
                }
                fields.extend(self.parse_field_decl(st, tok)?);
            }
            let id = st.add(AstKind::RecordDecl, tok);
            st.ast.set_leaf_data(
                id,
                AstLeaf::Record {
                    kind,
                    name: name.clone(),
                },
            );
            for field in fields {
                st.ast.add_edge(id, field);
            }
            record = Some(id);
        }
        Ok((CType::Record(kind, name), record))
    }

    fn parse_field_decl(&mut self, st: &mut ParseState, tok: usize) -> Result<Vec<NodeId>, String> {
        let specs = self.parse_specs(st, tok)?;
        let mut fields = Vec::new();
        loop {
            self.consume_attrs()?;
            let mut decl = self.parse_declarator(specs.ty.clone())?;
            self.consume_attrs()?;
            decl.ty = self.take_attrs(decl.ty);
            self.consume_bitfield()?;
            let id = st.add(AstKind::Field, tok);
            st.ast.set_leaf_data(
                id,
                AstLeaf::Field {
                    name: decl.name,
                    ty: decl.ty,
                },
            );
            fields.push(id);
            if self.eat(&Token::Comma) {
                continue;
            }
            self.expect(&Token::Semicolon)?;
            break;
        }
        Ok(fields)
    }

    fn parse_declarator(&mut self, mut base: CType) -> Result<Declarator, String> {
        while self.eat(&Token::Star) {
            let attrs = self.consume_pointer_attrs()?;
            base = CType::Pointer(Box::new(base));
            if !attrs.is_empty() {
                base = CType::Attributed(Box::new(base), attrs);
            }
        }

        let mut decl = if self.eat(&Token::LParen) {
            if self.eat(&Token::Star) {
                let attrs = self.consume_pointer_attrs()?;
                let name = self.parse_name()?;
                self.expect(&Token::RParen)?;
                let ty = if self.eat(&Token::LParen) {
                    let (params, varargs) = self.parse_param_list()?;
                    CType::Pointer(Box::new(CType::Function {
                        ret: Box::new(base),
                        params,
                        varargs,
                    }))
                } else {
                    CType::Pointer(Box::new(base))
                };
                let ty = if attrs.is_empty() {
                    ty
                } else {
                    CType::Attributed(Box::new(ty), attrs)
                };
                Declarator { name, ty }
            } else {
                let decl = self.parse_declarator(base)?;
                self.expect(&Token::RParen)?;
                decl
            }
        } else {
            Declarator {
                name: self.parse_name()?,
                ty: base,
            }
        };

        loop {
            if self.eat(&Token::LBracket) {
                let len = self.collect_until_matching(Token::LBracket, Token::RBracket)?;
                let len = (!len.is_empty()).then_some(tokens_text(&len));
                decl.ty = CType::Array(Box::new(decl.ty), len);
            } else if self.eat(&Token::LParen) {
                let (params, varargs) = self.parse_param_list()?;
                decl.ty = CType::Function {
                    ret: Box::new(decl.ty),
                    params,
                    varargs,
                };
            } else {
                break;
            }
        }

        Ok(decl)
    }

    fn parse_name(&mut self) -> Result<String, String> {
        self.consume_attrs()?;
        match self.next() {
            Some(Token::Identifier(name)) => Ok(name),
            Some(tok) => Err(format!("expected declarator name, found {tok}")),
            None => Err("expected declarator name".to_string()),
        }
    }

    fn parse_param_list(&mut self) -> Result<(Vec<CParam>, bool), String> {
        let mut params = Vec::new();
        let mut varargs = false;
        if self.eat(&Token::RParen) {
            return Ok((params, varargs));
        }
        loop {
            if self.eat(&Token::Ellipsis) {
                varargs = true;
            } else {
                let specs = self.parse_specs_for_param()?;
                let param = if matches!(self.peek(), Some(Token::Comma | Token::RParen)) {
                    CParam {
                        name: String::new(),
                        ty: specs,
                    }
                } else {
                    let pos = self.pos;
                    let attrs = self.attrs.clone();
                    let decl = match self.parse_declarator(specs.clone()) {
                        Ok(decl) => decl,
                        Err(_) => {
                            self.pos = pos;
                            self.attrs = attrs;
                            self.parse_abstract_declarator(specs)?
                        }
                    };
                    CParam {
                        name: decl.name,
                        ty: decl.ty,
                    }
                };
                if !matches!(param.ty, CType::Void) || !param.name.is_empty() {
                    params.push(param);
                }
            }
            if self.eat(&Token::Comma) {
                continue;
            }
            self.expect(&Token::RParen)?;
            break;
        }
        Ok((params, varargs))
    }

    fn parse_abstract_declarator(&mut self, mut base: CType) -> Result<Declarator, String> {
        while self.eat(&Token::Star) {
            let attrs = self.consume_pointer_attrs()?;
            base = CType::Pointer(Box::new(base));
            if !attrs.is_empty() {
                base = CType::Attributed(Box::new(base), attrs);
            }
        }
        self.consume_attrs()?;

        while self.eat(&Token::LBracket) {
            let len = self.collect_until_matching(Token::LBracket, Token::RBracket)?;
            let len = (!len.is_empty()).then_some(tokens_text(&len));
            base = CType::Array(Box::new(base), len);
        }

        if !matches!(self.peek(), Some(Token::Comma | Token::RParen)) {
            return Err("expected abstract declarator".to_string());
        }
        Ok(Declarator {
            name: String::new(),
            ty: base,
        })
    }

    fn parse_specs_for_param(&mut self) -> Result<CType, String> {
        let mut scratch = ParseState {
            ast: Ast::new(),
            spans: Vec::new(),
        };
        self.parse_specs(&mut scratch, 0).map(|specs| specs.ty)
    }

    fn consume_bitfield(&mut self) -> Result<(), String> {
        if self.eat(&Token::Colon) {
            while !matches!(self.peek(), Some(Token::Comma | Token::Semicolon) | None) {
                self.next();
            }
        }
        Ok(())
    }

    fn consume_pointer_attrs(&mut self) -> Result<Vec<String>, String> {
        let mut attrs = Vec::new();
        loop {
            match self.peek() {
                Some(Token::KwConst | Token::KwVolatile | Token::KwRestrict) => {
                    attrs.push(self.next().unwrap().to_string());
                }
                Some(Token::Identifier(name)) if is_decl_attr_name(name) => {
                    attrs.push(self.parse_attr()?);
                }
                _ => break,
            }
        }
        Ok(attrs)
    }

    fn consume_attrs(&mut self) -> Result<(), String> {
        while matches!(self.peek(), Some(Token::Identifier(name)) if is_decl_attr_name(name)) {
            let attr = self.parse_attr()?;
            self.attrs.push(attr);
        }
        Ok(())
    }

    fn take_attrs(&mut self, ty: CType) -> CType {
        if self.attrs.is_empty() {
            ty
        } else {
            CType::Attributed(Box::new(ty), std::mem::take(&mut self.attrs))
        }
    }

    fn parse_attr(&mut self) -> Result<String, String> {
        let name = match self.next() {
            Some(Token::Identifier(name)) => name,
            _ => unreachable!(),
        };
        let name = match name.as_str() {
            "__restrict" | "__restrict__" => "restrict".to_string(),
            _ => name,
        };
        if self.eat(&Token::LParen) {
            let args = self.collect_until_matching(Token::LParen, Token::RParen)?;
            Ok(format!("{name}({})", tokens_text(&args)))
        } else {
            Ok(name)
        }
    }

    fn skip_balanced(&mut self, open: Token, close: Token) -> Result<(), String> {
        let mut depth = 1usize;
        while let Some(tok) = self.next() {
            if tok == open {
                depth += 1;
            } else if tok == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
        }
        Err(format!("expected {close}"))
    }

    fn collect_until_matching(&mut self, open: Token, close: Token) -> Result<Vec<Token>, String> {
        let mut depth = 1usize;
        let mut out = Vec::new();
        while let Some(tok) = self.next() {
            if tok == open {
                depth += 1;
                out.push(tok);
            } else if tok == close {
                depth -= 1;
                if depth == 0 {
                    return Ok(out);
                }
                out.push(tok);
            } else {
                out.push(tok);
            }
        }
        Err(format!("expected {close}"))
    }
}

fn builtin_type(tokens: &[Token]) -> CType {
    let text = tokens_text(tokens);
    match text.as_str() {
        "void" => CType::Void,
        "char" => CType::Char,
        "int" | "signed" | "signed int" => CType::Int,
        "bool" => CType::Bool,
        "float" => CType::Float,
        "double" => CType::Double,
        _ => CType::Builtin(text),
    }
}

fn is_decl_attr_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        name,
        "_Nullable"
            | "_Nonnull"
            | "_Null_unspecified"
            | "__restrict"
            | "__restrict__"
            | "__THROW"
            | "__THROWNL"
            | "__wur"
            | "__nonnull"
            | "__attribute_malloc__"
            | "__attr_dealloc"
            | "__COLD"
            | "__fortified_attr_access"
            | "__attribute__"
            | "__asm"
            | "__asm__"
            | "__swift_nonisolated_unsafe"
            | "__swift_unavailable"
            | "__ptr"
    ) || name.starts_with("__")
        && (lower.contains("like")
            || lower.contains("alias")
            || lower.contains("availability")
            || lower.contains("deprecated"))
        || name.starts_with("_LIBC_")
}

fn tokens_text(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn add_param_node(st: &mut ParseState, tok: usize, mut param: CParam) -> NodeId {
    let id = st.add(AstKind::Param, tok);
    param.name.clear();
    st.ast.set_leaf_data(
        id,
        AstLeaf::Param {
            name: param.name,
            ty: param.ty,
        },
    );
    id
}

fn add_varargs_node(st: &mut ParseState, tok: usize) -> NodeId {
    st.add(AstKind::VarArgs, tok)
}

fn split_function_type(ty: CType) -> Result<(CType, Vec<CParam>, bool), CType> {
    match ty {
        CType::Function {
            ret,
            params,
            varargs,
        } => Ok((*ret, params, varargs)),
        CType::Attributed(inner, attrs) => match *inner {
            CType::Function {
                ret,
                params,
                varargs,
            } => Ok((CType::Attributed(ret, attrs), params, varargs)),
            other => Err(CType::Attributed(Box::new(other), attrs)),
        },
        other => Err(other),
    }
}

fn top_level_attr<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let group = recursive(|group| {
        let atom = any()
            .filter(|tok: &Token| !matches!(tok, Token::LParen | Token::RParen))
            .map(|tok| vec![tok]);
        choice((
            group
                .repeated()
                .collect::<Vec<Vec<Token>>>()
                .map(|parts| {
                    let mut toks = vec![Token::LParen];
                    toks.extend(parts.into_iter().flatten());
                    toks.push(Token::RParen);
                    toks
                })
                .delimited_by(just(Token::LParen), just(Token::RParen)),
            atom,
        ))
    });

    select! { Token::Identifier(name) => name }
        .try_map(|name, span| {
            is_decl_attr_name(&name)
                .then_some(name)
                .ok_or_else(|| Rich::custom(span, "expected top-level attribute"))
        })
        .then(
            group
                .repeated()
                .collect::<Vec<Vec<Token>>>()
                .delimited_by(just(Token::LParen), just(Token::RParen)),
        )
        .map_with(|(name, args), e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
            let tok = e.span().start;
            let st = &mut e.state().0;
            let id = st.add(AstKind::Attribute, tok);
            let args = args.into_iter().flatten().collect::<Vec<_>>();
            st.ast.set_leaf_data(
                id,
                AstLeaf::Attribute(format!("{name}({})", tokens_text(&args))),
            );
            id
        })
}

fn top_level_marker<'src, I>() -> impl Parser<'src, I, (), Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    select! { Token::Identifier(name) => name }
        .try_map(|name, span| {
            is_top_level_marker(&name)
                .then_some(())
                .ok_or_else(|| Rich::custom(span, "expected top-level marker"))
        })
        .then_ignore(just(Token::Semicolon).or_not())
}

fn parse_external_tokens(
    st: &mut ParseState,
    tok: usize,
    tokens: &[Token],
) -> Result<NodeId, String> {
    let mut parser = DeclParser::new(tokens);
    let specs = parser.parse_specs(st, tok)?;
    let is_typedef = specs
        .storage
        .iter()
        .any(|tok| matches!(tok, Token::KwTypedef));
    let is_extern = specs
        .storage
        .iter()
        .any(|tok| matches!(tok, Token::KwExtern));
    let mut nodes = Vec::new();
    if let Some(record) = specs.record {
        nodes.push(record);
    }

    if parser.is_done() {
        if nodes.len() == 1 {
            return Ok(nodes[0]);
        }
        if let CType::Record(kind, name) = specs.ty {
            let id = st.add(AstKind::RecordDecl, tok);
            st.ast.set_leaf_data(id, AstLeaf::Record { kind, name });
            return Ok(id);
        }
        return Err("record declaration has no declarator".to_string());
    }

    loop {
        parser.consume_attrs()?;
        let mut decl = parser.parse_declarator(specs.ty.clone())?;
        parser.consume_attrs()?;
        decl.ty = parser.take_attrs(decl.ty);
        let ty = decl.ty;
        if !is_typedef && let Ok((ret, params, varargs)) = split_function_type(ty.clone()) {
            let params = params
                .into_iter()
                .map(|param| add_param_node(st, tok, param))
                .collect::<Vec<_>>();
            let varargs = varargs.then(|| add_varargs_node(st, tok));
            let id = st.add(AstKind::Prototype, tok);
            st.ast.set_leaf_data(
                id,
                AstLeaf::Function {
                    name: decl.name,
                    ret,
                },
            );
            for param in params {
                st.ast.add_edge(id, param);
            }
            if let Some(varargs) = varargs {
                st.ast.add_edge(id, varargs);
            }
            nodes.push(id);
        } else {
            match ty {
                ty if is_typedef => {
                    let id = st.add(AstKind::Typedef, tok);
                    st.ast.set_leaf_data(
                        id,
                        AstLeaf::Typedef {
                            name: decl.name,
                            ty,
                        },
                    );
                    nodes.push(id);
                }
                ty if is_extern => {
                    let id = st.add(AstKind::Global, tok);
                    st.ast.set_leaf_data(
                        id,
                        AstLeaf::Global {
                            name: decl.name,
                            ty,
                        },
                    );
                    nodes.push(id);
                }
                _ => return Err("unsupported top-level declaration".to_string()),
            }
        }

        if parser.eat(&Token::Comma) {
            continue;
        }
        if parser.is_done() {
            break;
        }
        return Err(format!(
            "unexpected token in declaration: {}",
            parser.peek().unwrap()
        ));
    }

    if nodes.len() == 1 {
        Ok(nodes[0])
    } else {
        let group = st.add(AstKind::DeclGroup, tok);
        for node in nodes {
            st.ast.add_edge(group, node);
        }
        Ok(group)
    }
}

fn external_decl<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>> + Clone
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    let group = recursive(|group| {
        let atom = any()
            .filter(|tok: &Token| {
                !matches!(
                    tok,
                    Token::LBrace
                        | Token::RBrace
                        | Token::LParen
                        | Token::RParen
                        | Token::LBracket
                        | Token::RBracket
                )
            })
            .map(|tok| vec![tok]);
        choice((
            group
                .clone()
                .repeated()
                .collect::<Vec<Vec<Token>>>()
                .map(|parts| {
                    let mut toks = vec![Token::LBrace];
                    toks.extend(parts.into_iter().flatten());
                    toks.push(Token::RBrace);
                    toks
                })
                .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            group
                .clone()
                .repeated()
                .collect::<Vec<Vec<Token>>>()
                .map(|parts| {
                    let mut toks = vec![Token::LParen];
                    toks.extend(parts.into_iter().flatten());
                    toks.push(Token::RParen);
                    toks
                })
                .delimited_by(just(Token::LParen), just(Token::RParen)),
            group
                .repeated()
                .collect::<Vec<Vec<Token>>>()
                .map(|parts| {
                    let mut toks = vec![Token::LBracket];
                    toks.extend(parts.into_iter().flatten());
                    toks.push(Token::RBracket);
                    toks
                })
                .delimited_by(just(Token::LBracket), just(Token::RBracket)),
            atom,
        ))
    });
    let outer_atom = any()
        .filter(|tok: &Token| {
            !matches!(
                tok,
                Token::Semicolon
                    | Token::LBrace
                    | Token::RBrace
                    | Token::LParen
                    | Token::RParen
                    | Token::LBracket
                    | Token::RBracket
            )
        })
        .map(|tok| vec![tok]);
    choice((
        group
            .clone()
            .repeated()
            .collect::<Vec<Vec<Token>>>()
            .map(|parts| {
                let mut toks = vec![Token::LBrace];
                toks.extend(parts.into_iter().flatten());
                toks.push(Token::RBrace);
                toks
            })
            .delimited_by(just(Token::LBrace), just(Token::RBrace)),
        group
            .clone()
            .repeated()
            .collect::<Vec<Vec<Token>>>()
            .map(|parts| {
                let mut toks = vec![Token::LParen];
                toks.extend(parts.into_iter().flatten());
                toks.push(Token::RParen);
                toks
            })
            .delimited_by(just(Token::LParen), just(Token::RParen)),
        group
            .repeated()
            .collect::<Vec<Vec<Token>>>()
            .map(|parts| {
                let mut toks = vec![Token::LBracket];
                toks.extend(parts.into_iter().flatten());
                toks.push(Token::RBracket);
                toks
            })
            .delimited_by(just(Token::LBracket), just(Token::RBracket)),
        outer_atom,
    ))
    .repeated()
    .collect::<Vec<Vec<Token>>>()
    .then_ignore(just(Token::Semicolon))
    .try_map_with(|parts, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
        let tok = e.span().start;
        let tokens = parts.into_iter().flatten().collect::<Vec<_>>();
        let st = &mut e.state().0;
        parse_external_tokens(st, tok, &tokens).map_err(|msg| Rich::custom(e.span(), msg))
    })
}

fn translation_unit<'src, I>() -> impl Parser<'src, I, NodeId, Extra<'src>>
where
    I: ValueInput<'src, Token = Token, Span = Span>,
{
    choice((
        top_level_marker().to(None),
        top_level_attr().map(Some),
        external_decl().map(Some),
        function().map(Some),
    ))
    .repeated()
    .collect::<Vec<_>>()
    .then_ignore(end())
    .map_with(|functions, e: &mut MapExtra<'src, '_, I, Extra<'src>>| {
        let tok = e.span().start;
        let st = &mut e.state().0;
        let id = st.add(AstKind::TranslationUnit, tok);
        for item in functions.into_iter().flatten() {
            st.ast.add_edge(id, item);
        }
        id
    })
}

fn is_top_level_marker(name: &str) -> bool {
    matches!(name, "__BEGIN_DECLS" | "__END_DECLS")
}

#[cfg(test)]
mod tests {
    use logos::Logos;

    use super::parse;
    use crate::diagnostics::{Code, Span as ByteSpan, intern_file};
    use crate::lexer::Token;

    fn lex(src: &str) -> Vec<(Token, ByteSpan)> {
        let file = intern_file("<parser-test>", src);
        Token::lexer(src)
            .spanned()
            .map(|(r, span)| (r.unwrap(), ByteSpan::new(file, span.start)))
            .collect()
    }

    #[test]
    fn accepts_a_well_formed_function() {
        assert!(parse(&lex("int main(void) { return 0; }")).is_ok());
    }

    fn errors(src: &str) -> Vec<Code> {
        match parse(&lex(src)) {
            Ok(_) => panic!("expected parse to fail for {src:?}"),
            Err(diags) => diags.iter().map(|d| d.code()).collect(),
        }
    }

    #[test]
    fn missing_semicolon_is_rejected() {
        assert!(!errors("int main(void) { return 0 }").is_empty());
    }

    #[test]
    fn missing_closing_brace_is_unexpected_eof() {
        assert!(errors("int main(void) { return 0;").contains(&Code::UnexpectedEof));
    }
}
