use std::sync::Arc;

use crate::{Block, Context, OpId, OpInstance, Operation};

#[derive(Debug)]
pub enum PassError {
    MissingBlock(&'static str),
    RewriteFailed(OpId),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::MissingBlock(name) => {
                write!(f, "operation '{name}' does not have a parent block")
            }
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
            Ok(())
        } else {
            Err(PassError::RewriteFailed(target.op.id))
        }
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
        Context, IRBuilder, Operation, Type,
        builtin::{
            AddIOp, AddIOpBuilder, FuncOp, FuncOpBuilder, ModuleOpBuilder, ReturnOpBuilder,
            SubIOpBuilder,
        },
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
            let result_ty = context.get_value(add.result()).ty().clone();
            let new_op = SubIOpBuilder::new(context)
                .lhs(operands[0])
                .rhs(operands[1])
                .result_type(result_ty)
                .build();
            rewriter.replace_op(op, &new_op)
        }
    }

    #[test]
    fn nested_pass_manager_rewrites_ops() {
        let context = Context::with_default_dialects();
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
        let func_body = func.body();

        let mut func_builder = IRBuilder::new(func_body.clone());
        let add = AddIOpBuilder::new(&context)
            .lhs(func_body.arguments()[0].id())
            .rhs(func_body.arguments()[1].id())
            .result_type(Type::Integer { width: 32 })
            .build();
        let add_result = add.result();
        func_builder.insert(add);
        func_builder.insert(ReturnOpBuilder::new(&context).value(add_result).build());

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
    }
}
