use std::collections::HashMap;

// Core AST nodes

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
    pub parameters: HashMap<String, i32>,
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
    pub parameters: HashMap<String, i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub isas: Vec<Isa>,
    pub register_classes: Vec<RegisterClass>,
}
