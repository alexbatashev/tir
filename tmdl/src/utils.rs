use core::fmt;
use std::collections::HashMap;
use std::collections::hash_map::{
    IntoIter as HashMapIntoIter, Iter as HashMapIter, IterMut as HashMapIterMut,
};
use std::hash::Hash;
use std::ops::{Deref, DerefMut};

use chumsky::container::Container;

use crate::Type;
use crate::ast::{self, Instruction, Item};

#[derive(PartialEq, Clone)]
pub struct StableHashMap<K: Eq + Hash, V: PartialEq>(HashMap<K, V>);

impl<K: Eq + Hash, V: PartialEq> Default for StableHashMap<K, V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Eq + Hash, V: PartialEq> Deref for StableHashMap<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Eq + Hash, V: PartialEq> DerefMut for StableHashMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Eq + Hash, V: PartialEq> fmt::Debug for StableHashMap<K, V>
where
    K: Ord + fmt::Debug,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries: Vec<_> = self.0.iter().collect();
        entries.sort_by_key(|(k, _)| *k);
        f.debug_map().entries(entries).finish()
    }
}

impl<K: Eq + Hash, V: PartialEq> Into<StableHashMap<K, V>> for HashMap<K, V> {
    fn into(self) -> StableHashMap<K, V> {
        StableHashMap(self)
    }
}

impl<K: Eq + Hash, V: PartialEq> FromIterator<(K, V)> for StableHashMap<K, V>
where
    K: Eq + Hash,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(HashMap::from_iter(iter))
    }
}

impl<K: Eq + Hash, V: PartialEq> Container<(K, V)> for StableHashMap<K, V> {
    fn push(&mut self, item: (K, V)) {
        self.0.push(item)
    }

    fn with_capacity(n: usize) -> Self {
        Self(HashMap::with_capacity(n))
    }
}

impl<K: Eq + Hash, V: PartialEq> IntoIterator for StableHashMap<K, V> {
    type Item = (K, V);
    type IntoIter = HashMapIntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, K: Eq + Hash, V: PartialEq> IntoIterator for &'a StableHashMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = HashMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, K: Eq + Hash, V: PartialEq> IntoIterator for &'a mut StableHashMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = HashMapIterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

pub fn resolve_operands_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<&'a str, &'a ast::Item>,
) -> Vec<(String, Type)> {
    let mut result = Vec::new();

    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<&'a str, &'a ast::Item>,
        acc: &mut Vec<(String, Type)>,
    ) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.operands {
                acc.push((k.clone(), v.clone()));
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, item_cache, &mut result);
    }
    for (k, v) in &inst.operands {
        result.push((k.clone(), v.clone()));
    }
    result
}

pub fn get_encoding_arms<'a>(
    instruction: &'a Instruction,
    item_cache: &HashMap<&'a str, &'a Item>,
) -> Vec<ast::EncodingArm> {
    if !instruction.encoding.is_empty() {
        instruction.encoding.clone()
    } else {
        let mut cur = instruction.parent_template.as_ref();
        while let Some(name) = cur {
            if let Some(ast::Item::Template(t)) = item_cache.get(name.as_str()) {
                if !t.encoding.is_empty() {
                    return t.encoding.clone();
                }
                cur = t.parent_template.as_ref();
            } else {
                break;
            }
        }
        Vec::new()
    }
}

pub fn resolve_params_for_instruction<'a>(
    inst: &'a ast::Instruction,
    cache: &HashMap<&'a str, &'a ast::Item>,
) -> HashMap<String, (Type, Option<ast::Expr>)> {
    let mut result: HashMap<String, (Type, Option<ast::Expr>)> = HashMap::new();

    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<&'a str, &'a ast::Item>,
        acc: &mut HashMap<String, (Type, Option<ast::Expr>)>,
    ) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.params {
                acc.insert(k.clone(), v.clone());
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, cache, &mut result);
    }
    for (k, v) in &inst.params {
        result.insert(k.clone(), v.clone());
    }
    result
}

pub fn parse_literal_value(lit: &ast::LitInt) -> u64 {
    let v = lit.value();
    if v.starts_with("0b") {
        u64::from_str_radix(&v[2..], 2).unwrap_or(0)
    } else if v.starts_with("0x") || v.starts_with("0X") {
        u64::from_str_radix(&v[2..], 16).unwrap_or(0)
    } else {
        v.parse::<u64>().unwrap_or(0)
    }
}
