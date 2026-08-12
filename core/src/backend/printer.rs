use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::sync::Arc;

use tir::attributes::AttributeValue;
use tir::builtin::{DeclareOp, ModuleEndOp, ModuleOp};
use tir::{Context, OpInstance, Operation};

use crate::backend::{
    BlockEndOp, DataRelocOp, LiteralOp, MachineInstruction, SectionEndOp, SectionOp, SymbolEndOp,
    SymbolOp,
};

pub type AsmInstructionPrinter = fn(&Context, &OpInstance) -> Option<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmPrintError {
    MissingSymbolName,
    MissingInstructionPrinter { op: &'static str },
    InvalidInstruction { op: &'static str },
    UnsupportedOp { op: &'static str },
}

impl Display for AsmPrintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsmPrintError::MissingSymbolName => write!(f, "asm symbol is missing name"),
            AsmPrintError::MissingInstructionPrinter { op } => {
                write!(f, "no assembly printer registered for '{op}'")
            }
            AsmPrintError::InvalidInstruction { op } => {
                write!(f, "assembly printer rejected '{op}'")
            }
            AsmPrintError::UnsupportedOp { op } => {
                write!(f, "cannot print '{op}' as assembly")
            }
        }
    }
}

impl Error for AsmPrintError {}

pub struct AsmPrinter {
    instruction_printers: HashMap<String, AsmInstructionPrinter>,
}

impl AsmPrinter {
    pub fn new(instruction_printers: HashMap<String, AsmInstructionPrinter>) -> Self {
        Self {
            instruction_printers,
        }
    }

    pub fn print_instruction(
        &self,
        context: &Context,
        op: &OpInstance,
    ) -> Result<Option<String>, AsmPrintError> {
        let Some(printer) = self.instruction_printers.get(op.name().as_str()) else {
            return Ok(None);
        };
        printer(context, op)
            .map(Some)
            .ok_or(AsmPrintError::InvalidInstruction {
                op: op.name().as_str(),
            })
    }

    pub fn print_module(
        &self,
        context: &Context,
        module: &ModuleOp,
    ) -> Result<String, AsmPrintError> {
        let mut out = String::new();
        self.print_block(context, module.body(), &mut out)?;
        Ok(out)
    }

    fn print_block(
        &self,
        context: &Context,
        block: Arc<tir::Block>,
        out: &mut String,
    ) -> Result<(), AsmPrintError> {
        for op_id in block.op_ids() {
            let op = context.get_op(op_id);
            if op.is::<ModuleEndOp>()
                || op.is::<SectionEndOp>()
                || op.is::<SymbolEndOp>()
                || op.is::<BlockEndOp>()
                // External declarations produce no assembly; references resolve
                // at link time.
                || op.is::<DeclareOp>()
            {
                continue;
            }

            if let Some(section) = op.clone().as_op::<SectionOp>() {
                let name = string_attr(&op, "name").unwrap_or(".text");
                if name == ".text" {
                    out.push_str(".text\n");
                } else {
                    out.push_str(".section ");
                    out.push_str(name);
                    out.push('\n');
                }
                self.print_block(context, section.body(), out)?;
                continue;
            }

            if op.clone().as_op::<SymbolOp>().is_some() {
                let name = string_attr(&op, "name").ok_or(AsmPrintError::MissingSymbolName)?;
                if string_attr(&op, "binding") != Some("local") {
                    out.push_str(".global ");
                    out.push_str(name);
                    out.push('\n');
                }
                if let Some(align) = int_attr(&op, "align")
                    && align > 1
                {
                    out.push_str("\t.balign ");
                    out.push_str(&align.to_string());
                    out.push('\n');
                }
                out.push_str(name);
                out.push_str(":\n");
                // The symbol label above names the entry block, so only non-entry
                // blocks emit their own label (branch targets must be defined).
                let region = context.get_region(op.regions[0]);
                for (index, block) in region.iter(context.clone()).enumerate() {
                    if index > 0 {
                        match block.attr("name") {
                            Some(AttributeValue::Str(label)) => out.push_str(&label),
                            _ => {
                                out.push_str(".L");
                                out.push_str(&block.id().number().to_string());
                            }
                        }
                        out.push_str(":\n");
                    }
                    self.print_block(context, block, out)?;
                }
                continue;
            }

            if op.clone().as_op::<LiteralOp>().is_some() {
                let kind = string_attr(&op, "kind").ok_or(AsmPrintError::UnsupportedOp {
                    op: LiteralOp::name(),
                })?;
                out.push_str("\t.");
                out.push_str(kind);
                match kind {
                    "byte" | "half" | "word" | "dword" | "space" => {
                        let value = int_attr(&op, "value").ok_or(AsmPrintError::UnsupportedOp {
                            op: LiteralOp::name(),
                        })?;
                        out.push(' ');
                        out.push_str(&value.to_string());
                        out.push('\n');
                    }
                    _ => {
                        let value =
                            string_attr(&op, "value").ok_or(AsmPrintError::UnsupportedOp {
                                op: LiteralOp::name(),
                            })?;
                        out.push_str(" \"");
                        out.push_str(&escape_asm_string(value));
                        out.push_str("\"\n");
                    }
                }
                continue;
            }

            if op.clone().as_op::<DataRelocOp>().is_some() {
                let unsupported = || AsmPrintError::UnsupportedOp {
                    op: DataRelocOp::name(),
                };
                let symbol = string_attr(&op, "symbol").ok_or_else(unsupported)?;
                let directive = match int_attr(&op, "width") {
                    Some(4) => "word",
                    Some(8) => "quad",
                    _ => return Err(unsupported()),
                };
                let addend = int_attr(&op, "addend").ok_or_else(unsupported)?;
                out.push_str("\t.");
                out.push_str(directive);
                out.push(' ');
                out.push_str(symbol);
                if addend > 0 {
                    out.push('+');
                    out.push_str(&addend.to_string());
                } else if addend < 0 {
                    out.push_str(&addend.to_string());
                }
                out.push('\n');
                continue;
            }

            if let Some(text) = self.print_instruction(context, &op)? {
                out.push('\t');
                out.push_str(&text);
                out.push('\n');
                continue;
            }

            if op
                .clone()
                .as_interface::<dyn MachineInstruction>()
                .is_some()
            {
                return Err(AsmPrintError::MissingInstructionPrinter {
                    op: op.name().as_str(),
                });
            }

            return Err(AsmPrintError::UnsupportedOp {
                op: op.name().as_str(),
            });
        }
        Ok(())
    }
}

/// Escape a literal for a quoted assembler string directive.
fn escape_asm_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn int_attr(op: &OpInstance, name: &str) -> Option<i64> {
    op.attr(name).and_then(AttributeValue::as_int)
}

fn string_attr<'a>(op: &'a OpInstance, name: &str) -> Option<&'a str> {
    match op.attr(name)? {
        AttributeValue::Str(value) => Some(value.as_str()),
        _ => None,
    }
}
