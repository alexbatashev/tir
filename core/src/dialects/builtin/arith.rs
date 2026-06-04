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
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
        },
        sem: "(set result (shl lhs rhs))",
    }
}

operation! {
    ShrUIOp {
        name: "shrui",
        dialect: "builtin",
        operands: O {
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
        },
        sem: "(set result (lshr lhs rhs))",
    }
}

operation! {
    ShrSIOp {
        name: "shrsi",
        dialect: "builtin",
        operands: O {
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
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
            lhs: "crate::builtin::IntegerType",
            rhs: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::Integer<1>",
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
            input: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
        },
        sem: "(set result (sext input))",
    }
}

operation! {
    ExtUIOp {
        name: "extui",
        dialect: "builtin",
        operands: O {
            input: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
        },
        sem: "(set result (zext input))",
    }
}

operation! {
    TruncIOp {
        name: "trunci",
        dialect: "builtin",
        operands: O {
            input: "crate::builtin::IntegerType",
        },
        results: R {
            result: "crate::builtin::IntegerType",
        },
        sem: "(set result (trunc input))",
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, IRFormatter, Operation,
        builtin::{AddIOp, FuncOp, IntegerType, ops},
        graph::Dag,
        parse::ir::parse_ir,
        sem_expr::{AsSemExpr, ExprKind, ExprPayload, ExprPostGraph},
    };

    #[test]
    fn addi_construction() {
        let context = Context::with_default_dialects();
        let lhs = context.create_value(IntegerType::new(&context, 32), None);
        let rhs = context.create_value(IntegerType::new(&context, 32), None);

        let op = ops::addi(&context, lhs.id(), rhs.id(), IntegerType::new(&context, 32)).build();

        assert_eq!(op.operands().len(), 2);
        assert_eq!(
            context.get_value(op.result()).ty(),
            IntegerType::new(&context, 32)
        );
    }

    #[test]
    fn arith_roundtrip_in_func() {
        let context = Context::with_default_dialects();
        let param0 = context.create_value(IntegerType::new(&context, 32), None);
        let param1 = context.create_value(IntegerType::new(&context, 32), None);

        let region = context.create_region();
        let block = context.create_block(vec![param0, param1]);
        region.add_block(block.id());

        let func = ops::func(
            &context,
            "arith_demo",
            IntegerType::new(&context, 32),
            Some(region.id()),
        )
        .build();

        let mut builder = IRBuilder::new(func.body());

        let c1 = ops::constant(&context, 7, IntegerType::new(&context, 32)).build();
        let c2 = ops::constant(&context, 6, IntegerType::new(&context, 32)).build();
        let mul = ops::muli(
            &context,
            c1.result(),
            c2.result(),
            IntegerType::new(&context, 32),
        )
        .build();
        let add = ops::addi(
            &context,
            mul.result(),
            block.arguments()[0].id(),
            IntegerType::new(&context, 32),
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
        let c0 = ops::constant(&context, 1, IntegerType::new(&context, 32)).build();
        let c1 = ops::constant(&context, 2, IntegerType::new(&context, 32)).build();
        let src = format!(
            "%2 = addi %{}, %{} : !i32\n",
            c0.result().number(),
            c1.result().number()
        );

        let op = parse_ir::<AddIOp>(&context, &src).expect("Failed to parse addi");
        assert!(op.verify(&context).is_ok());
        assert_eq!(op.operands().len(), 2);
        assert_eq!(
            context.get_value(op.result()).ty(),
            IntegerType::new(&context, 32)
        );
    }

    #[test]
    fn addi_verify_fails_for_mismatched_operand_types() {
        let context = Context::with_default_dialects();
        let lhs = ops::constant(&context, 1, IntegerType::new(&context, 32)).build();
        let rhs = ops::constant(&context, 2, IntegerType::new(&context, 64)).build();

        let op = ops::addi(
            &context,
            lhs.result(),
            rhs.result(),
            IntegerType::new(&context, 32),
        )
        .build();

        let err = op
            .verify(&context)
            .expect_err("expected SameOperandType verification to fail");
        assert!(err.to_string().contains("operand types must be the same"));
    }

    fn make_binary_op_context() -> (Context, crate::ValueId, crate::ValueId) {
        let context = Context::with_default_dialects();
        let lhs = context.create_value(IntegerType::new(&context, 32), None);
        let rhs = context.create_value(IntegerType::new(&context, 32), None);
        (context, lhs.id(), rhs.id())
    }

    fn check_binary_sem(g: &ExprPostGraph, root: crate::graph::NodeId, expected_kind: ExprKind) {
        assert_eq!(g.len(), 3, "expected 3 nodes: lhs symbol, rhs symbol, op");
        assert_eq!(g.get_kind(root), &expected_kind);
        let children: Vec<_> = g.children(root).collect();
        assert_eq!(children.len(), 2);
        assert_eq!(g.get_kind(children[0]), &ExprKind::Symbol);
        assert_eq!(g.get_kind(children[1]), &ExprKind::Symbol);
        assert!(
            matches!(g.get_leaf_data(children[0]), Some(ExprPayload::SymbolId(0))),
            "lhs should be symbol 0"
        );
        assert!(
            matches!(g.get_leaf_data(children[1]), Some(ExprPayload::SymbolId(1))),
            "rhs should be symbol 1"
        );
    }

    fn check_sem_metadata(
        context: &Context,
        op: &impl Operation,
        g: &ExprPostGraph,
        root: crate::graph::NodeId,
        result: crate::ValueId,
    ) {
        assert_eq!(g.get_original_op(root), Some(op.id()));
        assert_eq!(
            g.get_actual_type(root),
            Some(context.get_value(result).ty())
        );
    }

    #[test]
    fn addi_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::addi(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::Add);
        check_sem_metadata(&context, &op, &g, root, op.result());
    }

    #[test]
    fn subi_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::subi(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::Sub);
    }

    #[test]
    fn muli_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::muli(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::Mul);
    }

    #[test]
    fn andi_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::andi(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::And);
    }

    #[test]
    fn ori_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::ori(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::Or);
    }

    #[test]
    fn xori_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::xori(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::Xor);
    }

    #[test]
    fn shli_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::shli(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::ShiftLeft);
    }

    #[test]
    fn shrui_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::shrui(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::ShiftRightLogic);
    }

    #[test]
    fn shrsi_sem_expr() {
        let (context, lhs, rhs) = make_binary_op_context();
        let op = ops::shrsi(&context, lhs, rhs, IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        check_binary_sem(&g, root, ExprKind::ShiftRightArithmetic);
    }

    /// The width-changing ops take their width from the result type via the unary
    /// sem-DSL forms: `extsi -> SExt(x, W)`, `extui -> ZExt(x, W)`,
    /// `trunci -> Extract(x, W-1, 0)`.
    fn const_value(g: &ExprPostGraph, node: crate::graph::NodeId) -> u64 {
        match g.get_leaf_data(node) {
            Some(ExprPayload::Int(v)) => v.to_u64(),
            other => panic!("expected an integer constant, got {other:?}"),
        }
    }

    #[test]
    fn extsi_sem_expr_uses_result_width() {
        let context = Context::with_default_dialects();
        let input = context.create_value(IntegerType::new(&context, 16), None);
        let op = ops::extsi(&context, input.id(), IntegerType::new(&context, 64)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        assert_eq!(g.get_kind(root), &ExprKind::SExt);
        let children: Vec<_> = g.children(root).collect();
        assert_eq!(g.get_kind(children[0]), &ExprKind::Symbol);
        assert_eq!(const_value(&g, children[1]), 64);
    }

    #[test]
    fn extui_sem_expr_uses_result_width() {
        let context = Context::with_default_dialects();
        let input = context.create_value(IntegerType::new(&context, 8), None);
        let op = ops::extui(&context, input.id(), IntegerType::new(&context, 32)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        assert_eq!(g.get_kind(root), &ExprKind::ZExt);
        assert_eq!(const_value(&g, g.children(root).nth(1).unwrap()), 32);
    }

    #[test]
    fn trunci_sem_expr_is_low_bit_extract() {
        let context = Context::with_default_dialects();
        let input = context.create_value(IntegerType::new(&context, 64), None);
        let op = ops::trunci(&context, input.id(), IntegerType::new(&context, 16)).build();
        let mut g = ExprPostGraph::new();
        let root = op.convert(&mut g);
        assert_eq!(g.get_kind(root), &ExprKind::Extract);
        let children: Vec<_> = g.children(root).collect();
        assert_eq!(g.get_kind(children[0]), &ExprKind::Symbol);
        assert_eq!(const_value(&g, children[1]), 15); // high = W - 1
        assert_eq!(const_value(&g, children[2]), 0); // low
    }
}
