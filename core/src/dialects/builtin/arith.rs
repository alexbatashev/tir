use crate::operation;

use crate as tir;
use crate::{Commutative, SameOperandType};

operation! {
    ConstantOp {
        name: "constant",
        dialect: "builtin",
        attributes: A {
            value: "Int",
        },
        results: R {
            result: "Integer",
        },
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
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [Commutative, SameOperandType],
        sem: "(set result (add lhs rhs))",
    }
}

impl Commutative for AddIOp {}
impl SameOperandType for AddIOp {}

operation! {
    SubIOp {
        name: "subi",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [SameOperandType],
        sem: "(set result (sub lhs rhs))",
    }
}

impl SameOperandType for SubIOp {}

operation! {
    MulIOp {
        name: "muli",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [Commutative, SameOperandType],
        sem: "(set result (mul lhs rhs))",
    }
}

impl Commutative for MulIOp {}
impl SameOperandType for MulIOp {}

operation! {
    AndIOp {
        name: "andi",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [Commutative],
        sem: "(set result (and lhs rhs))",
    }
}

impl Commutative for AndIOp {}

operation! {
    OrIOp {
        name: "ori",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [Commutative, SameOperandType],
        sem: "(set result (or lhs rhs))",
    }
}

impl Commutative for OrIOp {}
impl SameOperandType for OrIOp {}

operation! {
    XOrIOp {
        name: "xori",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        interfaces: [Commutative, SameOperandType],
        sem: "(set result (xor lhs rhs))",
    }
}

impl Commutative for XOrIOp {}
impl SameOperandType for XOrIOp {}

operation! {
    ShlIOp {
        name: "shli",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        sem: "(set result (shl lhs rhs))",
    }
}

operation! {
    ShrUIOp {
        name: "shrui",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
        sem: "(set result (lshr lhs rhs))",
    }
}

operation! {
    ShrSIOp {
        name: "shrsi",
        dialect: "builtin",
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "Integer",
        },
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
        operands: O {
            lhs: "Integer",
            rhs: "Integer",
        },
        results: R {
            result: "i1",
        },
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
        operands: O {
            input: "Integer",
        },
        results: R {
            result: "Integer",
        },
    }
}

operation! {
    ExtUIOp {
        name: "extui",
        dialect: "builtin",
        operands: O {
            input: "Integer",
        },
        results: R {
            result: "Integer",
        },
    }
}

operation! {
    TruncIOp {
        name: "trunci",
        dialect: "builtin",
        operands: O {
            input: "Integer",
        },
        results: R {
            result: "Integer",
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, Operation, Type,
        builtin::{AddIOp, FuncOp, ops},
        parse::ir::parse_ir,
    };

    #[test]
    fn addi_construction() {
        let context = Context::with_default_dialects();
        let lhs = context.create_value(Type::Integer { width: 32 }, None);
        let rhs = context.create_value(Type::Integer { width: 32 }, None);

        let op = ops::addi(&context, lhs.id(), rhs.id(), Type::Integer { width: 32 }).build();

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

        let func = ops::func(
            &context,
            "arith_demo",
            Type::Integer { width: 32 },
            Some(region.id()),
        )
        .build();

        let mut builder = IRBuilder::new(func.body());

        let c1 = ops::constant(&context, 7, Type::Integer { width: 32 }).build();
        let c2 = ops::constant(&context, 6, Type::Integer { width: 32 }).build();
        let mul = ops::muli(
            &context,
            c1.result(),
            c2.result(),
            Type::Integer { width: 32 },
        )
        .build();
        let add = ops::addi(
            &context,
            mul.result(),
            block.arguments()[0].id(),
            Type::Integer { width: 32 },
        )
        .build();
        let add_result = add.result();

        builder.insert(c1);
        builder.insert(c2);
        builder.insert(mul);
        builder.insert(add);
        builder.insert(ops::r#return(&context, add_result).build());

        assert!(func.verify(&context).is_ok());

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
        let c0 = ops::constant(&context, 1, Type::Integer { width: 32 }).build();
        let c1 = ops::constant(&context, 2, Type::Integer { width: 32 }).build();
        let src = format!(
            "%2 = addi %{}, %{} : i32\n",
            c0.result().number(),
            c1.result().number()
        );

        let op = parse_ir::<AddIOp>(&context, &src).expect("Failed to parse addi");
        assert!(op.verify(&context).is_ok());
        assert_eq!(op.operands().len(), 2);
        assert_eq!(
            context.get_value(op.result()).ty(),
            &Type::Integer { width: 32 }
        );
    }

    #[test]
    fn addi_verify_fails_for_mismatched_operand_types() {
        let context = Context::with_default_dialects();
        let lhs = ops::constant(&context, 1, Type::Integer { width: 32 }).build();
        let rhs = ops::constant(&context, 2, Type::Integer { width: 64 }).build();

        let op = ops::addi(
            &context,
            lhs.result(),
            rhs.result(),
            Type::Integer { width: 32 },
        )
        .build();

        let err = op
            .verify(&context)
            .expect_err("expected SameOperandType verification to fail");
        assert!(err.to_string().contains("operand types must be the same"));
    }
}
