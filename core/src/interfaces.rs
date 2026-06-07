use crate::sem_expr::Value;
use crate::utils::APInt;
use crate::{BlockId, Context, Operation, RegionId, ValueId};

/// An operation whose nested regions execute under a known fact about a value — e.g.
/// a structured `if` whose then/else bodies run when the condition is true/false.
/// Lets a flow-sensitive rewriter assume that fact inside the region without knowing
/// the concrete control-flow op.
pub trait RegionGuard {
    /// For each guarded region, the value known to equal a boolean inside it
    /// (`true` => 1, `false` => 0).
    fn guarded_regions(&self) -> Vec<(RegionId, ValueId, bool)>;
    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Relative execution cost of an operation, consulted by cost-driven rewriters
/// (e.g. InstCombine) to choose among equivalent forms. The default models one
/// cheap instruction; expensive ops override it. Exposed as an interface so the
/// cost is reachable from a `dyn Operation` without the concrete type.
pub trait OpCost {
    fn cost(&self) -> u32 {
        1
    }
    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// An operation that yields a compile-time constant integer, exposed generically so
/// rewriters can read the value without knowing the concrete constant op.
pub trait ConstantLike {
    fn constant_value(&self) -> APInt;
    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Folds an operation over constant operands. The `operation!` macro derives this
/// automatically for any op that declares `sem` (evaluating it through the
/// semantic interpreter); ops that fold but lack a semantic expression implement it
/// by hand.
pub trait ConstantFold {
    /// `operands[i]` is the constant value of operand `i`. Returns the folded
    /// result, or `None` when this op cannot fold these operands.
    fn fold(&self, operands: &[Value]) -> Option<Value>;
    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

pub trait Commutative {
    fn is_commutative(&self) -> bool {
        true
    }
    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

pub trait Terminator {
    fn is_terminator(&self) -> bool {
        true
    }

    fn successors(&self) -> Vec<BlockId> {
        Vec::new()
    }

    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

pub trait SameOperandType {
    fn verify_interface(
        &self,
        this: &dyn Operation,
        context: &Context,
    ) -> Result<(), crate::Error> {
        if this.operands().is_empty() {
            return Ok(());
        }

        let first_operand = *this.operands().first().unwrap();
        let first_type = context.get_value(first_operand).ty();

        let result = this
            .operands()
            .iter()
            .all(|&operand| context.get_value(operand).ty() == first_type);

        if !result {
            return Err(crate::Error::VerificationError(
                "operand types must be the same".to_string(),
            ));
        }

        Ok(())
    }
}

/// Identifies an operation that creates a memory location eligible for local SSA
/// promotion. Implementations describe the location generically rather than tying
/// mem2reg to a concrete pointer dialect.
pub trait PromotableAllocation {
    /// The SSA value that names the promotable memory location.
    fn promoted_location(&self) -> ValueId;

    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Identifies an operation that reads a value from a memory location.
pub trait MemoryRead {
    /// The memory location being read.
    fn read_location(&self) -> ValueId;
    /// The SSA value produced by the read.
    fn read_value(&self) -> ValueId;

    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}

/// Identifies an operation that writes a value to a memory location.
pub trait MemoryWrite {
    /// The memory location being written.
    fn write_location(&self) -> ValueId;
    /// The SSA value stored into the memory location.
    fn written_value(&self) -> ValueId;

    fn verify_interface(
        &self,
        _this: &dyn Operation,
        _context: &Context,
    ) -> Result<(), crate::Error> {
        Ok(())
    }
}
