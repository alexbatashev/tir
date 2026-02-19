use crate::utils::StableHashMap;
use crate::{Span, Type};
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum RegisterTrait {
    HardwiredZero,
    ReturnAddress,
    CallerSaved,
    CalleeSaved,
    StackPointer,
    ProgramCounter,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Register {
    pub name: String,
    pub alias: Option<String>,
    pub traits: Vec<RegisterTrait>,
    pub subregisters: Vec<Register>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegisterRange {
    pub start: String,
    pub end: String,
    pub alias_pattern: Option<String>,
    pub traits: Vec<RegisterTrait>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum RegisterDef {
    Single(Register),
    Range(RegisterRange),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegisterClass {
    pub name: String,
    pub for_isas: Vec<String>,
    #[serde(serialize_with = "serialize_params")]
    pub parameters: StableHashMap<String, (Type, Option<Expr>)>,
    pub registers: Vec<RegisterDef>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterNameTables {
    pub parse_names: Vec<(String, u16)>,
    pub isa_names: Vec<(u16, String)>,
    pub abi_names: Vec<(u16, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum IsaRequirement {
    Single(String),
    Any(Vec<String>),
    All(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Isa {
    pub name: String,
    pub requires: Option<IsaRequirement>,
    #[serde(serialize_with = "serialize_params")]
    pub parameters: StableHashMap<String, (Type, Option<Expr>)>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Template {
    pub name: String,
    pub for_isas: Vec<String>,
    pub parent_template: Option<String>,
    #[serde(serialize_with = "serialize_params")]
    pub params: StableHashMap<String, (Type, Option<Expr>)>,
    pub operands: Vec<(String, Type)>,
    pub encoding: Vec<EncodingArm>,
    pub asm: Option<Expr>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Instruction {
    pub name: String,
    pub for_isas: Vec<String>,
    pub parent_template: Option<String>,
    #[serde(serialize_with = "serialize_params")]
    pub params: StableHashMap<String, (Type, Option<Expr>)>,
    pub operands: Vec<(String, Type)>,
    pub encoding: Vec<EncodingArm>,
    pub asm: Option<Expr>,
    pub behavior: Expr,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EncodingArm {
    pub start: u16,
    pub end: Option<u16>,
    pub value: Expr,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Item {
    Isa(Isa),
    RegisterClass(RegisterClass),
    Template(Template),
    Instruction(Instruction),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum Lit {
    Str(LitStr),
    Int(LitInt),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct LitStr {
    value: String,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct LitInt {
    value: String,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Field {
    pub base: Box<Expr>,
    pub member: String,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct If {
    pub cond: Box<Expr>,
    pub then: Box<Expr>,
    pub else_: Option<Box<Expr>>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Block {
    pub stmts: Vec<Expr>,
    pub last_expr_return: bool,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Ident {
    pub name: String,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Assign {
    pub dest: String,
    pub value: Box<Expr>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    ShiftLeftLogical,
    ShiftRightLogical,
    ShiftRightArithmetic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Binary {
    pub lhs: Box<Expr>,
    pub rhs: Box<Expr>,
    pub op: BinOp,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum BuiltinFunction {
    Clamp,
    Extract,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Call {
    pub callee: Box<Expr>,
    pub arguments: Vec<Expr>,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Slice {
    pub base: Box<Expr>,
    pub start: u16,
    pub end: u16,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct IndexAccess {
    pub base: Box<Expr>,
    pub index: u16,
    #[serde(skip_serializing)]
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum Expr {
    Assign(Assign),
    Binary(Binary),
    Block(Block),
    Call(Call),
    Field(Field),
    Ident(Ident),
    If(If),
    IndexAccess(IndexAccess),
    Lit(Lit),
    Slice(Slice),
    BuiltinFunction(BuiltinFunction),
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct File {
    pub items: Vec<Item>,
    pub file_name: String,
}

impl LitInt {
    pub fn new(value: String, span: Span) -> Self {
        Self { value, span }
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

impl LitStr {
    pub fn new(value: String, span: Span) -> Self {
        Self { value, span }
    }

    pub fn value(&self) -> &str {
        &self.value
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
    pub fn new(name: String, span: Span) -> Ident {
        Ident { name, span }
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

impl Item {
    pub fn name(&self) -> &str {
        match self {
            Item::Isa(isa) => &isa.name,
            Item::Instruction(inst) => &inst.name,
            Item::RegisterClass(rc) => &rc.name,
            Item::Template(tmpl) => &tmpl.name,
        }
    }

    pub fn as_register_class(&self) -> Option<&RegisterClass> {
        match self {
            Item::RegisterClass(rc) => Some(rc),
            _ => None,
        }
    }

    pub fn as_instruction(&self) -> Option<&Instruction> {
        match self {
            Item::Instruction(i) => Some(i),
            _ => None,
        }
    }
}

impl Serialize for Type {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Type", 2)?;
        match self {
            Type::String => {
                state.serialize_field("name", "String")?;
            }
            Type::Integer => {
                state.serialize_field("name", "Integer")?;
            }
            Type::Bits(width) => {
                state.serialize_field("name", "Bits")?;
                state.serialize_field("width", width)?;
            }
            Type::Struct(name) => {
                state.serialize_field("name", "Struct")?;
                state.serialize_field("struct", name)?;
            }
            _ => unreachable!("Other types should not be part of AST"),
        }
        state.end()
    }
}

impl RegisterClass {
    pub fn register_name_tables(&self) -> RegisterNameTables {
        let mut entries = self
            .resolve_registers()
            .map(|reg| {
                (
                    parse_trailing_index(&reg.name).unwrap_or(u16::MAX),
                    reg.name,
                    reg.alias,
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(idx, _, _)| *idx);

        let mut next_alias_index = HashMap::new();
        entries.into_iter().fold(
            RegisterNameTables {
                parse_names: Vec::new(),
                isa_names: Vec::new(),
                abi_names: Vec::new(),
            },
            |mut out, (idx, isa_name, alias)| {
                if idx != u16::MAX {
                    out.parse_names.push((isa_name.clone(), idx));
                    out.isa_names.push((idx, isa_name));
                }

                if let Some(alias_name) = alias {
                    let full_alias = if alias_name.contains("{}") {
                        let stem = alias_name.replace("{}", "");
                        let counter = next_alias_index.entry(stem.clone()).or_insert(0);
                        let alias = format!("{}{}", stem, *counter);
                        *counter += 1;
                        alias
                    } else {
                        alias_name
                    };
                    out.parse_names.push((full_alias.clone(), idx));
                    out.abi_names.push((idx, full_alias));
                }

                out
            },
        )
    }

    pub fn hardwired_zero_register_index(&self) -> Option<u16> {
        self.resolve_registers().find_map(|reg| {
            reg.traits
                .iter()
                .any(|t| matches!(t, RegisterTrait::HardwiredZero))
                .then(|| parse_trailing_index(&reg.name).unwrap_or(u16::MAX))
        })
    }

    pub fn resolve_registers(&self) -> impl Iterator<Item = Register> {
        let mut registers = Vec::new();

        for def in &self.registers {
            match def {
                RegisterDef::Single(register) => registers.push(register.clone()),
                RegisterDef::Range(range) => {
                    let (Some(start_idx), Some(end_idx)) = (
                        parse_trailing_index(&range.start),
                        parse_trailing_index(&range.end),
                    ) else {
                        continue;
                    };

                    let prefix = strip_trailing_digits(&range.start);
                    for idx in start_idx..=end_idx {
                        registers.push(Register {
                            name: format!("{prefix}{idx}"),
                            alias: range.alias_pattern.clone(),
                            traits: range.traits.clone(),
                            subregisters: Vec::new(),
                            span: range.span,
                        });
                    }
                }
            }
        }

        registers.into_iter()
    }
}

fn parse_trailing_index(s: &str) -> Option<u16> {
    let mut i = s.len();
    while i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i < s.len() {
        s[i..].parse::<u16>().ok()
    } else {
        None
    }
}

fn strip_trailing_digits(s: &str) -> &str {
    let mut i = s.len();
    while i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
        i -= 1;
    }
    &s[..i]
}

impl File {
    pub fn isas(&self) -> impl Iterator<Item = &Isa> {
        self.items.iter().filter_map(|f| match f {
            Item::Isa(isa) => Some(isa),
            _ => None,
        })
    }

    pub fn templates(&self) -> impl Iterator<Item = &Template> {
        self.items.iter().filter_map(|f| match f {
            Item::Template(t) => Some(t),
            _ => None,
        })
    }

    pub fn instructions(&self) -> impl Iterator<Item = &Instruction> {
        self.items.iter().filter_map(|f| match f {
            Item::Instruction(i) => Some(i),
            _ => None,
        })
    }

    pub fn register_classes(&self) -> impl Iterator<Item = &RegisterClass> {
        self.items.iter().filter_map(|f| match f {
            Item::RegisterClass(rc) => Some(rc),
            _ => None,
        })
    }
}

#[derive(Serialize)]
struct ParamRef<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    ty: &'a Type,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a Expr>,
}

fn serialize_params<S>(
    params: &HashMap<String, (Type, Option<Expr>)>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut mapped: Vec<ParamRef<'_>> = params
        .iter()
        .map(|(name, (ty, val))| ParamRef {
            name,
            ty,
            value: val.as_ref(),
        })
        .collect();
    mapped.sort_by_key(|x| x.name);

    mapped.serialize(serializer)
}
