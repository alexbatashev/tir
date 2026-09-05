//! Where a non-local exit goes: the structured op a `break` or `continue`
//! leaves, found by walking out through the ops enclosing it.

use crate::{Callable, Context, Error, ExitScope, ExitScopeKind, ExitTarget, NonLocalExit, OpId};

/// The op `exit` leaves: the nearest enclosing loop or switch of the kind its
/// target names, or the nearest enclosing scope carrying its label. The walk
/// stops at the enclosing callable, so an exit with nothing to leave is an
/// error rather than a jump out of the function.
pub fn resolve_exit_target(context: &Context, exit: OpId) -> Result<OpId, Error> {
    let handle = context.get_op(exit);
    let spelled = format!("{}.{}", handle.dialect(), handle.name());
    let target = handle
        .clone()
        .as_interface::<dyn NonLocalExit>()
        .ok_or_else(|| Error::VerificationError(format!("{spelled} is not a non-local exit")))?
        .target();
    let mut current = context.parent_op(exit);
    while let Some(op) = current {
        let ancestor = context.get_op(op);
        if let Some(scope) = ancestor.clone().as_interface::<dyn ExitScope>() {
            let found = match &target {
                ExitTarget::InnermostLoop => scope.exit_scope() == ExitScopeKind::Loop,
                ExitTarget::InnermostSwitch => scope.exit_scope() == ExitScopeKind::Switch,
                ExitTarget::Label(label) => scope.label().as_deref() == Some(label),
            };
            if found {
                return Ok(op);
            }
        }
        if ancestor.has_interface::<dyn Callable>() {
            break;
        }
        current = context.parent_op(op);
    }
    let wanted = match target {
        ExitTarget::InnermostLoop => "loop".to_string(),
        ExitTarget::InnermostSwitch => "switch".to_string(),
        ExitTarget::Label(label) => format!("scope labeled {label}"),
    };
    Err(Error::VerificationError(format!(
        "{spelled} leaves no enclosing {wanted}"
    )))
}
