//! Format-neutral building blocks for object-file emission.
//!
//! TMDL-generated encoders turn a machine instruction into bytes plus a list
//! of fixups for operands whose value is not known at encode time (branch
//! targets, external symbols). Patchers re-scatter a resolved value into the
//! instruction's immediate bits once layout is known.

use tir::{BlockId, OpInstance};

/// What an unresolved instruction operand points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixupTarget {
    /// A basic block in the same symbol; resolved to a pc-relative delta
    /// during layout.
    Block(BlockId),
    /// A named symbol; becomes a relocation if it cannot be resolved locally.
    Symbol(String),
}

/// An operand left as zero bits in the encoded instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstFixup {
    /// TMDL operand name the fixup applies to (e.g. `"imm"`).
    pub operand: &'static str,
    pub target: FixupTarget,
}

/// One encoded machine instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedInst {
    /// Little-endian instruction bytes; fixup bits are zero.
    pub bytes: Vec<u8>,
    pub fixups: Vec<InstFixup>,
}

/// Encodes one operation. `None` means the operation cannot be encoded
/// (e.g. a virtual register survived register allocation).
pub type InstructionEncoder = fn(&OpInstance) -> Option<EncodedInst>;

/// Scatters a resolved fixup value into the instruction bytes. `None` means
/// the value does not fit the operand's encoding (out of range or misaligned).
pub type InstructionPatcher = fn(&mut [u8], i64) -> Option<()>;
