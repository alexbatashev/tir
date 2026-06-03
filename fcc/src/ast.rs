//! A tiny C abstract syntax tree — only the constructs needed to drive a
//! simple integer function (parameters, local `int` variables, arithmetic and
//! `return`) down to IR. There are no types beyond `int`/`void`, no control
//! flow, and no pointers at the source level.

#[derive(Debug, Clone, PartialEq)]
pub enum CType {
    Int,
    Void,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranslationUnit {
    pub functions: Vec<Function>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub ret: CType,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: CType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `int name = init;` or `int name;`
    Decl {
        name: String,
        ty: CType,
        init: Option<Expr>,
    },
    /// `name = value;`
    Assign { name: String, value: Expr },
    /// `return expr;` or `return;`
    Return(Option<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Var(String),
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
}
