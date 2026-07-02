//! A small C abstract syntax tree covering a C89/C99 subset: `int`/`void`
//! functions, local `int` variables, control flow (`if`/`else`, `while`,
//! `do`/`while`, `for`, `break`, `continue`), compound blocks, the usual
//! arithmetic/relational/logical operators and function calls. There are no
//! types beyond `int`/`void` and no pointers at the source level. Codegen
//! currently lowers only the original arithmetic subset; the rest is parsed
//! and stubbed.
//!
//! The tree is stored in core's [`PostOrderDag`], the same cache-friendly,
//! post-order layout the semantic-expression graph uses: node *kinds* live in a
//! flat vector while the variable-sized payload (names, literals, types) sits in
//! a sparse side table keyed by node id. Children always precede their parent,
//! so the root is the last node.

use tir::graph::{Dag, NodeId, PostOrderDag};

use crate::diagnostics::Span;

/// The AST: node payloads ([`AstNode`], kind + source span) live in the DAG's
/// dense vector, while the variable-sized leaf payload sits in its side table.
pub type Ast = PostOrderDag<AstNode, AstLeaf>;

/// A node's dense payload: its structural [`AstKind`] and the source [`Span`]
/// where the construct begins, used to point diagnostics at the offending code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstNode {
    pub kind: AstKind,
    pub span: Span,
}

impl AstNode {
    pub fn new(kind: AstKind, span: Span) -> Self {
        AstNode { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CType {
    Int,
    Void,
    Char,
    Const(Box<CType>),
    Pointer(Box<CType>),
}

/// The structural kind of an AST node. How its children are interpreted depends
/// solely on the kind; payload data lives in the matching [`AstLeaf`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstKind {
    /// Children: the translation unit's functions.
    TranslationUnit,
    /// Children: parameters.
    Prototype,
    /// Children: parameters, then body statements.
    Function,
    Param,
    VarArgs,
    /// Child: the optional initializer expression.
    Decl,
    /// Child: the assigned value expression.
    Assign,
    /// Child: the optional returned expression.
    Return,
    /// Children: the block's statements.
    Block,
    /// Child: the wrapped expression (an expression used as a statement).
    ExprStmt,
    /// Children: condition, then-branch, and an optional else-branch.
    If,
    /// Children: condition, body.
    While,
    /// Children: body, condition.
    DoWhile,
    /// Children: init, condition, step, body. Omitted clauses are [`AstKind::Empty`].
    For,
    Break,
    Continue,
    /// A placeholder for an omitted `for` clause or a null statement.
    Empty,
    /// Children: left-hand side, right-hand side.
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    LogAnd,
    LogOr,
    /// Child: the single operand.
    Neg,
    Not,
    /// Children: the argument expressions. Callee name lives in [`AstLeaf::Call`].
    Call,
    Var,
    String,
    Int,
}

/// Payload for the nodes that carry one. Indexed by node id through
/// [`Dag::get_leaf_data`]; structural nodes ([`AstKind::TranslationUnit`],
/// [`AstKind::Return`], the binary operators) have none.
#[derive(Debug, Clone, PartialEq)]
pub enum AstLeaf {
    Function { name: String, ret: CType },
    Param { name: String, ty: CType },
    Decl { name: String, ty: CType },
    Assign(String),
    Call(String),
    Var(String),
    String(String),
    Int(i64),
}

fn render_ctype(ty: &CType) -> String {
    match ty {
        CType::Int => "Int".to_string(),
        CType::Void => "Void".to_string(),
        CType::Char => "Char".to_string(),
        CType::Const(inner) => format!("Const({})", render_ctype(inner)),
        CType::Pointer(inner) => format!("Ptr({})", render_ctype(inner)),
    }
}

/// Render the tree as an indented outline, used by the `--stage ast` output.
pub fn render(ast: &Ast) -> String {
    let mut out = String::new();
    if let Some(root) = ast.root() {
        render_node(ast, root, 0, &mut out);
    }
    out
}

fn render_node(ast: &Ast, id: NodeId, depth: usize, out: &mut String) {
    use std::fmt::Write;

    let label = match ast.get_node(id).kind {
        AstKind::TranslationUnit => "TranslationUnit".to_string(),
        AstKind::Prototype => match ast.get_leaf_data(id) {
            Some(AstLeaf::Function { name, ret }) => {
                format!("Prototype {name:?} -> {}", render_ctype(ret))
            }
            _ => unreachable!(),
        },
        AstKind::Function => match ast.get_leaf_data(id) {
            Some(AstLeaf::Function { name, ret }) => {
                format!("Function {name:?} -> {}", render_ctype(ret))
            }
            _ => unreachable!(),
        },
        AstKind::Param => match ast.get_leaf_data(id) {
            Some(AstLeaf::Param { name, ty }) if name.is_empty() => {
                format!("Param _: {}", render_ctype(ty))
            }
            Some(AstLeaf::Param { name, ty }) => format!("Param {name:?}: {}", render_ctype(ty)),
            _ => unreachable!(),
        },
        AstKind::VarArgs => "VarArgs".to_string(),
        AstKind::Decl => match ast.get_leaf_data(id) {
            Some(AstLeaf::Decl { name, ty }) => format!("Decl {name:?}: {}", render_ctype(ty)),
            _ => unreachable!(),
        },
        AstKind::Assign => match ast.get_leaf_data(id) {
            Some(AstLeaf::Assign(name)) => format!("Assign {name:?}"),
            _ => unreachable!(),
        },
        AstKind::Return => "Return".to_string(),
        AstKind::Block => "Block".to_string(),
        AstKind::ExprStmt => "ExprStmt".to_string(),
        AstKind::If => "If".to_string(),
        AstKind::While => "While".to_string(),
        AstKind::DoWhile => "DoWhile".to_string(),
        AstKind::For => "For".to_string(),
        AstKind::Break => "Break".to_string(),
        AstKind::Continue => "Continue".to_string(),
        AstKind::Empty => "Empty".to_string(),
        AstKind::Add => "Add".to_string(),
        AstKind::Sub => "Sub".to_string(),
        AstKind::Mul => "Mul".to_string(),
        AstKind::Div => "Div".to_string(),
        AstKind::Mod => "Mod".to_string(),
        AstKind::Lt => "Lt".to_string(),
        AstKind::Gt => "Gt".to_string(),
        AstKind::Le => "Le".to_string(),
        AstKind::Ge => "Ge".to_string(),
        AstKind::Eq => "Eq".to_string(),
        AstKind::Ne => "Ne".to_string(),
        AstKind::LogAnd => "LogAnd".to_string(),
        AstKind::LogOr => "LogOr".to_string(),
        AstKind::Neg => "Neg".to_string(),
        AstKind::Not => "Not".to_string(),
        AstKind::Call => match ast.get_leaf_data(id) {
            Some(AstLeaf::Call(name)) => format!("Call {name:?}"),
            _ => unreachable!(),
        },
        AstKind::Var => match ast.get_leaf_data(id) {
            Some(AstLeaf::Var(name)) => format!("Var {name:?}"),
            _ => unreachable!(),
        },
        AstKind::String => match ast.get_leaf_data(id) {
            Some(AstLeaf::String(value)) => format!("String {value:?}"),
            _ => unreachable!(),
        },
        AstKind::Int => match ast.get_leaf_data(id) {
            Some(AstLeaf::Int(value)) => format!("Int {value}"),
            _ => unreachable!(),
        },
    };

    writeln!(out, "{:indent$}{label}", "", indent = depth * 2).unwrap();
    for child in ast.children(id) {
        render_node(ast, child, depth + 1, out);
    }
}
