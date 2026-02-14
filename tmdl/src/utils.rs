use std::collections::HashMap;

use crate::ast::{self, Instruction, Item};

pub fn resolve_operands_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<String, &'a ast::Item>,
) -> Vec<(String, ast::Type)> {
    let mut result = Vec::new();

    // collect from root-most template first
    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<String, &'a ast::Item>,
        acc: &mut Vec<(String, ast::Type)>,
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
    item_cache: &HashMap<String, &'a Item>,
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
    cache: &HashMap<String, &'a ast::Item>,
) -> HashMap<String, (ast::Type, Option<ast::Expr>)> {
    let mut result: HashMap<String, (ast::Type, Option<ast::Expr>)> = HashMap::new();
    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<String, &'a ast::Item>,
        acc: &mut HashMap<String, (ast::Type, Option<ast::Expr>)>,
    ) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.params {
                acc.entry(k.clone()).or_insert(v.clone());
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
