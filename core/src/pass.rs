use std::sync::Arc;

use crate::{Block, Context, OpId, OpInstance, Operation};

#[derive(Debug)]
pub enum PassError {
    MissingBlock(&'static str),
    InvalidRuleSet(String),
    RewriteFailed(OpId),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::MissingBlock(name) => {
                write!(f, "operation '{name}' does not have a parent block")
            }
            PassError::InvalidRuleSet(message) => write!(f, "invalid rule set: {message}"),
            PassError::RewriteFailed(op) => write!(f, "failed to rewrite op {op:?}"),
        }
    }
}

impl std::error::Error for PassError {}

#[derive(Debug, Clone, Copy)]
pub enum PassTarget {
    Any,
    Operation(&'static str),
}

impl PassTarget {
    fn matches(&self, op: &OpInstance) -> bool {
        match self {
            PassTarget::Any => true,
            PassTarget::Operation(name) => op.name == *name,
        }
    }
}

#[derive(Clone)]
pub struct OperationRef {
    op: Arc<OpInstance>,
    block: Option<Arc<Block>>,
    position: Option<usize>,
}

impl OperationRef {
    pub fn new(op: Arc<OpInstance>, block: Option<Arc<Block>>, position: Option<usize>) -> Self {
        Self {
            op,
            block,
            position,
        }
    }

    pub fn op(&self) -> &Arc<OpInstance> {
        &self.op
    }

    pub fn block(&self) -> Option<&Arc<Block>> {
        self.block.as_ref()
    }

    pub fn position(&self) -> Option<usize> {
        self.position
    }

    pub fn name(&self) -> &'static str {
        self.op.name
    }

    pub fn as_op<T: Operation>(&self) -> Option<T> {
        self.op.clone().as_op::<T>()
    }

    pub fn as_interface<I: ?Sized + 'static>(&self) -> Option<Box<I>> {
        self.op.clone().as_interface::<I>()
    }
}

pub trait Pass: Send {
    fn name(&self) -> &'static str;
    fn target(&self) -> PassTarget {
        PassTarget::Any
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError>;
}

pub struct Rewriter {
    context: Context,
}

impl Rewriter {
    pub fn new(context: Context) -> Self {
        Self { context }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn replace_op(
        &mut self,
        target: &OperationRef,
        new_op: &dyn Operation,
    ) -> Result<(), PassError> {
        let block = target
            .block
            .as_ref()
            .ok_or(PassError::MissingBlock(target.name()))?;
        if block.replace_op(target.op.id, new_op.id()) {
            // The replaced-out op no longer references its operands; the new op
            // registered its own uses when it was added to the context. Drop the old
            // op from the arena so it doesn't linger as a phantom.
            self.context.detach_op_uses(&target.op);
            self.context.remove_operation(target.op.id);
            Ok(())
        } else {
            Err(PassError::RewriteFailed(target.op.id))
        }
    }

    pub fn erase_op(&mut self, target: &OperationRef) -> Result<(), PassError> {
        let block = target
            .block
            .as_ref()
            .ok_or(PassError::MissingBlock(target.name()))?;
        if block.remove_op(target.op.id) {
            self.context.detach_op_uses(&target.op);
            self.context.remove_operation(target.op.id);
            Ok(())
        } else {
            Err(PassError::RewriteFailed(target.op.id))
        }
    }

    /// Insert `new_op` immediately before `target` in its block. Used when one
    /// source op lowers to several machine instructions (e.g. a sub-word sign
    /// extension becoming `slli` then `srai`): the feeding instructions are inserted
    /// ahead of the op that consumes them. Repeated calls before the same target
    /// preserve insertion order.
    pub fn insert_op_before(
        &mut self,
        target: &OperationRef,
        new_op: &dyn Operation,
    ) -> Result<(), PassError> {
        let block = target
            .block
            .as_ref()
            .ok_or(PassError::MissingBlock(target.name()))?;
        let position = block
            .op_ids()
            .iter()
            .position(|id| *id == target.op.id)
            .ok_or(PassError::RewriteFailed(target.op.id))?;
        block.insert(position, new_op.id());
        Ok(())
    }
}

enum PassNode {
    Pass(Box<dyn Pass>),
    Nested {
        op_name: &'static str,
        manager: PassManager,
    },
}

pub struct PassManager {
    passes: Vec<PassNode>,
}

impl PassManager {
    pub fn new() -> Self {
        Self { passes: vec![] }
    }

    pub fn add_pass<P: Pass + 'static>(&mut self, pass: P) -> &mut Self {
        self.passes.push(PassNode::Pass(Box::new(pass)));
        self
    }

    pub fn nest(&mut self, op_name: &'static str) -> &mut PassManager {
        self.passes.push(PassNode::Nested {
            op_name,
            manager: PassManager::new(),
        });
        match self.passes.last_mut() {
            Some(PassNode::Nested { manager, .. }) => manager,
            _ => unreachable!("nested pass manager entry just added"),
        }
    }

    pub fn run(&mut self, context: &Context, op: Arc<OpInstance>) -> Result<(), PassError> {
        let root = OperationRef {
            op,
            block: None,
            position: None,
        };
        self.run_on_op_ref(context, root)
    }

    pub fn run_on_op_ref(
        &mut self,
        context: &Context,
        root: OperationRef,
    ) -> Result<(), PassError> {
        let mut rewriter = Rewriter::new(context.clone());
        for entry in &mut self.passes {
            PassManager::run_entry(entry, context, &root, &mut rewriter)?;
        }
        Ok(())
    }

    fn run_entry(
        entry: &mut PassNode,
        context: &Context,
        root: &OperationRef,
        rewriter: &mut Rewriter,
    ) -> Result<(), PassError> {
        match entry {
            PassNode::Pass(pass) => PassManager::walk_ops(context, root, &mut |op_ref| {
                if pass.target().matches(op_ref.op()) {
                    pass.run(&op_ref, context, rewriter)?;
                }
                Ok(())
            }),
            PassNode::Nested { op_name, manager } => {
                PassManager::walk_ops(context, root, &mut |op_ref| {
                    if op_ref.name() == *op_name {
                        manager.run_on_op_ref(context, op_ref.clone())?;
                    }
                    Ok(())
                })
            }
        }
    }

    fn walk_ops<F>(context: &Context, root: &OperationRef, f: &mut F) -> Result<(), PassError>
    where
        F: FnMut(OperationRef) -> Result<(), PassError>,
    {
        f(root.clone())?;
        for region_id in &root.op.regions {
            let region = context.get_region(*region_id);
            for block in region.iter(context.clone()) {
                let op_ids = block.op_ids();
                for (index, op_id) in op_ids.into_iter().enumerate() {
                    // A pass run earlier in this walk may have erased or replaced a
                    // later op in the same block (isel rewrites the whole block at
                    // once); the snapshot still holds the old id. Skip ops that are no
                    // longer live — a replacement carries a new id and isn't revisited.
                    if !context.has_operation(op_id) {
                        continue;
                    }
                    let op = context.get_op(op_id);
                    let child = OperationRef {
                        op,
                        block: Some(block.clone()),
                        position: Some(index),
                    };
                    PassManager::walk_ops(context, &child, f)?;
                }
            }
        }
        Ok(())
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, IRBuilder, Operation,
        builtin::{AddIOp, FuncOp, IntegerType, ops},
    };

    use super::{Pass, PassError, PassManager, PassTarget};

    struct AddToSubPass;

    impl Pass for AddToSubPass {
        fn name(&self) -> &'static str {
            "add-to-sub"
        }

        fn target(&self) -> PassTarget {
            PassTarget::Operation(AddIOp::name())
        }

        fn run(
            &mut self,
            op: &super::OperationRef,
            context: &Context,
            rewriter: &mut super::Rewriter,
        ) -> Result<(), PassError> {
            let add = op.as_op::<AddIOp>().expect("target guarantees AddIOp");
            let operands = add.operands();
            let result_ty = context.get_value(add.result()).ty();
            let new_op = ops::subi(context, operands[0], operands[1], result_ty).build();
            rewriter.replace_op(op, &new_op)
        }
    }

    #[test]
    fn nested_pass_manager_rewrites_ops() {
        let context = Context::with_default_dialects();
        let module = ops::module(&context, None).build();

        let param0 = context.create_value(IntegerType::new(&context, 32), None);
        let param1 = context.create_value(IntegerType::new(&context, 32), None);

        let region = context.create_region();
        let block = context.create_block(vec![param0, param1]);
        region.add_block(block.id());

        let func = ops::func(
            &context,
            "demo",
            IntegerType::new(&context, 32),
            Some(region.id()),
        )
        .build();
        let func_body = func.body();

        let mut func_builder = IRBuilder::new(func_body.clone());
        let add = ops::addi(
            &context,
            func_body.arguments()[0].id(),
            func_body.arguments()[1].id(),
            IntegerType::new(&context, 32),
        )
        .build();
        let add_result = add.result();
        let add_id = add.id();
        func_builder.insert(add);
        func_builder.insert(ops::r#return(&context, add_result).build());

        let mut module_builder = IRBuilder::new(module.body());
        module_builder.insert(func);

        let mut pm = PassManager::new();
        pm.nest(FuncOp::name()).add_pass(AddToSubPass);

        pm.run(&context, context.get_op(module.id()))
            .expect("pass pipeline should succeed");

        let op_names: Vec<_> = func_body
            .op_ids()
            .into_iter()
            .map(|op_id| context.get_op(op_id).name)
            .collect();

        assert_eq!(op_names, vec!["subi", "return"]);

        // The use-list followed the rewrite: param0 is now used by the subi (the
        // replacement), not the erased addi.
        let subi_id = func_body.op_ids()[0];
        let uses = context.value_uses(func_body.arguments()[0].id());
        assert_eq!(uses.len(), 1, "param0 should have exactly one use");
        assert_eq!(uses[0].op(), subi_id);

        // The replaced-out addi is gone from the arena, not just the block.
        assert!(
            !context.has_operation(add_id),
            "replaced op should leave the arena"
        );
    }

    #[test]
    fn erasing_an_op_detaches_its_operand_uses() {
        let context = Context::with_default_dialects();
        let i32 = IntegerType::new(&context, 32);

        let region = context.create_region();
        let arg = context.create_value(i32, None);
        let block = context.create_block(vec![arg.clone()]);
        region.add_block(block.id());
        let func = ops::func(&context, "demo", i32, Some(region.id())).build();
        let body = func.body();

        let mut b = IRBuilder::new(body.clone());
        let neg = ops::subi(
            &context,
            body.arguments()[0].id(),
            body.arguments()[0].id(),
            i32,
        )
        .build();
        let neg_id = neg.id();
        let neg_ref = super::OperationRef::new(context.get_op(neg_id), Some(body.clone()), None);
        b.insert(neg);
        assert!(context.is_value_used(body.arguments()[0].id()));

        let mut rewriter = super::Rewriter::new(context.clone());
        rewriter.erase_op(&neg_ref).expect("erase should succeed");

        assert!(
            !context.is_value_used(body.arguments()[0].id()),
            "erasing the only consumer must clear the value's uses"
        );
        // The erased op is gone from the arena, not just the block.
        assert!(
            !context.has_operation(neg_id),
            "erased op should leave the arena"
        );
    }
}
