use quote::format_ident;
use syn::{Expr, FieldValue, Ident, Lit, Member};

pub fn expr_as_string(expr: &Expr) -> String {
    match &expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(str) => str.value(),
            _ => unreachable!(),
        },
        Expr::Path(p) => p.path.get_ident().unwrap().to_string(),
        _ => unreachable!(),
    }
}

pub fn field_name(field: &FieldValue) -> String {
    match &field.member {
        Member::Named(name) => name.to_string(),
        _ => unreachable!(),
    }
}

pub fn expr_as_ident_vec(expr: &Expr) -> Vec<Ident> {
    if let Expr::Array(arr) = expr {
        arr.elems
            .iter()
            .map(|e| {
                if let Expr::Path(p) = e {
                    p.path.get_ident().unwrap().clone()
                } else {
                    unreachable!()
                }
            })
            .collect()
    } else {
        unreachable!()
    }
}

pub fn op_fn_ident(name: &str) -> Ident {
    let mut sanitized = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        sanitized.push_str("op");
    }
    let first = sanitized.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        sanitized.insert_str(0, "op_");
    }
    format_ident!("r#{}", sanitized)
}
