use crate::operation;

use crate as tir;

operation! {
    ConstantOp {
        name: "constant",
        dialect: "builtin",
        attributes: A {
            value: "Int",
        },
        results: [result],
    }
}

impl ConstantOpBuilder {
    pub fn value(self, v: i64) -> Self {
        self.attr("value", tir::attributes::AttributeValue::Int(v))
    }
}

operation! {
    AddIOp {
        name: "addi",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (add lhs rhs))",
    }
}

operation! {
    SubIOp {
        name: "subi",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (sub lhs rhs))",
    }
}

operation! {
    MulIOp {
        name: "muli",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (mul lhs rhs))",
    }
}

operation! {
    AndIOp {
        name: "andi",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (and lhs rhs))",
    }
}

operation! {
    OrIOp {
        name: "ori",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (or lhs rhs))",
    }
}

operation! {
    XOrIOp {
        name: "xori",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (xor lhs rhs))",
    }
}

operation! {
    ShlIOp {
        name: "shli",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (shl lhs rhs))",
    }
}

operation! {
    ShrUIOp {
        name: "shrui",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (lshr lhs rhs))",
    }
}

operation! {
    ShrSIOp {
        name: "shrsi",
        dialect: "builtin",
        operands: [lhs, rhs],
        results: [result],
        sem: "(set result (ashr lhs rhs))",
    }
}

operation! {
    CmpIOp {
        name: "cmpi",
        dialect: "builtin",
        attributes: A {
            predicate: "Str",
        },
        operands: [lhs, rhs],
        results: [result],
    }
}

impl CmpIOpBuilder {
    pub fn predicate(self, pred: &str) -> Self {
        self.attr(
            "predicate",
            tir::attributes::AttributeValue::Str(pred.to_string()),
        )
    }
}

operation! {
    ExtSIOp {
        name: "extsi",
        dialect: "builtin",
        operands: [input],
        results: [result],
    }
}

operation! {
    ExtUIOp {
        name: "extui",
        dialect: "builtin",
        operands: [input],
        results: [result],
    }
}

operation! {
    TruncIOp {
        name: "trunci",
        dialect: "builtin",
        operands: [input],
        results: [result],
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, Operation, Type,
        builtin::{
            AddIOp, FuncOp,
            arith::{AddIOpBuilder, ConstantOpBuilder, MulIOpBuilder},
            func::{FuncOpBuilder, ReturnOpBuilder},
        },
        parse::ir::parse_ir,
    };

    #[test]
    fn addi_construction() {
        let context = Context::with_default_dialects();
        let lhs = context.create_value(Type::Integer { width: 32 }, None);
        let rhs = context.create_value(Type::Integer { width: 32 }, None);

        let op = AddIOpBuilder::new(&context)
            .lhs(lhs.id())
            .rhs(rhs.id())
            .result_type(Type::Integer { width: 32 })
            .build();

        assert_eq!(op.operands().len(), 2);
        assert_eq!(
            context.get_value(op.result()).ty(),
            &Type::Integer { width: 32 }
        );
    }

    #[test]
    fn arith_roundtrip_in_func() {
        let context = Context::with_default_dialects();
        let param0 = context.create_value(Type::Integer { width: 32 }, None);
        let param1 = context.create_value(Type::Integer { width: 32 }, None);

        let region = context.create_region();
        let block = context.create_block(vec![param0, param1]);
        region.add_block(block.id());

        let func = FuncOpBuilder::new(&context)
            .sym_name("arith_demo")
            .ret_type(Type::Integer { width: 32 })
            .body(region.id())
            .build();

        let mut builder = IRBuilder::new(func.body());

        let c1 = ConstantOpBuilder::new(&context)
            .value(7)
            .result_type(Type::Integer { width: 32 })
            .build();
        let c2 = ConstantOpBuilder::new(&context)
            .value(6)
            .result_type(Type::Integer { width: 32 })
            .build();
        let mul = MulIOpBuilder::new(&context)
            .lhs(c1.result())
            .rhs(c2.result())
            .result_type(Type::Integer { width: 32 })
            .build();
        let add = AddIOpBuilder::new(&context)
            .lhs(mul.result())
            .rhs(block.arguments()[0].id())
            .result_type(Type::Integer { width: 32 })
            .build();
        let add_result = add.result();

        builder.insert(c1);
        builder.insert(c2);
        builder.insert(mul);
        builder.insert(add);
        builder.insert(ReturnOpBuilder::new(&context).value(add_result).build());

        let mut buf = String::new();
        let mut f = IRFormatter::new(&mut buf);
        func.print(&mut f).expect("print ok");
        assert!(buf.contains("addi"));
        assert!(buf.contains("muli"));
        assert!(buf.contains("constant"));
        assert!(buf.contains("%"));
        assert!(buf.contains(" = "));

        let new_context = Context::with_default_dialects();
        let new_func = parse_ir::<FuncOp>(&new_context, &buf).expect("Failed to parse function");

        let mut new_buf = String::new();
        let mut f = IRFormatter::new(&mut new_buf);
        new_func.print(&mut f).expect("print ok");
        assert_eq!(buf, new_buf);
    }

    #[test]
    fn parse_single_addi_with_result_prefix() {
        let context = Context::with_default_dialects();
        let src = "%2 = addi %0, %1 : i32\n";

        let op = parse_ir::<AddIOp>(&context, src).expect("Failed to parse addi");
        assert_eq!(op.operands().len(), 2);
        assert_eq!(
            context.get_value(op.result()).ty(),
            &Type::Integer { width: 32 }
        );
    }
}
