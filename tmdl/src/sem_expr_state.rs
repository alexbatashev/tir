use crate::ast;

/// Target-independent statement/control-flow form for an instruction behavior.
/// Value expressions stay as AST references until the target printer lowers
/// them to SemGraph; statement classification happens once here.
pub enum Effect<'a> {
    Assign(&'a ast::Assign),
    Store(&'a ast::Expr),
    StoreConditional(&'a ast::Expr),
    Fence(&'a ast::Expr),
    Trap(&'a ast::Expr),
    Block(Vec<Effect<'a>>),
    If {
        cond: &'a ast::Expr,
        then_effect: Box<Effect<'a>>,
        else_effect: Option<Box<Effect<'a>>>,
    },
    Try {
        source: &'a ast::TryExcept,
        body: Box<Effect<'a>>,
    },
}

/// Statement-level hooks invoked by [`compile_to_state`] while folding a
/// behavior into a single state expression. `None` from a hook marks the
/// statement unsupported.
pub trait StateEmitter {
    type State: Clone;
    type Condition;

    /// Boolean condition of an `if`.
    fn cond(&self, e: &ast::Expr) -> Self::Condition;
    fn assign(&self, a: &ast::Assign, state: &Self::State) -> Option<Self::State>;
    /// A bare `store(addr, size, value)` effect statement.
    fn store(&self, c: &ast::Call, state: &Self::State) -> Option<Self::State>;
    /// A bare `store_conditional(...)` effect statement: the reservation-gated
    /// memory write applies, the `bits<1>` success value is discarded.
    fn store_conditional(&self, c: &ast::Call, state: &Self::State) -> Option<Self::State>;
    /// A `fence(pred, succ)`/`fence_i()` effect statement.
    fn fence(&self, c: &ast::Call, state: &Self::State) -> Option<Self::State>;
    /// A `trap(args...)` statement: the ISA's trap-entry sequence, compiled
    /// against the current state via `compile`.
    fn trap(
        &self,
        c: &ast::Call,
        state: &Self::State,
        compile: &dyn Fn(&ast::Expr, &Self::State) -> Self::State,
    ) -> Option<Self::State>;
    fn ite(
        &self,
        cond: &Self::Condition,
        then_state: &Self::State,
        else_state: &Self::State,
    ) -> Self::State;
    /// Assemble a try/except from the already-compiled no-trap `body_state`;
    /// handler bodies are compiled against the entry state via `compile`,
    /// giving them precise-trap semantics.
    fn try_except(
        &self,
        t: &ast::TryExcept,
        state: &Self::State,
        body_state: &Self::State,
        compile: &dyn Fn(&ast::Expr, &Self::State) -> Self::State,
    ) -> Option<Self::State>;
    fn unsupported(&self);
}

fn is_store_call(e: &ast::Expr) -> bool {
    matches!(
        e,
        ast::Expr::Call(c) if matches!(
            &*c.callee,
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::Store)
        )
    )
}

fn is_trap_call(e: &ast::Expr) -> bool {
    matches!(
        e,
        ast::Expr::Call(c) if matches!(
            &*c.callee,
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::Trap)
        )
    )
}

fn is_store_conditional_call(e: &ast::Expr) -> bool {
    matches!(
        e,
        ast::Expr::Call(c) if matches!(
            &*c.callee,
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::StoreConditional)
        )
    )
}

fn is_fence_call(e: &ast::Expr) -> bool {
    matches!(
        e,
        ast::Expr::Call(c) if matches!(
            &*c.callee,
            ast::Expr::BuiltinFunction(ast::BuiltinFunction::Fence | ast::BuiltinFunction::FenceI)
        )
    )
}

pub fn lower_effect(expr: &ast::Expr) -> Option<Effect<'_>> {
    match expr {
        ast::Expr::Assign(assign) => Some(Effect::Assign(assign)),
        ast::Expr::Call(_) if is_store_call(expr) => Some(Effect::Store(expr)),
        ast::Expr::Call(_) if is_store_conditional_call(expr) => {
            Some(Effect::StoreConditional(expr))
        }
        ast::Expr::Call(_) if is_fence_call(expr) => Some(Effect::Fence(expr)),
        ast::Expr::Call(_) if is_trap_call(expr) => Some(Effect::Trap(expr)),
        ast::Expr::Block(block) => Some(Effect::Block(
            block
                .stmts
                .iter()
                .map(lower_effect)
                .collect::<Option<Vec<_>>>()?,
        )),
        ast::Expr::If(if_expr) => Some(Effect::If {
            cond: &if_expr.cond,
            then_effect: Box::new(lower_effect(&if_expr.then)?),
            else_effect: match if_expr.else_.as_deref() {
                Some(expr) => Some(Box::new(lower_effect(expr)?)),
                None => None,
            },
        }),
        ast::Expr::Try(try_expr) => Some(Effect::Try {
            source: try_expr,
            body: Box::new(lower_effect(&try_expr.body)?),
        }),
        _ => None,
    }
}

pub fn compile_to_state<E: StateEmitter>(
    expr: &ast::Expr,
    state: &E::State,
    emitter: &E,
) -> E::State {
    let Some(effect) = lower_effect(expr) else {
        emitter.unsupported();
        return state.clone();
    };
    compile_effect_to_state(&effect, state, emitter)
}

fn compile_effect_to_state<E: StateEmitter>(
    effect: &Effect<'_>,
    state: &E::State,
    emitter: &E,
) -> E::State {
    let or_unsupported = |result: Option<E::State>| {
        result.unwrap_or_else(|| {
            emitter.unsupported();
            state.clone()
        })
    };
    match effect {
        Effect::Assign(assign) => or_unsupported(emitter.assign(assign, state)),
        Effect::Store(ast::Expr::Call(call)) => or_unsupported(emitter.store(call, state)),
        Effect::StoreConditional(ast::Expr::Call(call)) => {
            or_unsupported(emitter.store_conditional(call, state))
        }
        Effect::Fence(ast::Expr::Call(call)) => or_unsupported(emitter.fence(call, state)),
        Effect::Trap(ast::Expr::Call(call)) => {
            or_unsupported(emitter.trap(call, state, &|expr, state| {
                compile_to_state(expr, state, emitter)
            }))
        }
        Effect::Block(effects) => {
            let mut current = state.clone();
            for effect in effects {
                current = compile_effect_to_state(effect, &current, emitter);
            }
            current
        }
        Effect::If {
            cond,
            then_effect,
            else_effect,
        } => {
            let cond = emitter.cond(cond);
            let then_state = compile_effect_to_state(then_effect, state, emitter);
            let else_state = if let Some(effect) = else_effect {
                compile_effect_to_state(effect, state, emitter)
            } else {
                state.clone()
            };
            emitter.ite(&cond, &then_state, &else_state)
        }
        Effect::Try { source, body } => {
            let body_state = compile_effect_to_state(body, state, emitter);
            or_unsupported(
                emitter.try_except(source, state, &body_state, &|expr, state| {
                    compile_to_state(expr, state, emitter)
                }),
            )
        }
        _ => unreachable!("call effects are always backed by call expressions"),
    }
}
