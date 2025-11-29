use tir::helpers::{dialect, operation};

include!(concat!(env!("OUT_DIR"), "/riscv.rs"));

dialect! {
    RiscvDialect {
        name: "riscv",
        operations: [
            // RV32I
            AddOp,
            SubOp,
            ShiftLeftLogicalOp,
            // ShiftRightLogicalOp,
            XorOp,
            AndOp,
            OrOp
        ],
    }
}

impl RiscvDialect {
    pub fn get_asm_parser(&self) -> tir_be_common::AsmParser {
        let parser = tir_be_common::AsmParser::new(get_instruction_parsers());
        parser
    }
}

#[cfg(test)]
mod tests {
    use tir::{Context, IRFormatter, Operation};
    use tir_be_common::AsmDialect;

    use crate::RiscvDialect;

    #[test]
    fn smoke_parser() {
        let context = Context::with_default_dialects();
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();
        let dialect = context.find_dialect::<RiscvDialect>().unwrap();
        let asm_parser = dialect.get_asm_parser();

        let test = "
        .global foo
        entry:
            add a0, a1, a2
        ";

        let module = asm_parser.parse_asm(&context, test);
        let module = module.unwrap();

        let mut new_buf = String::new();
        let mut f = IRFormatter::new(&mut new_buf);
        module.print(&mut f).expect("Failed to print module");
        insta::assert_snapshot!(&new_buf);
    }
}
