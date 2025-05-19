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
    pub params: HashMap<String, (Type, Option<Expr>)>,
    pub operands: HashMap<String, String>,
    pub encoding: Vec<EncodingArm>,
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
pub struct Ident {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Lit),
    Field(Field),
    Ident(Ident),
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

impl Into<Expr> for LitInt {
    fn into(self) -> Expr {
        Expr::Lit(Lit::Int(self))
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
