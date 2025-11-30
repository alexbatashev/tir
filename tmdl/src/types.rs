use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};

use crate::ast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeVar(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SizeVar(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Size {
    Const(u16),
    Var(SizeVar),
    Add(Box<Size>, Box<Size>),
    Sub(Box<Size>, Box<Size>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Integer,
    String,
    Bool,
    Bits(Size),
    Struct(String),
    Var(TypeVar),
    Fun(Vec<Type>, Box<Type>),
}

impl Type {
    pub fn bits_const(width: u16) -> Self {
        Type::Bits(Size::Const(width))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheme {
    pub ty_vars: Vec<TypeVar>,
    pub size_vars: Vec<SizeVar>,
    pub ty: Type,
}

impl Scheme {
    pub fn new(ty_vars: Vec<TypeVar>, size_vars: Vec<SizeVar>, ty: Type) -> Self {
        Self {
            ty_vars,
            size_vars,
            ty,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Subst {
    pub types: HashMap<TypeVar, Type>,
    pub sizes: HashMap<SizeVar, Size>,
}

impl Subst {
    pub fn empty() -> Self {
        Subst {
            types: HashMap::new(),
            sizes: HashMap::new(),
        }
    }

    /// Compose substitutions: `self` is applied after `other`.
    pub fn compose(&self, other: &Subst) -> Subst {
        let mut types = other
            .types
            .iter()
            .map(|(tv, t)| (*tv, t.apply(self)))
            .collect::<HashMap<_, _>>();
        types.extend(self.types.clone());

        let mut sizes = other
            .sizes
            .iter()
            .map(|(sv, s)| (*sv, s.apply(self)))
            .collect::<HashMap<_, _>>();
        sizes.extend(self.sizes.clone());

        Subst { types, sizes }
    }
}

pub trait Substitute {
    fn apply(&self, s: &Subst) -> Self;
}

pub trait FreeVars {
    fn free_type_vars(&self) -> BTreeSet<TypeVar>;
    fn free_size_vars(&self) -> BTreeSet<SizeVar>;
}

impl Substitute for Size {
    fn apply(&self, s: &Subst) -> Self {
        match self {
            Size::Const(_) => self.clone(),
            Size::Var(v) => s.sizes.get(v).cloned().unwrap_or(Size::Var(*v)),
            Size::Add(a, b) => Size::Add(Box::new(a.apply(s)), Box::new(b.apply(s))),
            Size::Sub(a, b) => Size::Sub(Box::new(a.apply(s)), Box::new(b.apply(s))),
        }
    }
}

impl FreeVars for Size {
    fn free_type_vars(&self) -> BTreeSet<TypeVar> {
        BTreeSet::new()
    }

    fn free_size_vars(&self) -> BTreeSet<SizeVar> {
        match self {
            Size::Const(_) => BTreeSet::new(),
            Size::Var(v) => BTreeSet::from([*v]),
            Size::Add(a, b) | Size::Sub(a, b) => {
                let mut set = a.free_size_vars();
                set.extend(b.free_size_vars());
                set
            }
        }
    }
}

impl Substitute for Type {
    fn apply(&self, s: &Subst) -> Self {
        match self {
            Type::Var(v) => s.types.get(v).cloned().unwrap_or(Type::Var(*v)),
            Type::Bits(sz) => Type::Bits(sz.apply(s)),
            Type::Fun(args, ret) => Type::Fun(
                args.iter().map(|t| t.apply(s)).collect(),
                Box::new(ret.apply(s)),
            ),
            t @ (Type::Integer | Type::String | Type::Bool | Type::Struct(_)) => t.clone(),
        }
    }
}

impl FreeVars for Type {
    fn free_type_vars(&self) -> BTreeSet<TypeVar> {
        match self {
            Type::Var(v) => BTreeSet::from([*v]),
            Type::Bits(sz) => sz.free_type_vars(),
            Type::Fun(args, ret) => {
                let mut set = ret.free_type_vars();
                for a in args {
                    set.extend(a.free_type_vars());
                }
                set
            }
            _ => BTreeSet::new(),
        }
    }

    fn free_size_vars(&self) -> BTreeSet<SizeVar> {
        match self {
            Type::Bits(sz) => sz.free_size_vars(),
            Type::Fun(args, ret) => {
                let mut set = ret.free_size_vars();
                for a in args {
                    set.extend(a.free_size_vars());
                }
                set
            }
            _ => BTreeSet::new(),
        }
    }
}

impl Substitute for Scheme {
    fn apply(&self, s: &Subst) -> Self {
        // Avoid capturing quantified variables by removing them from the substitution
        let filtered = Subst {
            types: s
                .types
                .iter()
                .filter(|(k, _)| !self.ty_vars.contains(k))
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            sizes: s
                .sizes
                .iter()
                .filter(|(k, _)| !self.size_vars.contains(k))
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
        };
        Scheme {
            ty_vars: self.ty_vars.clone(),
            size_vars: self.size_vars.clone(),
            ty: self.ty.apply(&filtered),
        }
    }
}

impl FreeVars for Scheme {
    fn free_type_vars(&self) -> BTreeSet<TypeVar> {
        let mut set = self.ty.free_type_vars();
        for v in &self.ty_vars {
            set.remove(v);
        }
        set
    }

    fn free_size_vars(&self) -> BTreeSet<SizeVar> {
        let mut set = self.ty.free_size_vars();
        for v in &self.size_vars {
            set.remove(v);
        }
        set
    }
}

#[derive(Debug, Default, Clone)]
pub struct TypeEnv {
    pub vars: HashMap<String, Scheme>,
}

impl TypeEnv {
    pub fn get(&self, name: &str) -> Option<&Scheme> {
        self.vars.get(name)
    }

    pub fn insert(&mut self, name: impl Into<String>, scheme: Scheme) {
        self.vars.insert(name.into(), scheme);
    }

    pub fn extend(&self, name: impl Into<String>, scheme: Scheme) -> Self {
        let mut cloned = self.clone();
        cloned.insert(name, scheme);
        cloned
    }
}

pub fn generalize(env: &TypeEnv, ty: Type) -> Scheme {
    let env_free_tys = env
        .vars
        .values()
        .flat_map(|s| s.free_type_vars())
        .collect::<BTreeSet<_>>();
    let env_free_sizes = env
        .vars
        .values()
        .flat_map(|s| s.free_size_vars())
        .collect::<BTreeSet<_>>();

    let mut ty_vars = ty.free_type_vars();
    ty_vars.retain(|v| !env_free_tys.contains(v));

    let mut size_vars = ty.free_size_vars();
    size_vars.retain(|v| !env_free_sizes.contains(v));

    Scheme::new(
        ty_vars.into_iter().collect(),
        size_vars.into_iter().collect(),
        ty,
    )
}

pub fn instantiate(scheme: &Scheme, ctx: &TypeCtx) -> Type {
    let mut subst = Subst::empty();
    for tv in &scheme.ty_vars {
        subst.types.insert(*tv, ctx.fresh_type_var());
    }
    for sv in &scheme.size_vars {
        subst.sizes.insert(*sv, ctx.fresh_size_var());
    }
    scheme.ty.apply(&subst)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeError {
    Occurs(TypeVar, Type),
    SizeOccurs(SizeVar, Size),
    Mismatch(Type, Type),
    SizeMismatch(Size, Size),
}

pub fn unify(t1: Type, t2: Type) -> Result<Subst, TypeError> {
    match (t1, t2) {
        (Type::Var(v), t) | (t, Type::Var(v)) => bind_type_var(v, t),
        (Type::Integer, Type::Integer) => Ok(Subst::empty()),
        (Type::String, Type::String) => Ok(Subst::empty()),
        (Type::Bool, Type::Bool) => Ok(Subst::empty()),
        (Type::Struct(a), Type::Struct(b)) if a == b => Ok(Subst::empty()),
        (Type::Bits(a), Type::Bits(b)) => unify_size(a, b),
        (Type::Fun(a_args, a_ret), Type::Fun(b_args, b_ret)) => {
            if a_args.len() != b_args.len() {
                return Err(TypeError::Mismatch(
                    Type::Fun(a_args, a_ret),
                    Type::Fun(b_args, b_ret),
                ));
            }

            let mut subst = Subst::empty();
            for (a, b) in a_args.into_iter().zip(b_args.into_iter()) {
                let s1 = unify(a.apply(&subst), b.apply(&subst))?;
                subst = subst.compose(&s1);
            }
            let s_ret = unify(a_ret.apply(&subst), b_ret.apply(&subst))?;
            subst = subst.compose(&s_ret);
            Ok(subst)
        }
        (a, b) => Err(TypeError::Mismatch(a, b)),
    }
}

fn bind_type_var(var: TypeVar, ty: Type) -> Result<Subst, TypeError> {
    if matches!(ty, Type::Var(v) if v == var) {
        return Ok(Subst::empty());
    }
    if ty.free_type_vars().contains(&var) {
        return Err(TypeError::Occurs(var, ty));
    }
    let mut s = Subst::empty();
    s.types.insert(var, ty);
    Ok(s)
}

pub fn unify_size(a: Size, b: Size) -> Result<Subst, TypeError> {
    let a = simplify_size(a);
    let b = simplify_size(b);
    match (a, b) {
        (Size::Const(x), Size::Const(y)) if x == y => Ok(Subst::empty()),
        (Size::Var(v), s) | (s, Size::Var(v)) => bind_size_var(v, s),
        (x, y) => Err(TypeError::SizeMismatch(x, y)),
    }
}

fn bind_size_var(var: SizeVar, sz: Size) -> Result<Subst, TypeError> {
    if matches!(sz, Size::Var(v) if v == var) {
        return Ok(Subst::empty());
    }
    if sz.free_size_vars().contains(&var) {
        return Err(TypeError::SizeOccurs(var, sz));
    }
    let mut s = Subst::empty();
    s.sizes.insert(var, sz);
    Ok(s)
}

// Reduce simple arithmetic on sizes to help unification (e.g., Add(Const 4, Const 4) -> Const 8).
pub fn simplify_size(sz: Size) -> Size {
    fn eval(sz: &Size) -> Option<u32> {
        match sz {
            Size::Const(c) => Some(*c as u32),
            Size::Var(_) => None,
            Size::Add(a, b) => Some(eval(a)? + eval(b)?),
            Size::Sub(a, b) => Some(eval(a)? - eval(b)?),
        }
    }

    if let Some(v) = eval(&sz) {
        if v <= u16::MAX as u32 {
            return Size::Const(v as u16);
        }
    }

    match sz {
        Size::Add(a, b) => Size::Add(Box::new(simplify_size(*a)), Box::new(simplify_size(*b))),
        Size::Sub(a, b) => Size::Sub(Box::new(simplify_size(*a)), Box::new(simplify_size(*b))),
        other => other,
    }
}

#[derive(Debug, Default)]
pub struct TypeCtx {
    next_ty: Cell<u32>,
    next_sz: Cell<u32>,
}

impl TypeCtx {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fresh_type_var(&self) -> Type {
        let id = self.next_ty.get();
        self.next_ty.set(id + 1);
        Type::Var(TypeVar(id))
    }

    pub fn fresh_size_var(&self) -> Size {
        let id = self.next_sz.get();
        self.next_sz.set(id + 1);
        Size::Var(SizeVar(id))
    }
}

impl From<&ast::Type> for Type {
    fn from(t: &ast::Type) -> Self {
        match t {
            ast::Type::String => Type::String,
            ast::Type::Integer => Type::Integer,
            ast::Type::Bits(w) => Type::Bits(Size::Const(*w)),
            ast::Type::Struct(name) => Type::Struct(name.clone()),
        }
    }
}

impl From<ast::Type> for Type {
    fn from(t: ast::Type) -> Self {
        Type::from(&t)
    }
}
