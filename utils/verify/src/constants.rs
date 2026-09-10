use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use isla_lib::bitvector::b129::B129;
use isla_lib::ir::{Def, IRTypeInfo, Instr, Loc, Name, Symtab, Val};
use isla_lib::source_loc::SourceLoc;
use isla_lib::{ir_lexer::new_ir_lexer, value_parser, zencode};
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub constants: BTreeMap<String, Constant>,
    pub execution_wrapper: Option<std::path::PathBuf>,
    #[serde(default)]
    pub linearize: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Constant {
    Constructor {
        constructor: String,
        value: Box<Self>,
    },
    Fields(BTreeMap<String, Self>),
    Literal(String),
    Bool(bool),
}

impl Config {
    pub fn apply(
        self,
        definitions: &mut [Def<Name, B129>],
        symtab: &Symtab<'_>,
        types: &IRTypeInfo,
    ) -> anyhow::Result<()> {
        for (name, value) in self.constants {
            let name = symtab
                .get(&zencode::encode(&name))
                .context("unknown Sail constant")?;
            let setup = definitions
                .iter_mut()
                .find_map(|definition| match definition {
                    Def::Let(bindings, setup)
                        if bindings.iter().any(|(binding, _)| *binding == name) =>
                    {
                        Some(setup)
                    }
                    _ => None,
                })
                .context("Sail constant binding is absent")?;
            value.assign(Loc::Id(name), setup, symtab, types)?;
        }
        Ok(())
    }
}

impl Constant {
    fn assign(
        &self,
        location: Loc<Name>,
        setup: &mut Vec<Instr<Name, B129>>,
        symtab: &Symtab<'_>,
        types: &IRTypeInfo,
    ) -> anyhow::Result<()> {
        if let Self::Fields(fields) = self {
            for (name, value) in fields {
                let field = symtab
                    .get(&zencode::encode(name))
                    .context("unknown Sail field")?;
                value.assign(
                    Loc::Field(Box::new(location.clone()), field),
                    setup,
                    symtab,
                    types,
                )?;
            }
        } else {
            let value = self.value(symtab, types)?;
            setup.push(Instr::PrimopReset(
                location,
                Arc::new(move |_, _, _| Ok(value.clone())),
                SourceLoc::unknown(),
            ));
        }
        Ok(())
    }

    fn value(&self, symtab: &Symtab<'_>, types: &IRTypeInfo) -> anyhow::Result<Val<B129>> {
        Ok(match self {
            Self::Bool(value) => Val::Bool(*value),
            Self::Literal(value) => value_parser::ValParser::new()
                .parse(symtab, types, new_ir_lexer(value))
                .map_err(|_| anyhow!("invalid Sail constant {value}"))?,
            Self::Constructor { constructor, value } => {
                let name = symtab
                    .get(&zencode::encode(constructor))
                    .context("unknown Sail constructor")?;
                Val::Ctor(name, Box::new(value.value(symtab, types)?))
            }
            Self::Fields(_) => return Err(anyhow!("a struct constant needs an existing value")),
        })
    }
}
