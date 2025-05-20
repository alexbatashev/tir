use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum RegisterTrait {
    HardwiredZero,
    ReturnAddress,
    CallerSaved,
    CalleeSaved,
    StackPointer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Register {
    pub name: String,
    pub alias: Option<String>,
    pub traits: Vec<RegisterTrait>,
    pub subregisters: Vec<Register>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegisterRange {
    pub start: String,
    pub end: String,
    pub alias_pattern: Option<String>,
    pub traits: Vec<RegisterTrait>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegisterDef {
    Single(Register),
    Range(RegisterRange),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegisterClass {
    pub name: String,
    pub for_isas: Vec<String>,
    pub parameters: HashMap<String, Expr>,
    pub registers: Vec<RegisterDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IsaRequirement {
    Single(String),
    Any(Vec<String>),
    All(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Isa {
    pub name: String,
    pub requires: Option<IsaRequirement>,
    pub parameters: HashMap<String, Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub name: String,
    pub for_isas: Vec<String>,
    pub parent_template: Option<String>,
    pub params: HashMap<String, (Type, Option<Expr>)>,
    pub operands: HashMap<String, String>,
    pub encoding: Vec<EncodingArm>,
    pub asm: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub name: String,
    pub for_isas: Vec<String>,
    pub parent_template: Option<String>,
    pub params: HashMap<String, (Type, Option<Expr>)>,
    pub operands: HashMap<String, String>,
    pub encoding: Vec<EncodingArm>,
    pub asm: Option<Expr>,
    pub behavior: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodingArm {
    pub start: u16,
    pub end: Option<u16>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Isa(Isa),
    RegisterClass(RegisterClass),
    Template(Template),
    Instruction(Instruction),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    Integer,
    Bits(u16),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Str(LitStr),
    Int(LitInt),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LitStr {
    value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LitInt {
    value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub base: Box<Expr>,
    pub member: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct If {
    pub cond: Box<Expr>,
    pub then: Box<Expr>,
    pub else_: Option<Box<Expr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Expr>,
    pub last_expr_return: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub dest: String,
    pub value: Box<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binary {
    pub lhs: Box<Expr>,
    pub rhs: Box<Expr>,
    pub op: BinOp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub base: Box<Expr>,
    pub arguments: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Assign(Assign),
    Binary(Binary),
    Call(Call),
    Lit(Lit),
    Field(Field),
    Ident(Ident),
    If(If),
    Block(Block),
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub items: Vec<Item>,
}

impl LitInt {
    pub fn new(value: String) -> Self {
        Self { value }
    }
}

impl LitStr {
    pub fn new(value: String) -> Self {
        Self { value }
    }
}

impl Into<Expr> for LitInt {
    fn into(self) -> Expr {
        Expr::Lit(Lit::Int(self))
    }
}

impl Into<Expr> for LitStr {
    fn into(self) -> Expr {
        Expr::Lit(Lit::Str(self))
    }
}

impl Ident {
    pub fn new(name: String) -> Ident {
        Ident { name }
    }
}

impl Into<Expr> for Ident {
    fn into(self) -> Expr {
        Expr::Ident(self)
    }
}

impl Into<Expr> for Block {
    fn into(self) -> Expr {
        Expr::Block(self)
    }
}

impl Into<Expr> for If {
    fn into(self) -> Expr {
        Expr::If(self)
    }
}
