use tir::helpers::{dialect, operation};

include!(concat!(env!("OUT_DIR"), "/riscv.rs"));

operation! {
    VirtualReturnOp {
        name: "vret",
        dialect: "riscv",
        operands: [value],
    }
}

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
            OrOp,
            VirtualReturnOp
        ],
    }
}

pub mod ops {
    pub use super::*;
}

impl RiscvDialect {
    pub fn get_asm_parser(&self) -> tir_be_common::AsmParser {
        let parser = tir_be_common::AsmParser::new(get_instruction_parsers());
        parser
    }
}

fn lower_func_and_return_to_asm_symbol(
    context: &tir::Context,
    op: &tir::OperationRef,
    rewriter: &mut tir::Rewriter,
) -> Result<bool, tir::PassError> {
    use tir::Operation;
    use tir::attributes::{AttributeValue, RegisterAttr};
    use tir::builtin::{FuncOp, ReturnOp};

    if let Some(func) = op.as_op::<FuncOp>() {
        // asm.symbol regions require an explicit symbol_end terminator.
        let body = func.body();
        let has_symbol_end = body
            .op_ids()
            .last()
            .map(|id| context.get_op(*id).name == tir_be_common::SymbolEndOp::name())
            .unwrap_or(false);
        if !has_symbol_end {
            let mut b = tir::IRBuilder::new(body);
            b.insert(tir_be_common::SymbolEndOpBuilder::new(context).build());
        }

        let sym_name = func
            .attributes()
            .iter()
            .find(|a| a.name == "sym_name")
            .and_then(|a| match &a.value {
                AttributeValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string());

        let arg_regs = func
            .body()
            .arguments()
            .iter()
            .map(|arg| {
                AttributeValue::Register(RegisterAttr::Virtual {
                    id: arg.id().number(),
                    class: Some("GPR".to_string()),
                })
            })
            .collect::<Vec<_>>();

        let lowered = tir_be_common::SymbolOpBuilder::new(context)
            .body(op.op().regions[0])
            .attr("name", AttributeValue::Str(sym_name))
            .attr("arg_regs", AttributeValue::Array(arg_regs))
            .build();
        rewriter.replace_op(op, &lowered)?;
        return Ok(true);
    }

    if let Some(ret) = op.as_op::<ReturnOp>() {
        let mut builder = VirtualReturnOpBuilder::new(context);
        if let Some(value) = ret.operands().first().copied() {
            builder = builder.value(value);
        }
        let lowered = builder.build();
        rewriter.replace_op(op, &lowered)?;
        return Ok(true);
    }

    Ok(false)
}

pub fn create_isel_pass() -> tir_be_common::isel::InstructionSelectPass {
    tir_be_common::isel::InstructionSelectPass::new(get_isel_rules())
        .with_op_lowering(lower_func_and_return_to_asm_symbol)
}

#[cfg(test)]
mod tests {
    use tir::{
        Context, IRBuilder, IRFormatter, Operation, PassManager, Type,
        builtin::{FuncOp, ops},
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
        context.register_dialect::<AsmDialect>();
        context.register_dialect::<RiscvDialect>();

        let module = ops::module(&context, None).build();

        let param0 = context.create_value(Type::Integer { width: 32 }, None);
        let param1 = context.create_value(Type::Integer { width: 32 }, None);
        let region = context.create_region();
        let block = context.create_block(vec![param0, param1]);
        region.add_block(block.id());

        let func = ops::func(
            &context,
            "demo",
            Type::Integer { width: 32 },
            Some(region.id()),
        )
        .build();

        let mut fb = IRBuilder::new(func.body());
        let add = ops::addi(
            &context,
            func.body().arguments()[0].id(),
            func.body().arguments()[1].id(),
            Type::Integer { width: 32 },
        )
        .build();
        let add_result = add.result();
        fb.insert(add);
        fb.insert(ops::r#return(&context, add_result).build());

        let mut mb = IRBuilder::new(module.body());
        let _func = mb.insert(func);
        mb.insert(ops::module_end(&context).build());

        module.verify(&context).expect("Invalid module");
        // assert!(module.verify(&context).is_ok());
        let mut pm = PassManager::new();
        pm.nest(FuncOp::name()).add_pass(create_isel_pass());
        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");
        assert!(module.verify(&context).is_ok());

        let mut buf = String::new();
        let mut fmt = IRFormatter::new(&mut buf);
        module.print(&mut fmt).expect("print lowered module");
        insta::assert_snapshot!("builtin_add_lowers_to_riscv_add_ir", &buf);
    }
}
