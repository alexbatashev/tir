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

pub fn create_isel_pass() -> tir_be_common::isel::InstructionSelectPass {
    tir_be_common::isel::InstructionSelectPass::new(get_isel_rules())
}

#[cfg(test)]
mod tests {
    use tir::{
        Context, IRBuilder, IRFormatter, Operation, PassManager, Type,
        builtin::{AddIOpBuilder, FuncOp, FuncOpBuilder, ModuleOpBuilder, ReturnOpBuilder},
    };
    use tir_be_common::AsmDialect;

    use crate::{RiscvDialect, create_isel_pass};

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

    #[test]
    fn builtin_add_lowers_to_riscv_add() {
        let context = Context::with_default_dialects();
        context.register_dialect::<RiscvDialect>();

        let module = ModuleOpBuilder::new(&context).build();

        let param0 = context.create_value(Type::Integer { width: 32 }, None);
        let param1 = context.create_value(Type::Integer { width: 32 }, None);
        let region = context.create_region();
        let block = context.create_block(vec![param0, param1]);
        region.add_block(block.id());

        let func = FuncOpBuilder::new(&context)
            .sym_name("demo")
            .ret_type(Type::Integer { width: 32 })
            .body(region.id())
            .build();

        let mut fb = IRBuilder::new(func.body());
        let add = AddIOpBuilder::new(&context)
            .lhs(func.body().arguments()[0].id())
            .rhs(func.body().arguments()[1].id())
            .result_type(Type::Integer { width: 32 })
            .build();
        let add_result = add.result();
        fb.insert(add);
        fb.insert(ReturnOpBuilder::new(&context).value(add_result).build());

        let mut mb = IRBuilder::new(module.body());
        let func = mb.insert(func);

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name()).add_pass(create_isel_pass());
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let lowered = context.get_op(func.id()).as_op::<FuncOp>().expect("func");
        let mut buf = String::new();
        let mut fmt = IRFormatter::new(&mut buf);
        lowered.print(&mut fmt).expect("print lowered func");
        insta::assert_snapshot!("builtin_add_lowers_to_riscv_add_ir", &buf);
    }
}
