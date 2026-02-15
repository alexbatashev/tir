use crate::{Context, Operation};

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
