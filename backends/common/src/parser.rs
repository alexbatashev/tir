use std::collections::HashMap;

use tir::{
    IRBuilder,
    builtin::{ModuleEndOpBuilder, ModuleOp, ModuleOpBuilder},
    parse::tokens::Parser,
};

use crate::{SectionOpBuilder, SymbolEndOpBuilder, SymbolOpBuilder, lex, lexer::Token};

pub type AsmInstructionParser =
    for<'src> fn(&tir::Context, &mut IRBuilder, &mut Parser<'src, Token<'src>>) -> Result<(), ()>;

pub struct AsmParser {
    instruction_parsers: HashMap<String, Box<AsmInstructionParser>>,
}

impl AsmParser {
    pub fn new(instruction_parsers: HashMap<String, Box<AsmInstructionParser>>) -> Self {
        AsmParser {
            instruction_parsers,
        }
    }

    pub fn parse_asm(&self, context: &tir::Context, src: &str) -> Result<ModuleOp, ()> {
        let module = ModuleOpBuilder::new(&context).build();

        let tokens = lex(src)?;

        let mut parser = Parser::new(&tokens);

        let mut builder = IRBuilder::new(module.body());
        let section_op = builder.insert(SectionOpBuilder::new(context).build());
        builder.insert(ModuleEndOpBuilder::new(context).build());
        let section_body = section_op.body();
        builder.set_insertion_point_to_start(section_body.clone());

        while let Some(token) = parser.peek() {
            match token {
                Token::Global => {
                    let _ = parser.bump();
                    let name = parser.bump();
                    match name {
                        Some(Token::Ident(name)) => {
                            builder.set_insertion_point_to_start(section_body.clone());
                            let global_op = builder.insert(
                                SymbolOpBuilder::new(context)
                                    .attr(
                                        "name",
                                        tir::attributes::AttributeValue::Str((*name).to_string()),
                                    )
                                    .build(),
                            );
                            builder.set_insertion_point_to_start(global_op.body());
                            builder.insert(SymbolEndOpBuilder::new(context).build());
                            builder.set_insertion_point_to_start(global_op.body());
                        }
                        _ => return Err(()),
                    }
                }
                Token::Label(_) => {
                    // FIXME just skip for now, use actual block names in future.
                    let _ = parser.bump();
                }
                Token::Text => {
                    // FIXME set insertion point to end of text section
                }
                Token::Ident(ident) => {
                    // Try to dispatch to an instruction parser by mnemonic
                    let key = ident.to_string();
                    if let Some(handler) = self.instruction_parsers.get(&key) {
                        // consume mnemonic
                        let _ = parser.bump();
                        // parse the rest of the instruction
                        handler(context, &mut builder, &mut parser)?;
                    } else {
                        // Unknown ident in text section; skip it for now
                        let _ = parser.bump();
                    }
                }
                _ => {
                    let _ = parser.bump();
                }
            }
        }

        Ok(module)
    }
}
