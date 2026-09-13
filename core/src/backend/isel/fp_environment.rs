use tir::{Context, OperationRef, PassError, attributes::AttributeValue, fp};

/// Check a target whose floating instructions require the default environment.
pub fn check_default_fp_environment(
    context: &Context,
    function: &OperationRef,
    target: &str,
) -> Result<(), PassError> {
    if !function.op().is::<tir::func::FuncOp>() {
        return Ok(());
    }
    let mut fixed = None;
    let mut changing = None;
    let mut unsupported = None;
    let mut pending = function.op().regions();
    while let Some(region) = pending.pop() {
        for id in context.get_region(region).op_ids() {
            let op = context.get_op(id);
            pending.extend(op.regions());
            if op.dialect().as_str() != "fp" {
                continue;
            }
            if matches!(
                op.name().as_str(),
                "set_round" | "restore" | "update" | "hold"
            ) {
                changing.get_or_insert(op.name());
            }
            if let Some(AttributeValue::FpSemantics(semantics)) = op.attr("semantics") {
                let (rounding, exceptions) = match *semantics {
                    fp::Semantics::Arithmetic(s) => (Some(s.rounding), s.exceptions),
                    fp::Semantics::IntegerConversion(s) => (Some(s.rounding), s.exceptions),
                    fp::Semantics::Comparison(s) => (None, s.exceptions),
                };
                if matches!(rounding, Some(fp::Rounding::Fixed(_))) {
                    fixed.get_or_insert(op.name());
                }
                if exceptions != fp::Exceptions::Ignore {
                    unsupported.get_or_insert((op.name(), "observable floating-point exceptions"));
                } else if rounding == Some(fp::Rounding::Dynamic) {
                    unsupported.get_or_insert((op.name(), "dynamic rounding"));
                }
            } else if op
                .clone()
                .as_interface::<dyn tir::HasResourceSemantics>()
                .is_some()
            {
                unsupported.get_or_insert((op.name(), "floating-point environment access"));
            }
        }
    }
    if let (Some(fixed), Some(changing)) = (fixed, changing) {
        return Err(PassError::InvalidRuleSet(format!(
            "{target}: fp.{fixed} uses fixed rounding but fp.{changing} changes the floating-point environment"
        )));
    }
    if let Some((op, reason)) = unsupported {
        return Err(PassError::InvalidRuleSet(format!(
            "{target}: fp.{op} does not support {reason}"
        )));
    }
    Ok(())
}
