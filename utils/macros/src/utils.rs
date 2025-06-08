use syn::{Expr, FieldValue, Ident, Lit, Member};

pub fn expr_as_string(expr: &Expr) -> String {
    match &expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(str) => str.value(),
            _ => unreachable!(),
        },
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
