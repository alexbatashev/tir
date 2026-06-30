//! AST mirroring SMT-LIB 2.7 grammar; theory meaning is resolved at conversion, not baked in here.

/// Non-alphanumeric characters permitted in a simple (unquoted) symbol.
pub const SYMBOL_CHARS: &str = "+-/*=%?!.$_~&^<>@";

/// Whether `name` is a valid simple symbol and so needs no `|...|` quoting.
pub fn is_simple_symbol(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || SYMBOL_CHARS.contains(first) => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || SYMBOL_CHARS.contains(c))
}

/// `<spec_constant>`. Hex/Binary keep prefix-less digits so width survives round-trip; numerals capped at u128.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecConstant {
    Numeral(u128),
    Decimal(String),
    Hexadecimal(String),
    Binary(String),
    String(String),
}

/// A `<symbol>`: logical name without `|...|` quotes; quoting is a printing concern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(pub String);

/// A `<keyword>`, stored without the leading `:`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Keyword(pub String);

/// An `<index>`: the `7`/`0` in `(_ extract 7 0)` or a symbolic index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Index {
    Numeral(u128),
    Symbol(Symbol),
}

/// An `<identifier>`: a bare symbol, or an indexed `(_ symbol index+)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identifier {
    pub symbol: Symbol,
    pub indices: Vec<Index>,
}

impl Identifier {
    pub fn simple(name: impl Into<String>) -> Self {
        Identifier {
            symbol: Symbol(name.into()),
            indices: Vec::new(),
        }
    }

    pub fn is_simple(&self) -> bool {
        self.indices.is_empty()
    }
}

/// A `<sort>`: an identifier optionally applied to argument sorts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sort {
    pub id: Identifier,
    pub params: Vec<Sort>,
}

impl Sort {
    pub fn simple(id: Identifier) -> Self {
        Sort {
            id,
            params: Vec::new(),
        }
    }
}

/// A `<qual_identifier>`: an identifier, optionally `(as id sort)`-annotated to disambiguate overloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QualIdentifier {
    Plain(Identifier),
    Annotated(Identifier, Sort),
}

impl QualIdentifier {
    pub fn identifier(&self) -> &Identifier {
        match self {
            QualIdentifier::Plain(id) | QualIdentifier::Annotated(id, _) => id,
        }
    }
}

/// An `<s_expr>`: the generic untyped form for attribute values and other nested expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SExpr {
    Constant(SpecConstant),
    Symbol(Symbol),
    Keyword(Keyword),
    List(Vec<SExpr>),
}

/// An `<attribute_value>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributeValue {
    Constant(SpecConstant),
    Symbol(Symbol),
    List(Vec<SExpr>),
}

/// An `<attribute>`: a keyword optionally carrying a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub keyword: Keyword,
    pub value: Option<AttributeValue>,
}

/// A `(symbol term)` binding inside `let`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarBinding {
    pub var: Symbol,
    pub term: Term,
}

/// A `(symbol sort)` binding inside a quantifier or function definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortedVar {
    pub var: Symbol,
    pub sort: Sort,
}

/// A `match` pattern: a variable/nullary constructor, or `(constructor var+)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    Var(Symbol),
    Constructor(Symbol, Vec<Symbol>),
}

/// A `(pattern term)` arm of a `match`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub body: Term,
}

/// A `<term>`. Quantifiers and `match` exist for grammar fidelity only; conversion rejects them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    Constant(SpecConstant),
    Ident(QualIdentifier),
    App(QualIdentifier, Vec<Term>),
    Let(Vec<VarBinding>, Box<Term>),
    Forall(Vec<SortedVar>, Box<Term>),
    Exists(Vec<SortedVar>, Box<Term>),
    Match(Box<Term>, Vec<MatchCase>),
    Annotated(Box<Term>, Vec<Attribute>),
}

/// A `function_def`: `symbol (sorted_var*) sort term`, shared by `define-fun`/`define-fun-rec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDef {
    pub name: Symbol,
    pub params: Vec<SortedVar>,
    pub return_sort: Sort,
    pub body: Term,
}

/// A `function_dec`: the signature half `(symbol (sorted_var*) sort)` used by `define-funs-rec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDec {
    pub name: Symbol,
    pub params: Vec<SortedVar>,
    pub return_sort: Sort,
}

/// A `prop_literal`: `symbol` or `(not symbol)`, used by `check-sat-assuming`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropLiteral {
    pub symbol: Symbol,
    pub negated: bool,
}

/// A top-level `<command>`. Datatype, array and string declarations are out of scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SetLogic(Symbol),
    SetOption(Attribute),
    SetInfo(Attribute),
    DeclareSort(Symbol, u128),
    DefineSort(Symbol, Vec<Symbol>, Sort),
    DeclareConst(Symbol, Sort),
    DeclareFun(Symbol, Vec<Sort>, Sort),
    DefineFun(FunctionDef),
    DefineFunRec(FunctionDef),
    DefineFunsRec(Vec<FunctionDec>, Vec<Term>),
    Assert(Term),
    CheckSat,
    CheckSatAssuming(Vec<PropLiteral>),
    GetAssertions,
    GetModel,
    GetValue(Vec<Term>),
    GetProof,
    GetUnsatCore,
    GetUnsatAssumptions,
    GetAssignment,
    GetInfo(Keyword),
    GetOption(Keyword),
    Push(u128),
    Pop(u128),
    Reset,
    ResetAssertions,
    Echo(String),
    Exit,
}

/// A full SMT-LIB script: a sequence of commands.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Script(pub Vec<Command>);
