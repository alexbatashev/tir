use crate::ast;

pub fn compile_to_state<FEval, FAssign, FIf>(
    expr: &ast::Expr,
    st_name: &str,
    eval_expr: &FEval,
    emit_assign: &FAssign,
    emit_if: &FIf,
) -> String
where
    FEval: Fn(&ast::Expr) -> String,
    FAssign: Fn(&ast::Assign, &str) -> Option<String>,
    FIf: Fn(&str, &str, &str) -> String,
{
    match expr {
        ast::Expr::Assign(a) => emit_assign(a, st_name).unwrap_or_else(|| st_name.to_string()),
        ast::Expr::Block(b) => {
            let mut current = st_name.to_string();
            for stmt in &b.stmts {
                if matches!(
                    stmt,
                    ast::Expr::Assign(_) | ast::Expr::Block(_) | ast::Expr::If(_)
                ) {
                    current = compile_to_state(stmt, &current, eval_expr, emit_assign, emit_if);
                }
            }
            current
        }
        ast::Expr::If(i) => {
            let cond = eval_expr(&i.cond);
            let then_state = compile_to_state(&i.then, st_name, eval_expr, emit_assign, emit_if);
            let else_state = if let Some(e) = &i.else_ {
                compile_to_state(e, st_name, eval_expr, emit_assign, emit_if)
            } else {
                st_name.to_string()
            };
            emit_if(&cond, &then_state, &else_state)
        }
        _ => st_name.to_string(),
    }
}
