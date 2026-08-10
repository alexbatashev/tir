use std::collections::HashMap;

use tir_symbolic::egraph::{EGraph, ENode, Id, Pattern, Rewrite, Rhs, Substitution, Var};

use crate::utils::APInt;
use crate::{
    ConstantFold, Context, OperationRef, PassError, Rewriter, TypeId, ValueId,
    analysis::GateNode,
    builtin::{IntegerType, ops},
    sem::Value,
};

use super::node::{Node, OpProv};

type Sym = u32;
type Rule = Rewrite<Node, Sym>;

pub type EmitFn = Box<
    dyn Fn(&Context, &[ValueId], TypeId, &OperationRef, &mut Rewriter) -> Result<ValueId, PassError>
        + Send
        + Sync,
>;

pub struct Ruleset {
    pub rewrites: Vec<Rule>,
    pub emits: Vec<Option<EmitFn>>,
}

impl Ruleset {
    fn new() -> Self {
        Self {
            rewrites: Vec::new(),
            emits: Vec::new(),
        }
    }

    fn push(&mut self, rewrite: Rule, emit: Option<EmitFn>) {
        self.rewrites.push(rewrite);
        self.emits.push(emit);
    }
}

pub fn builtin_ruleset(context: &Context, eg: &EGraph<Node>) -> Ruleset {
    let mut ruleset = generated_ruleset(context);
    for template in fold_templates(eg) {
        ruleset.push(const_fold(context.clone(), &template), None);
    }
    ruleset
}

fn operand(substitution: &Substitution<Sym>, index: u32) -> Id {
    substitution
        .get(&Var::Symbol(index))
        .expect("bound operand")
}

fn const_value(eg: &EGraph<Node>, class: Id) -> Option<APInt> {
    eg.nodes(eg.find(class)).iter().find_map(|node| match node {
        Node::Const { value, .. } => Some(value.clone()),
        _ => None,
    })
}

fn class_type(eg: &EGraph<Node>, class: Id) -> Option<TypeId> {
    eg.nodes(eg.find(class)).iter().find_map(Node::op_type)
}

fn class_int_width(context: &Context, eg: &EGraph<Node>, class: Id) -> Option<u32> {
    class_value_type(context, eg, class).and_then(|ty| {
        (context.get_type_data(ty).as_ref() as &dyn std::any::Any)
            .downcast_ref::<IntegerType>()
            .map(IntegerType::width)
    })
}

fn class_value_type(context: &Context, eg: &EGraph<Node>, class: Id) -> Option<TypeId> {
    eg.nodes(eg.find(class)).iter().find_map(|node| match node {
        Node::Op { ty, .. } => Some(*ty),
        Node::Gate(gate, _) => match gate {
            GateNode::Input(value)
            | GateNode::Gamma { value, .. }
            | GateNode::Mu { value }
            | GateNode::Eta { value }
            | GateNode::Phi { value } => Some(context.get_value(*value).ty()),
            GateNode::Op(_) => unreachable!("operations use Node::Op"),
        },
        Node::Const { .. } => None,
    })
}

fn bind_width(widths: &mut HashMap<&'static str, u32>, name: &'static str, width: u32) -> bool {
    widths.get(name).is_none_or(|bound| *bound == width) && {
        widths.insert(name, width);
        true
    }
}

fn class_is_literal(eg: &EGraph<Node>, class: Id, literal: i64) -> bool {
    const_value(eg, class).is_some_and(|value| {
        let mask = if value.width() == 64 {
            u64::MAX
        } else {
            (1u64 << value.width()) - 1
        };
        value.to_u64() == literal as u64 & mask
    })
}

fn emit_shl() -> EmitFn {
    Box::new(|context, operands, ty, target, rewriter| {
        let op = ops::shli(context, operands[0], operands[1], ty).build();
        rewriter.insert_op_before(target, &op)?;
        Ok(op.result())
    })
}

/// One LHS template per distinct seeded-op signature in `eg`, so const folding
/// searches the classes holding such an op instead of every class. Only a seeded
/// op ever folds and rewrites introduce none, so the seeded graph fixes the set.
fn fold_templates(eg: &EGraph<Node>) -> Vec<Node> {
    let mut templates: Vec<Node> = Vec::new();
    for class in eg.classes() {
        for node in class.nodes() {
            if !matches!(
                node,
                Node::Op {
                    prov: OpProv::Seeded(_),
                    ..
                }
            ) {
                continue;
            }
            let template = node
                .op_template(node.children().to_vec())
                .expect("an op node has an op template");
            if !templates.iter().any(|seen| {
                seen.matches(&template) && seen.children().len() == template.children().len()
            }) {
                templates.push(template);
            }
        }
    }
    templates
}

fn const_fold(context: Context, template: &Node) -> Rule {
    let mut lhs = Pattern::new();
    let args: Vec<Id> = (0..template.children().len())
        .map(|index| lhs.var(Var::Symbol(index as Sym)))
        .collect();
    let mut root = template.clone();
    root.children_mut().copy_from_slice(&args);
    lhs.add(root);
    Rewrite::new(
        "const-fold",
        lhs,
        Rhs::Apply(Box::new(move |eg, _substitution, root| {
            if let Some(value) = fold_class(&context, eg, eg.find(root)) {
                let folded = eg.add(konst(value));
                eg.union(root, folded);
            }
        })),
    )
}

fn fold_class(context: &Context, eg: &EGraph<Node>, class: Id) -> Option<APInt> {
    eg.nodes(class).iter().find_map(|node| {
        let Node::Op {
            prov: OpProv::Seeded(op),
            args,
            ..
        } = node
        else {
            return None;
        };
        if !context.has_operation(*op) {
            return None;
        }
        let operands: Vec<Value> = args
            .iter()
            .map(|&class| const_value(eg, class).map(Value::Int))
            .collect::<Option<_>>()?;
        match context
            .get_op(*op)
            .as_interface::<dyn ConstantFold>()?
            .fold(&operands)
        {
            Some(Value::Int(value)) => Some(value),
            _ => None,
        }
    })
}

fn konst(value: APInt) -> Node {
    Node::Const {
        value,
        origin: None,
    }
}

include!(concat!(env!("OUT_DIR"), "/instcombine_rules.rs"));
