//! Table-driven instruction encoders and decoders.
//!
//! TMDL emits one spec per instruction instead of a generated function; these
//! engines interpret the spec. Bit placement is described as runs: operand bits
//! `[op_lo, op_lo + width)` map to word bits `[word_lo, word_lo + width)`.

use tir::attributes::{AttributeValue, NamedAttribute, RegisterAttr};
use tir::{Context, OpId, OpInstance};

use crate::backend::binary::{EncodedInst, FixupTarget, InstFixup};
use crate::backend::regalloc::RegClassId;

/// One contiguous run of an operand's bits placed into the encoded word.
#[derive(Debug, Clone, Copy)]
pub struct FieldRun {
    pub op_lo: u16,
    pub word_lo: u16,
    pub width: u16,
}

fn run_mask(width: u16) -> u128 {
    if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    }
}

/// Encode-side scatter: place `value`'s runs into `word`.
fn scatter(word: &mut u128, value: u128, runs: &[FieldRun]) {
    for run in runs {
        *word |= ((value >> run.op_lo as u32) & run_mask(run.width)) << run.word_lo as u32;
    }
}

/// Decode-side gather: rebuild an operand value from its runs in `word`.
fn gather(word: u32, runs: &[FieldRun]) -> u64 {
    let mut value: u64 = 0;
    for run in runs {
        value |= (((word >> run.word_lo as u32) as u64) & run_mask(run.width) as u64)
            << run.op_lo as u32;
    }
    value
}

/// One operand's contribution to an instruction encoding.
pub struct EncodeField {
    pub attr: &'static str,
    /// Immediate fit check for a field narrower than 64 bits: `(min, max)` for
    /// signed spellings, `umax` (exclusive) for unsigned. `None` for full-width
    /// fields and unconstrained operands.
    pub int_range: Option<(i64, i64, u64)>,
    pub runs: &'static [FieldRun],
    pub register: bool,
}

/// The encoding of one instruction: fixed bits plus per-operand field runs.
pub struct EncodeSpec {
    pub const_word: u128,
    pub width_bytes: usize,
    pub fields: &'static [EncodeField],
}

/// Interprets an [`EncodeSpec`]. `None` when an operand cannot be encoded (e.g.
/// a virtual register survived register allocation); symbol/block operands
/// become fixups with their bits left zero.
pub fn encode_with(op: &OpInstance, spec: &EncodeSpec) -> Option<EncodedInst> {
    let mut word = spec.const_word;
    let mut fixups = Vec::new();
    for field in spec.fields {
        if field.register {
            let value = match op.attr(field.attr)? {
                AttributeValue::Register(RegisterAttr::Physical { index, .. }) => *index as u128,
                _ => return None,
            };
            scatter(&mut word, value, field.runs);
            continue;
        }
        // Immediates written in assembly may be spelled signed or unsigned
        // (`-1` vs `0xFFF`), so accept either fit within the declared width.
        match op.attr(field.attr)? {
            AttributeValue::Int(v) => {
                if let Some((min, max, _)) = field.int_range
                    && !(min..max).contains(v)
                {
                    return None;
                }
                scatter(&mut word, *v as u128, field.runs);
            }
            AttributeValue::UInt(v) => {
                if let Some((_, _, umax)) = field.int_range
                    && *v >= umax
                {
                    return None;
                }
                scatter(&mut word, *v as u128, field.runs);
            }
            AttributeValue::Str(s) => fixups.push(InstFixup {
                operand: field.attr,
                target: FixupTarget::Symbol(s.to_string()),
            }),
            AttributeValue::Block(b) => fixups.push(InstFixup {
                operand: field.attr,
                target: FixupTarget::Block(*b),
            }),
            _ => return None,
        }
    }
    Some(EncodedInst {
        bytes: word.to_le_bytes()[..spec.width_bytes].to_vec(),
        fixups,
    })
}

/// Re-scatter of a resolved fixup value into an instruction's immediate field.
pub struct PatchSpec {
    /// Signed fit check for the value; `None` for full-width fields.
    pub range: Option<(i64, i64)>,
    /// Operand bits below the lowest encoded bit are silently dropped by the
    /// scatter (e.g. bit 0 of RISC-V branch offsets); a value with any of them
    /// set cannot be represented.
    pub dropped_mask: u128,
    pub width_bytes: usize,
    pub runs: &'static [FieldRun],
}

/// Interprets a [`PatchSpec`]. `None` when the value does not fit the operand's
/// encoding (out of range or misaligned).
pub fn patch_with(bytes: &mut [u8], value: i64, spec: &PatchSpec) -> Option<()> {
    if let Some((min, max)) = spec.range
        && !(min..max).contains(&value)
    {
        return None;
    }
    if spec.dropped_mask != 0 && (value as u128) & spec.dropped_mask != 0 {
        return None;
    }
    if bytes.len() < spec.width_bytes {
        return None;
    }
    let mut word: u128 = 0;
    for (i, b) in bytes.iter().enumerate().take(spec.width_bytes) {
        word |= (*b as u128) << (8 * i);
    }
    scatter(&mut word, value as u128, spec.runs);
    let out = word.to_le_bytes();
    bytes[..spec.width_bytes].copy_from_slice(&out[..spec.width_bytes]);
    Some(())
}

/// What a decoded field becomes: a physical register of a fixed class, or a
/// raw integer immediate.
#[derive(Clone, Copy)]
pub enum DecodeFieldKind {
    Register(RegClassId),
    Int,
}

/// One operand's reconstruction from the encoded word.
#[derive(Clone, Copy)]
pub struct DecodeField {
    pub attr: &'static str,
    pub kind: DecodeFieldKind,
    pub runs: &'static [FieldRun],
}

/// The decoding of one instruction: fixed-bit match plus per-operand field
/// runs. Only emitted for instructions the generator can invert.
pub struct DecodeSpec {
    /// `(dialect, op)` identity of the operation to build.
    pub op: (&'static str, &'static str),
    pub fixed_mask: u32,
    pub const_word: u32,
    pub fields: &'static [DecodeField],
    /// Every attribute the op declares; decoding fills all of them.
    pub attrs: &'static [&'static str],
}

/// Interprets a [`DecodeSpec`]: matches the fixed bits, rebuilds each operand,
/// and builds the op in `context`.
pub fn decode_with(context: &Context, word: u32, spec: &DecodeSpec) -> Option<OpId> {
    if word & spec.fixed_mask != spec.const_word {
        return None;
    }
    let attributes: Vec<NamedAttribute> = spec
        .fields
        .iter()
        .map(|field| {
            let value = gather(word, field.runs);
            let attr = match field.kind {
                DecodeFieldKind::Register(class) => {
                    AttributeValue::Register(RegisterAttr::Physical {
                        class,
                        index: value as u16,
                    })
                }
                DecodeFieldKind::Int => AttributeValue::Int(value as i64),
            };
            context.named_attribute(field.attr, attr)
        })
        .collect();
    for declared in spec.attrs {
        if !attributes
            .iter()
            .any(|a| Some(a.name) == context.sym(declared))
        {
            panic!("Missing required attribute: {declared}");
        }
    }
    let instance = OpInstance::new_dynamic(
        spec.op,
        context.as_context_ref(),
        vec![],
        vec![],
        vec![],
        attributes,
    );
    Some(context.add_operation(instance).id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::regalloc::{RegClassInfo, RegisterView};

    static R_CLASS: RegClassInfo = RegClassInfo {
        name: "R",
        file: "R",
        registers: &[0, 1, 2, 3, 4, 5, 6, 7],
        group_width: 1,
        view: RegisterView {
            bit_offset: 0,
            merge: false,
        },
    };

    fn phys(index: u16) -> AttributeValue {
        AttributeValue::Register(RegisterAttr::Physical {
            class: RegClassId::new(&R_CLASS),
            index,
        })
    }

    fn op_with(attrs: Vec<(&str, AttributeValue)>) -> (Context, OpInstance) {
        let context = Context::with_default_dialects();
        let attributes = attrs
            .into_iter()
            .map(|(name, value)| context.named_attribute(name, value))
            .collect();
        let instance = OpInstance::new_dynamic(
            ("test", "inst"),
            context.as_context_ref(),
            vec![],
            vec![],
            vec![],
            attributes,
        );
        (context, instance)
    }

    // `lui rd, imm`: 55 | rd << 7 | (imm & 0xFFFFF) << 12
    const LUI: EncodeSpec = EncodeSpec {
        const_word: 55,
        width_bytes: 4,
        fields: &[
            EncodeField {
                attr: "rd",
                int_range: None,
                runs: &[FieldRun {
                    op_lo: 0,
                    word_lo: 7,
                    width: 5,
                }],
                register: true,
            },
            EncodeField {
                attr: "imm",
                int_range: Some((-524288, 1048576, 1048576)),
                runs: &[FieldRun {
                    op_lo: 0,
                    word_lo: 12,
                    width: 20,
                }],
                register: false,
            },
        ],
    };

    #[test]
    fn encode_scatters_register_and_immediate() {
        let (_context, op) = op_with(vec![("rd", phys(5)), ("imm", AttributeValue::Int(0x12345))]);
        let encoded = encode_with(&op, &LUI).unwrap();
        assert_eq!(
            encoded.bytes,
            (55u32 | (5 << 7) | (0x12345 << 12)).to_le_bytes()
        );
        assert!(encoded.fixups.is_empty());
    }

    #[test]
    fn encode_rejects_out_of_range_and_virtual() {
        let (_context, op) = op_with(vec![("rd", phys(5)), ("imm", AttributeValue::Int(1048576))]);
        assert!(encode_with(&op, &LUI).is_none());

        let (_context, op) = op_with(vec![
            (
                "rd",
                AttributeValue::Register(RegisterAttr::Virtual { id: 0, class: None }),
            ),
            ("imm", AttributeValue::Int(1)),
        ]);
        assert!(encode_with(&op, &LUI).is_none());
    }

    #[test]
    fn encode_leaves_symbol_operand_as_fixup() {
        let (_context, op) = op_with(vec![
            ("rd", phys(5)),
            ("imm", AttributeValue::Str("g".into())),
        ]);
        let encoded = encode_with(&op, &LUI).unwrap();
        assert_eq!(encoded.bytes, (55u32 | (5 << 7)).to_le_bytes());
        assert_eq!(
            encoded.fixups,
            vec![InstFixup {
                operand: "imm",
                target: FixupTarget::Symbol("g".to_string()),
            }]
        );
    }

    // `beq rs1, rs2, imm`: 99 | 0 << 12 | rs1 << 15 | rs2 << 20, imm scattered.
    const BEQ_PATCH: PatchSpec = PatchSpec {
        range: Some((-4096, 4096)),
        dropped_mask: 1,
        width_bytes: 4,
        runs: &[
            FieldRun {
                op_lo: 11,
                word_lo: 7,
                width: 1,
            },
            FieldRun {
                op_lo: 1,
                word_lo: 8,
                width: 4,
            },
            FieldRun {
                op_lo: 5,
                word_lo: 25,
                width: 6,
            },
            FieldRun {
                op_lo: 12,
                word_lo: 31,
                width: 1,
            },
        ],
    };

    #[test]
    fn patch_scatters_resolved_value() {
        let mut bytes = 99u32.to_le_bytes();
        assert!(patch_with(&mut bytes, 16, &BEQ_PATCH).is_some());
        let word = u32::from_le_bytes(bytes);
        assert_eq!(word & 28799, 99, "fixed bits untouched");
        assert_eq!(gather(word & !28799, BEQ_PATCH.runs), 16);
    }

    #[test]
    fn patch_rejects_unrepresentable_values() {
        let mut bytes = 99u32.to_le_bytes();
        assert!(patch_with(&mut bytes, 4096, &BEQ_PATCH).is_none());
        assert!(patch_with(&mut bytes, 15, &BEQ_PATCH).is_none());
        assert!(patch_with(&mut bytes[..2], 16, &BEQ_PATCH).is_none());
    }

    const BEQ_DECODE: DecodeSpec = DecodeSpec {
        op: ("test", "beq"),
        fixed_mask: 28799,
        const_word: 99,
        fields: &[
            DecodeField {
                attr: "rs1",
                kind: DecodeFieldKind::Register(RegClassId::new(&R_CLASS)),
                runs: &[FieldRun {
                    op_lo: 0,
                    word_lo: 15,
                    width: 5,
                }],
            },
            DecodeField {
                attr: "imm",
                kind: DecodeFieldKind::Int,
                runs: BEQ_PATCH.runs,
            },
        ],
        attrs: &["rs1", "imm"],
    };

    #[test]
    fn decode_matches_and_gathers() {
        let context = Context::with_default_dialects();
        let word = 99u32 | (5 << 15) | (1 << 8); // rs1 = 5, imm bit1 set
        let id = decode_with(&context, word, &BEQ_DECODE).expect("decodes");
        let op = context.get_op(id);
        assert_eq!(op.dialect().as_str(), "test");
        assert_eq!(op.name().as_str(), "beq");
        assert_eq!(op.attr("rs1"), Some(&phys(5)),);
        assert_eq!(op.attr("imm"), Some(&AttributeValue::Int(2)));

        assert!(decode_with(&context, word | 1 << 14, &BEQ_DECODE).is_none());
    }
}
