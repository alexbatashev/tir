use tir::helpers::{dialect, operation};
use tir::{Any, Operation};

include!(concat!(env!("OUT_DIR"), "/arm64.rs"));

dialect! {
    Arm64Dialect {
        name: "arm64",
        operations: [
            AddOp,
            SubOp,
            AddImmediateOp,
            SubImmediateOp,
            AndOp,
            OrOp,
            XorOp,
            LogicalShiftLeftVariableOp,
            LogicalShiftRightVariableOp,
            ArithmeticShiftRightVariableOp,
            CompareOp,
            LoadByteUnsignedOp,
            LoadHalfwordUnsignedOp,
            LoadWordUnsignedOp,
            LoadDoublewordOp,
            LoadByteSignedOp,
            LoadHalfwordSignedOp,
            LoadWordSignedOp,
            StoreByteOp,
            StoreHalfwordOp,
            StoreWordOp,
            StoreDoublewordOp,
            BranchImmediateOp,
            BranchLinkOp,
            BranchRegisterOp,
            BranchLinkRegOp,
            ReturnOp,
            BranchEqOp,
            BranchNotEqOp,
            BranchLessThanOp,
            BranchGreaterEqOp,
            BranchLowerUnsignedOp,
            BranchHigherOrSameUnsignedOp,
        ],
    }
}
