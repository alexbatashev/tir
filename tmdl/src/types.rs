use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Copy, Hash)]
pub struct TypeVar(u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    // Core (ground) types
    String,
    Integer,
    Bits(u16),
    Struct(String),

    // HM types
    Var(TypeVar),
    Fn(Box<Type>, Box<Type>),
    Con(String, Vec<Type>),
}

#[derive(Debug, Clone, Default)]
pub struct Substitution {
    map: HashMap<TypeVar, Type>,
}

/// A plymorphic type
#[derive(Debug, Clone)]
pub struct TypeScheme {
    /// Quantified variables
    pub vars: Vec<TypeVar>,
    /// Body
    pub ty: Type,
}

#[derive(Debug, Clone, Default)]
pub struct TypeEnv {
    bindings: HashMap<String, TypeScheme>,
    parent: Option<Box<TypeEnv>>,
}

impl Type {
    /// Collect all free type variables.
    pub fn free_vars(&self) -> HashSet<TypeVar> {
        match self {
            Type::Var(v) => std::iter::once(*v).collect(),
            Type::Fn(a, b) => {
                let mut s = a.free_vars();
                s.extend(b.free_vars());
                s
            }
            Type::Con(_, args) => args.iter().flat_map(|a| a.free_vars()).collect(),
            // Ground types have no free variables
            _ => HashSet::new(),
        }
    }

    /// Apply a substitution to this type.
    pub fn apply(&self, subst: &Substitution) -> Type {
        match self {
            Type::Var(v) => subst.get(v),
            Type::Fn(a, b) => Type::Fn(Box::new(a.apply(subst)), Box::new(b.apply(subst))),
            Type::Con(name, args) => {
                Type::Con(name.clone(), args.iter().map(|a| a.apply(subst)).collect())
            }
            other => other.clone(),
        }
    }

    /// Check whether a type variable occurs in this type (for occurs check).
    pub fn occurs(&self, v: TypeVar) -> bool {
        match self {
            Type::Var(u) => *u == v,
            Type::Fn(a, b) => a.occurs(v) || b.occurs(v),
            Type::Con(_, args) => args.iter().any(|a| a.occurs(v)),
            _ => false,
        }
    }
}

impl Substitution {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, v: TypeVar, ty: Type) {
        self.map.insert(v, ty);
    }

    pub fn get(&self, v: &TypeVar) -> Type {
        match self.map.get(v) {
            Some(ty) => ty.clone(),
            None => Type::Var(*v),
        }
    }

    pub fn compose(mut self, other: &Self) -> Self {
        self.map.values_mut().map(|ty| *ty = ty.apply(other));
        other.map.iter().map(|(v, ty)| {
            self.map.entry(*v).or_insert_with(|| ty.clone());
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Allocates fresh type variables with monotonically increasing IDs.
#[derive(Debug, Clone, Default)]
pub struct TypeVarGen(u32);

impl TypeVarGen {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fresh(&mut self) -> TypeVar {
        let v = TypeVar(self.0);
        self.0 += 1;
        v
    }
}

impl TypeScheme {
    /// Monomorphic scheme - no quantification
    pub fn mono(ty: Type) -> Self {
        TypeScheme { vars: vec![], ty }
    }

    pub fn free_vars(&self) -> HashSet<TypeVar> {
        let mut fv = self.ty.free_vars();
        for v in &self.vars {
            fv.remove(v);
        }
        fv
    }
}

impl TypeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enter_scope(self) -> Self {
        TypeEnv {
            bindings: HashMap::new(),
            parent: Some(Box::new(self)),
        }
    }

    pub fn exit_scope(self) -> Option<Self> {
        self.parent.map(|p| *p)
    }

    pub fn bind(&mut self, name: impl Into<String>, scheme: TypeScheme) {
        self.bindings.insert(name.into(), scheme);
    }

    pub fn get(&self, name: impl AsRef<str>) -> Option<&TypeScheme> {
        self.bindings.get(name.as_ref())
    }

    pub fn free_vars(&self) -> HashSet<TypeVar> {
        let mut fv: HashSet<TypeVar> = self.bindings.values().flat_map(|s| s.free_vars()).collect();
        if let Some(parent) = &self.parent {
            fv.extend(parent.free_vars());
        }
        fv
    }
}
