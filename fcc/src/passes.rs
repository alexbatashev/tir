use std::collections::HashMap;
use std::sync::Arc;

use tir::analysis::{AnalysisManager, PreservedAnalyses};
use tir::builtin::{BranchOp, CondBranchOp, FuncOp, ReturnOp, TokenType, ops as b};
use tir::{
    Block, BlockId, Context, IRBuilder, Operand, Operation, OperationRef, Pass, PassError,
    PassTarget, RegionId, Rewriter, ValueId, scf,
};

use crate::cir;

#[derive(Clone, Copy)]
struct LoopTargets {
    break_dest: BlockId,
    continue_dest: BlockId,
}

pub struct LowerCirControlFlowPass;

impl LowerCirControlFlowPass {
    pub fn new() -> Self {
        Self
    }

    fn single_block(context: &Context, region: RegionId) -> Option<Arc<Block>> {
        let blocks = context
            .get_region(region)
            .iter(context.clone())
            .collect::<Vec<_>>();
        (blocks.len() == 1).then(|| blocks[0].clone())
    }

    fn entry_block(context: &Context, region: RegionId) -> Arc<Block> {
        context
            .get_region(region)
            .iter(context.clone())
            .next()
            .unwrap()
    }

    fn condition_operand(context: &Context, region: RegionId) -> Option<ValueId> {
        let block = Self::single_block(context, region)?;
        let op_ids = block.op_ids();
        let last = *op_ids.last()?;
        context
            .get_op(last)
            .as_op::<cir::ConditionOp>()
            .map(|condition| condition.operands()[0])
    }

    fn body_is_structured(context: &Context, region: RegionId) -> bool {
        let Some(block) = Self::single_block(context, region) else {
            return false;
        };
        let op_ids = block.op_ids();
        let Some(last) = op_ids.last() else {
            return false;
        };
        let terminator = context.get_op(*last);
        if terminator.clone().as_op::<cir::YieldOp>().is_none()
            && terminator.clone().as_op::<cir::BreakOp>().is_none()
            && terminator.clone().as_op::<cir::ContinueOp>().is_none()
        {
            return false;
        }
        op_ids[..op_ids.len() - 1].iter().all(|op_id| {
            let op = context.get_op(*op_id);
            if op.dialect == "cir" && op.clone().as_op::<cir::IfOp>().is_some() {
                return op
                    .regions
                    .iter()
                    .all(|region| Self::body_is_structured(context, *region));
            }
            if op.dialect == "cir" && op.clone().as_op::<cir::WhileOp>().is_some() {
                return Self::while_is_structured(context, &op);
            }
            if op.dialect == "cir" && op.clone().as_op::<cir::ForOp>().is_some() {
                return Self::for_is_structured(context, &op);
            }
            op.regions.is_empty()
        })
    }

    fn while_is_structured(context: &Context, op: &tir::OpInstance) -> bool {
        Self::condition_operand(context, op.regions[0]).is_some()
            && Self::body_is_structured(context, op.regions[1])
    }

    fn for_is_structured(context: &Context, op: &tir::OpInstance) -> bool {
        Self::condition_operand(context, op.regions[0]).is_some()
            && Self::body_is_structured(context, op.regions[1])
            && Self::body_is_structured(context, op.regions[2])
            && !Self::loop_scope_is_used(context, op.regions[1])
    }

    fn loop_scope_is_used(context: &Context, body: RegionId) -> bool {
        let scope = Self::entry_block(context, body).arguments()[0].id();
        context.is_value_used(scope)
    }

    fn next_control_op_in_region(
        context: &Context,
        region: RegionId,
    ) -> Option<(Arc<Block>, Arc<tir::OpInstance>)> {
        for block in context.get_region(region).iter(context.clone()) {
            for op_id in block.op_ids() {
                let op = context.get_op(op_id);
                if op.dialect == "cir"
                    && (op.clone().as_op::<cir::WhileOp>().is_some()
                        || op.clone().as_op::<cir::ForOp>().is_some()
                        || op.clone().as_op::<cir::DoOp>().is_some()
                        || op.clone().as_op::<cir::IfOp>().is_some())
                {
                    return Some((block, op));
                }
                for child in &op.regions {
                    if let Some(found) = Self::next_control_op_in_region(context, *child) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    fn region_uses_direct_loop_target(
        context: &Context,
        region: RegionId,
        loop_targets: &HashMap<ValueId, LoopTargets>,
    ) -> bool {
        context
            .get_region(region)
            .iter(context.clone())
            .any(|block| {
                block.op_ids().into_iter().any(|op_id| {
                    let op = context.get_op(op_id);
                    let exits_direct_loop = (op.clone().as_op::<cir::BreakOp>().is_some()
                        || op.clone().as_op::<cir::ContinueOp>().is_some())
                        && loop_targets.contains_key(&op.operands[0]);
                    exits_direct_loop
                        || op.regions.iter().any(|region| {
                            Self::region_uses_direct_loop_target(context, *region, loop_targets)
                        })
                })
            })
    }

    fn split_after(context: &Context, block: &Arc<Block>, op: &Arc<tir::OpInstance>) -> Arc<Block> {
        let continuation = context.create_block(vec![]);
        let op_ids = block.op_ids();
        let position = op_ids.iter().position(|id| *id == op.id).unwrap();
        for op_id in op_ids.into_iter().skip(position + 1) {
            block.remove_op(op_id);
            continuation.insert(continuation.len(), op_id);
        }
        continuation
    }

    fn erase_control_op(
        rewriter: &mut Rewriter,
        block: &Arc<Block>,
        op: Arc<tir::OpInstance>,
    ) -> Result<(), PassError> {
        rewriter.erase_op(&OperationRef::new(op, Some(block.clone()), None))
    }

    fn add_blocks(context: &Context, region: RegionId, blocks: &[Arc<Block>]) {
        let region = context.get_region(region);
        for block in blocks {
            region.add_block(block.id());
        }
    }

    fn rewrite_internal_branches(
        context: &Context,
        rewriter: &mut Rewriter,
        blocks: &[Arc<Block>],
        mapping: &HashMap<BlockId, BlockId>,
    ) -> Result<(), PassError> {
        for block in blocks {
            for op_id in block.op_ids() {
                let op = context.get_op(op_id);
                let replacement: Option<Box<dyn Operation>> =
                    if let Some(branch) = op.clone().as_op::<BranchOp>() {
                        mapping.get(&branch.dest()).map(|destination| {
                            Box::new(b::br(context, branch.dest_args(), *destination).build())
                                as Box<dyn Operation>
                        })
                    } else if let Some(branch) = op.clone().as_op::<CondBranchOp>() {
                        let true_dest = mapping
                            .get(&branch.true_dest())
                            .copied()
                            .unwrap_or(branch.true_dest());
                        let false_dest = mapping
                            .get(&branch.false_dest())
                            .copied()
                            .unwrap_or(branch.false_dest());
                        (true_dest != branch.true_dest() || false_dest != branch.false_dest()).then(
                            || {
                                Box::new(
                                    b::cond_br(
                                        context,
                                        branch.condition(),
                                        branch.true_args(),
                                        branch.false_args(),
                                        true_dest,
                                        false_dest,
                                    )
                                    .build(),
                                ) as Box<dyn Operation>
                            },
                        )
                    } else {
                        None
                    };
                if let Some(replacement) = replacement {
                    rewriter.replace_op(
                        &OperationRef::new(op, Some(block.clone()), None),
                        replacement.as_ref(),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn extract_region(
        context: &Context,
        rewriter: &mut Rewriter,
        region: RegionId,
    ) -> Result<Vec<Arc<Block>>, PassError> {
        let old_blocks = context
            .get_region(region)
            .iter(context.clone())
            .collect::<Vec<_>>();
        let token = TokenType::new(context);
        let mut mapping = HashMap::new();
        let mut blocks = Vec::with_capacity(old_blocks.len());
        for old in &old_blocks {
            let mut arguments = Vec::new();
            for argument in old.arguments() {
                if argument.ty() == token {
                    continue;
                }
                let replacement = context.create_value(argument.ty(), None);
                context.replace_value_uses(argument.id(), replacement.id());
                arguments.push(replacement);
            }
            let new = context.create_block(arguments);
            mapping.insert(old.id(), new.id());
            blocks.push(new);
        }
        for (old, new) in old_blocks.iter().zip(&blocks) {
            for op_id in old.op_ids() {
                old.remove_op(op_id);
                new.insert(new.len(), op_id);
            }
        }
        Self::rewrite_internal_branches(context, rewriter, &blocks, &mapping)?;
        Ok(blocks)
    }

    fn replace_with_branch(
        context: &Context,
        rewriter: &mut Rewriter,
        block: &Arc<Block>,
        destination: BlockId,
    ) -> Result<(), PassError> {
        let terminator = context.get_op(*block.op_ids().last().unwrap());
        let branch = b::br(context, vec![], destination).build();
        rewriter.replace_op(
            &OperationRef::new(terminator, Some(block.clone()), None),
            &branch,
        )
    }

    fn rewrite_condition(
        context: &Context,
        rewriter: &mut Rewriter,
        block: &Arc<Block>,
        true_dest: BlockId,
        false_dest: BlockId,
    ) -> Result<(), PassError> {
        let terminator = context.get_op(*block.op_ids().last().unwrap());
        let condition = terminator
            .clone()
            .as_op::<cir::ConditionOp>()
            .ok_or_else(|| {
                PassError::InvalidRuleSet(
                    "CIR condition region must end with cir.condition".to_string(),
                )
            })?
            .operands()[0];
        let branch = b::cond_br(context, condition, vec![], vec![], true_dest, false_dest).build();
        rewriter.replace_op(
            &OperationRef::new(terminator, Some(block.clone()), None),
            &branch,
        )
    }

    fn rewrite_region_exits(
        context: &Context,
        rewriter: &mut Rewriter,
        blocks: &[Arc<Block>],
        normal_dest: BlockId,
        loop_targets: &HashMap<ValueId, LoopTargets>,
    ) -> Result<(), PassError> {
        for block in blocks {
            let terminator = context.get_op(*block.op_ids().last().unwrap());
            let destination = if terminator.clone().as_op::<cir::YieldOp>().is_some() {
                Some(normal_dest)
            } else if let Some(exit) = terminator.clone().as_op::<cir::BreakOp>() {
                Some(
                    loop_targets
                        .get(&exit.operands()[0])
                        .ok_or_else(|| {
                            PassError::InvalidRuleSet("cir.break has no enclosing loop".to_string())
                        })?
                        .break_dest,
                )
            } else if let Some(exit) = terminator.clone().as_op::<cir::ContinueOp>() {
                Some(
                    loop_targets
                        .get(&exit.operands()[0])
                        .ok_or_else(|| {
                            PassError::InvalidRuleSet(
                                "cir.continue has no enclosing loop".to_string(),
                            )
                        })?
                        .continue_dest,
                )
            } else if terminator.clone().as_op::<BranchOp>().is_some()
                || terminator.clone().as_op::<CondBranchOp>().is_some()
                || terminator.clone().as_op::<ReturnOp>().is_some()
            {
                None
            } else {
                return Err(PassError::InvalidRuleSet(
                    "CIR region has an unsupported exit".to_string(),
                ));
            };
            if let Some(destination) = destination {
                Self::replace_with_branch(context, rewriter, block, destination)?;
            }
        }
        Ok(())
    }

    fn structured_body(
        context: &Context,
        rewriter: &mut Rewriter,
        source: RegionId,
    ) -> Result<RegionId, PassError> {
        let source = Self::single_block(context, source).unwrap();
        let region = context.create_region();
        let arguments = source
            .arguments()
            .iter()
            .map(|argument| {
                let replacement = context.create_value(argument.ty(), None);
                context.replace_value_uses(argument.id(), replacement.id());
                replacement
            })
            .collect();
        let block = context.create_block(arguments);
        region.add_block(block.id());
        for op_id in source.op_ids() {
            source.remove_op(op_id);
            block.insert(block.len(), op_id);
        }
        let terminator = context.get_op(*block.op_ids().last().unwrap());
        let replacement: Box<dyn Operation> =
            if terminator.clone().as_op::<cir::BreakOp>().is_some() {
                Box::new(scf::ops::r#break(context, terminator.operands[0]).build())
            } else if terminator.clone().as_op::<cir::ContinueOp>().is_some() {
                Box::new(scf::ops::r#continue(context, terminator.operands[0]).build())
            } else {
                Box::new(scf::ops::r#yield(context, Operand::none()).build())
            };
        rewriter.replace_op(
            &OperationRef::new(terminator, Some(block), None),
            replacement.as_ref(),
        )?;
        Ok(region.id())
    }

    fn structured_condition(
        context: &Context,
        rewriter: &mut Rewriter,
        source: RegionId,
    ) -> Result<RegionId, PassError> {
        let source = Self::single_block(context, source).unwrap();
        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        for op_id in source.op_ids() {
            source.remove_op(op_id);
            block.insert(block.len(), op_id);
        }
        let terminator = context.get_op(*block.op_ids().last().unwrap());
        let condition = terminator.operands[0];
        let replacement = scf::ops::condition(context, condition, Operand::none()).build();
        rewriter.replace_op(
            &OperationRef::new(terminator, Some(block), None),
            &replacement,
        )?;
        Ok(region.id())
    }

    fn structured_for_body(
        context: &Context,
        body_region: RegionId,
        step_region: RegionId,
    ) -> Result<RegionId, PassError> {
        let body = Self::single_block(context, body_region).unwrap();
        let step = Self::single_block(context, step_region).unwrap();
        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        for source in [body, step] {
            let op_ids = source.op_ids();
            for op_id in op_ids.iter().take(op_ids.len() - 1) {
                source.remove_op(*op_id);
                block.insert(block.len(), *op_id);
            }
        }
        IRBuilder::new(block).insert(scf::ops::r#yield(context, Operand::none()).build());
        Ok(region.id())
    }

    fn lower_structured(
        context: &Context,
        rewriter: &mut Rewriter,
        block: Arc<Block>,
        op: Arc<tir::OpInstance>,
    ) -> Result<(), PassError> {
        let replacement: Box<dyn Operation> = if op.clone().as_op::<cir::WhileOp>().is_some() {
            let condition = Self::structured_condition(context, rewriter, op.regions[0])?;
            let body = Self::structured_body(context, rewriter, op.regions[1])?;
            Box::new(
                scf::ops::r#while(context, Operand::none(), None, Some(condition), Some(body))
                    .build(),
            )
        } else if op.clone().as_op::<cir::ForOp>().is_some() {
            let condition = Self::structured_condition(context, rewriter, op.regions[0])?;
            let body = Self::structured_for_body(context, op.regions[1], op.regions[2])?;
            Box::new(
                scf::ops::r#while(context, Operand::none(), None, Some(condition), Some(body))
                    .build(),
            )
        } else {
            let then_body = Self::structured_body(context, rewriter, op.regions[0])?;
            let else_body = Self::structured_body(context, rewriter, op.regions[1])?;
            Box::new(
                scf::ops::r#if(
                    context,
                    op.operands[0],
                    None,
                    Some(then_body),
                    Some(else_body),
                )
                .build(),
            )
        };
        rewriter.replace_op(
            &OperationRef::new(op, Some(block), None),
            replacement.as_ref(),
        )
    }

    fn lower_direct(
        context: &Context,
        rewriter: &mut Rewriter,
        function_region: RegionId,
        block: Arc<Block>,
        op: Arc<tir::OpInstance>,
        loop_targets: &mut HashMap<ValueId, LoopTargets>,
    ) -> Result<(), PassError> {
        if op.clone().as_op::<cir::WhileOp>().is_some() {
            let scope = Self::entry_block(context, op.regions[1]).arguments()[0].id();
            let condition = Self::extract_region(context, rewriter, op.regions[0])?;
            let body = Self::extract_region(context, rewriter, op.regions[1])?;
            let continuation = Self::split_after(context, &block, &op);
            loop_targets.insert(
                scope,
                LoopTargets {
                    break_dest: continuation.id(),
                    continue_dest: condition[0].id(),
                },
            );
            Self::add_blocks(context, function_region, &condition);
            Self::add_blocks(context, function_region, &body);
            Self::add_blocks(
                context,
                function_region,
                std::slice::from_ref(&continuation),
            );
            Self::erase_control_op(rewriter, &block, op)?;
            IRBuilder::new(block).insert(b::br(context, vec![], condition[0].id()).build());
            Self::rewrite_condition(
                context,
                rewriter,
                &condition[0],
                body[0].id(),
                continuation.id(),
            )?;
            Self::rewrite_region_exits(context, rewriter, &body, condition[0].id(), loop_targets)?;
        } else if op.clone().as_op::<cir::ForOp>().is_some() {
            let scope = Self::entry_block(context, op.regions[1]).arguments()[0].id();
            let condition = Self::extract_region(context, rewriter, op.regions[0])?;
            let body = Self::extract_region(context, rewriter, op.regions[1])?;
            let step = Self::extract_region(context, rewriter, op.regions[2])?;
            let continuation = Self::split_after(context, &block, &op);
            loop_targets.insert(
                scope,
                LoopTargets {
                    break_dest: continuation.id(),
                    continue_dest: step[0].id(),
                },
            );
            Self::add_blocks(context, function_region, &condition);
            Self::add_blocks(context, function_region, &body);
            Self::add_blocks(context, function_region, &step);
            Self::add_blocks(
                context,
                function_region,
                std::slice::from_ref(&continuation),
            );
            Self::erase_control_op(rewriter, &block, op)?;
            IRBuilder::new(block).insert(b::br(context, vec![], condition[0].id()).build());
            Self::rewrite_condition(
                context,
                rewriter,
                &condition[0],
                body[0].id(),
                continuation.id(),
            )?;
            Self::rewrite_region_exits(context, rewriter, &body, step[0].id(), loop_targets)?;
            Self::rewrite_region_exits(context, rewriter, &step, condition[0].id(), loop_targets)?;
        } else if op.clone().as_op::<cir::DoOp>().is_some() {
            let scope = Self::entry_block(context, op.regions[0]).arguments()[0].id();
            let body = Self::extract_region(context, rewriter, op.regions[0])?;
            let condition = Self::extract_region(context, rewriter, op.regions[1])?;
            let continuation = Self::split_after(context, &block, &op);
            loop_targets.insert(
                scope,
                LoopTargets {
                    break_dest: continuation.id(),
                    continue_dest: condition[0].id(),
                },
            );
            Self::add_blocks(context, function_region, &body);
            Self::add_blocks(context, function_region, &condition);
            Self::add_blocks(
                context,
                function_region,
                std::slice::from_ref(&continuation),
            );
            Self::erase_control_op(rewriter, &block, op)?;
            IRBuilder::new(block).insert(b::br(context, vec![], body[0].id()).build());
            Self::rewrite_region_exits(context, rewriter, &body, condition[0].id(), loop_targets)?;
            Self::rewrite_condition(
                context,
                rewriter,
                &condition[0],
                body[0].id(),
                continuation.id(),
            )?;
        } else {
            let then_blocks = Self::extract_region(context, rewriter, op.regions[0])?;
            let else_blocks = Self::extract_region(context, rewriter, op.regions[1])?;
            let continuation = Self::split_after(context, &block, &op);
            Self::add_blocks(context, function_region, &then_blocks);
            Self::add_blocks(context, function_region, &else_blocks);
            Self::add_blocks(
                context,
                function_region,
                std::slice::from_ref(&continuation),
            );
            let condition = op.operands[0];
            Self::erase_control_op(rewriter, &block, op)?;
            IRBuilder::new(block).insert(
                b::cond_br(
                    context,
                    condition,
                    vec![],
                    vec![],
                    then_blocks[0].id(),
                    else_blocks[0].id(),
                )
                .build(),
            );
            Self::rewrite_region_exits(
                context,
                rewriter,
                &then_blocks,
                continuation.id(),
                loop_targets,
            )?;
            Self::rewrite_region_exits(
                context,
                rewriter,
                &else_blocks,
                continuation.id(),
                loop_targets,
            )?;
        }
        Ok(())
    }
}

impl Default for LowerCirControlFlowPass {
    fn default() -> Self {
        Self::new()
    }
}

impl Pass for LowerCirControlFlowPass {
    fn name(&self) -> &'static str {
        "lower-cir-control-flow"
    }

    fn target(&self) -> PassTarget {
        PassTarget::Operation(FuncOp::name())
    }

    fn run(
        &mut self,
        op: &OperationRef,
        context: &Context,
        rewriter: &mut Rewriter,
        _analyses: &AnalysisManager,
    ) -> Result<PreservedAnalyses, PassError> {
        if op.as_op::<FuncOp>().is_none() {
            return Ok(PreservedAnalyses::all());
        }
        let function_region = op.op().regions[0];
        let mut loop_targets = HashMap::new();
        let mut changed = false;
        while let Some((block, control)) = Self::next_control_op_in_region(context, function_region)
        {
            let structured = if control.clone().as_op::<cir::WhileOp>().is_some() {
                Self::while_is_structured(context, &control)
            } else if control.clone().as_op::<cir::ForOp>().is_some() {
                Self::for_is_structured(context, &control)
            } else if control.clone().as_op::<cir::IfOp>().is_some() {
                control
                    .regions
                    .iter()
                    .all(|region| Self::body_is_structured(context, *region))
            } else {
                false
            } && !control.regions.iter().any(|region| {
                Self::region_uses_direct_loop_target(context, *region, &loop_targets)
            });
            if structured {
                Self::lower_structured(context, rewriter, block, control)?;
            } else {
                Self::lower_direct(
                    context,
                    rewriter,
                    function_region,
                    block,
                    control,
                    &mut loop_targets,
                )?;
            }
            changed = true;
        }
        Ok(if changed {
            PreservedAnalyses::none()
        } else {
            PreservedAnalyses::all()
        })
    }
}
