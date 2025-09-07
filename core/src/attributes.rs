use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
    Str(String),
    Int(i64),
    UInt(u64),
    F32(f32),
    F64(f64),
    Bool(bool),
    Array(Vec<AttributeValue>),
    Dict(BTreeMap<String, AttributeValue>),
    Register(RegisterAttr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeRole {
    None,
    Def,
    Use,
    Clobber,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegisterAttr {
    Physical { class: String, index: u16 },
    Virtual { id: u32, class: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamedAttribute {
    pub name: String,
    pub value: AttributeValue,
}

impl NamedAttribute {
    pub fn new(name: impl Into<String>, value: AttributeValue) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

impl AttributeValue {
    pub fn print(&self, fmt: &mut crate::IRFormatter) -> Result<(), std::fmt::Error> {
        match self {
            AttributeValue::Str(s) => fmt.write(format!("\"{}\"", s)),
            AttributeValue::Int(i) => fmt.write(i.to_string()),
            AttributeValue::UInt(u) => fmt.write(u.to_string()),
            AttributeValue::F32(fv) => fmt.write(fv.to_string()),
            AttributeValue::F64(fv) => fmt.write(fv.to_string()),
            AttributeValue::Bool(b) => fmt.write(if *b { "true" } else { "false" }),
            AttributeValue::Array(arr) => {
                fmt.write("[")?;
                let mut first = true;
                for v in arr {
                    if !first {
                        fmt.write(", ")?;
                    }
                    first = false;
                    v.print(fmt)?;
                }
                fmt.write("]")
            }
            AttributeValue::Dict(map) => {
                fmt.write("{")?;
                let mut first = true;
                for (k, v) in map.iter() {
                    if !first {
                        fmt.write(", ")?;
                    }
                    first = false;
                    fmt.write(k)?;
                    fmt.write(" = ")?;
                    v.print(fmt)?;
                }
                fmt.write("}")
            }
            AttributeValue::Register(r) => match r {
                RegisterAttr::Physical { class, index } => {
                    // Prefer a readable ISA-like form when possible; otherwise class[index]
                    if class == "GPR" {
                        fmt.write(format!("x{}", index))
                    } else {
                        fmt.write(format!("{}[{}]", class, index))
                    }
                }
                RegisterAttr::Virtual { id, class } => {
                    if let Some(cls) = class {
                        fmt.write(format!("%virt{}:{}", id, cls))
                    } else {
                        fmt.write(format!("%virt{}", id))
                    }
                }
            },
        }
    }
}
