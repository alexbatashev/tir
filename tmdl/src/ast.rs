use std::fmt::Display;

#[derive(Debug, Clone)]
pub struct TranslationUnit {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Register(ItemRegister),
    Record(ItemRecord),
    Enum(ItemEnum),
}

#[derive(Debug, Clone)]
pub enum Expr {}

#[derive(Debug, Clone)]
pub struct Ident(pub String);

#[derive(Debug, Clone)]
pub struct ItemRegister {
    pub name: Ident,
}

#[derive(Debug, Clone)]
pub enum Type {
    Named(Ident),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: Ident,
    pub ty: Type,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct ItemRecord {}

#[derive(Debug, Clone)]
pub struct ItemEnum {
    pub name: Ident,
    pub variants: Vec<Ident>,
}

impl Display for TranslationUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("translation_unit {\n")?;
        f.write_str("items: [\n")?;
        for item in &self.items {
            f.write_fmt(format_args!("{}", item))?;
            f.write_str(",\n")?;
        }
        f.write_str("]\n}")?;
        Ok(())
    }
}

impl Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::Enum(enum_) => f.write_fmt(format_args!("{}", enum_)),
            _ => todo!(),
        }
    }
}

impl Display for ItemEnum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("enum {\nname: ")?;
        f.write_str(&self.name.0)?;
        f.write_str(",\n")?;

        f.write_str("variants: [\n")?;

        for variant in &self.variants {
            f.write_str(&variant.0)?;
            f.write_str(",\n")?;
        }

        f.write_str("]\n}")?;

        Ok(())
    }
}
