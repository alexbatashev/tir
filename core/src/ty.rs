use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Integer { width: u32 },
    Float32,
    Float64,
    Index,
    None,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Integer { width } => write!(f, "i{}", width),
            Type::Float32 => write!(f, "f32"),
            Type::Float64 => write!(f, "f64"),
            Type::Index => write!(f, "index"),
            Type::None => write!(f, "none"),
        }
    }
}

impl FromStr for Type {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "f32" => Ok(Type::Float32),
            "f64" => Ok(Type::Float64),
            "index" => Ok(Type::Index),
            "none" => Ok(Type::None),
            _ if s.starts_with('i') => {
                let width: u32 = s[1..]
                    .parse()
                    .map_err(|_| format!("Invalid integer type: {}", s))?;
                Ok(Type::Integer { width })
            }
            _ => Err(format!("Unknown type: {}", s)),
        }
    }
}
