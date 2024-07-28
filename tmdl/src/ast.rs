pub struct File {
    pub items: Vec<Item>,
}

pub enum Item {
    Register(ItemRegister),
    Struct(ItemStruct),
    Enum(ItemEnum),
}

pub enum Expr {}

pub struct Ident(pub String);

pub struct ItemRegister {
    pub name: Ident,
}

pub enum Type {
    Named(Ident),
}

pub struct Field {
    name: Ident,
    ty: Type,
    default: Option<Expr>,
}

pub struct ItemStruct {}

pub struct ItemEnum {
    pub(crate) name: Ident,
    pub(crate) variants: Vec<Ident>,
}
