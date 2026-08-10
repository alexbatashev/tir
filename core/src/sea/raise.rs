//! Raising `builtin.func`/`scf` onto the sea, one function at a time.
//!
//! # The tail-controlled normalization
//!
//! θ is the only loop: it is tail controlled, so its body always runs once. A
//! pre-tested `scf.while` therefore raises to γ-wrapping-θ. Writing the loop as
//! `C` (the condition region, mapping the carried tuple `x` to a predicate and a
//! forwarded tuple) and `B` (the body, mapping the forwarded tuple to the next
//! carried tuple):
//!
//! ```text
//!   while: x0 → (c,v) = C(x0); if ¬c exit with v; else x1 = B(v); repeat
//!        ≡ (c0,v0) = C(x0);
//!          γ(c0) { 0: v0,  1: θ(v0) { v → (C(B(v)) as (pred, v')) } }
//! ```
//!
//! `C` is evaluated once before the loop and once at the end of each iteration —
//! the standard cost of loop rotation, and the only duplication the mapping
//! introduces. A `do`-shaped `scf.while`, whose condition region is a constant
//! `true` (what fcc's `do` lowering emits), needs no rotation: `c0` is known, so
//! the wrapping γ is skipped and the loop raises to a plain θ.
//!
//! # Valued exits
//!
//! `scf.break`/`scf.continue` carry one value per loop port. They map through γ
//! results feeding θ results: the γ holding the exits gains a leading `broke`
//! result, and each arm reports `(broke, carried')` — `(0, latched)` when it
//! falls through, `(1, values)` for a break, `(0, values)` for a continue, which
//! under this shape is the same edge as falling through. A second γ on `broke`
//! then decides the θ continuation predicate: not broken re-evaluates `C`,
//! broken yields `0`. Nothing is speculated — `C` only runs on the path that
//! would have run it.
//!
//! Exits are supported where they are the body's terminator, or the terminators
//! of the arms of an `scf.if` immediately preceding it. Anything deeper needs
//! Bahmann's general predicate lifting over the code that follows the exit; a
//! function containing one bails with that reason rather than raising something
//! unsound.
//!
//! # Memory
//!
//! One conservative state edge threads every operation that is not provably
//! pure, in program order, and is carried through every γ and θ signature.
//! Per-alloca chains are a later increment.

use std::sync::Arc;

use crate::attributes::{AttributeValue, NamedAttribute};
use crate::builtin::{FuncOp, IntegerType, TokenType, UnitType};
use crate::dialects::scf;
use crate::{Block, Context, OpId, OpInstance, RegionId as IrRegion, TypeId, Value, ValueId};

use super::Error;
use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::kinds;

/// A function raised onto the sea.
pub struct Raised {
    pub graph: Graph,
    /// The λ node the function became. Its region's leading arguments are the
    /// function's parameters and its trailing argument is the memory state.
    pub lambda: NodeId,
}

/// Raise `func` onto a fresh graph. Returns the shape that could not be mapped,
/// naming why, rather than raising something unsound.
pub fn raise_function(context: &Context, func: &FuncOp) -> Result<Raised, Error> {
    let i1 = IntegerType::new(context, 1);
    let mut raiser = Raiser {
        context,
        graph: Graph::new(i1, &[]),
        i1,
    };

    let body = func.body();
    let parameters: Vec<&Value> = body.arguments().iter().collect();
    let mut arguments: Vec<PortType> = parameters
        .iter()
        .map(|value| PortType::Value(value.ty()))
        .collect();
    arguments.push(PortType::State);

    let region = raiser.graph.open_region(&arguments);
    let mut scope = Scope {
        bindings: parameters
            .iter()
            .enumerate()
            .map(|(index, value)| (value.id(), Origin::argument(region, index as u32)))
            .collect(),
        state: Origin::argument(region, parameters.len() as u32),
    };

    let tail = raiser.raise_block(&mut scope, &body)?;
    let Tail::Return(values) = tail else {
        return Err(Error::new("a function body must end with builtin.return"));
    };
    let mut results = scope.resolve_all(&values)?;
    results.push(scope.state);
    raiser.graph.close_region(region, &results)?;

    let function = PortType::Value(UnitType::new(context));
    let lambda = raiser
        .graph
        .add_node(kinds::LAMBDA, &[], &[function], &[region], Vec::new())?;
    raiser.graph.finish(&[Origin::output(lambda, 0)])?;

    Ok(Raised {
        graph: raiser.graph,
        lambda,
    })
}

/// The values visible in one sea region, and the memory state reaching the point
/// being raised.
struct Scope {
    bindings: Vec<(ValueId, Origin)>,
    state: Origin,
}

impl Scope {
    fn lookup(&self, value: ValueId) -> Option<Origin> {
        self.bindings
            .iter()
            .rev()
            .find(|(bound, _)| *bound == value)
            .map(|(_, origin)| *origin)
    }

    fn resolve(&self, value: ValueId) -> Result<Origin, Error> {
        self.lookup(value)
            .ok_or_else(|| Error::new(format!("value %{} is not defined here", value.number())))
    }

    fn resolve_all(&self, values: &[ValueId]) -> Result<Vec<Origin>, Error> {
        values.iter().map(|&value| self.resolve(value)).collect()
    }

    fn bind(&mut self, value: ValueId, origin: Origin) {
        self.bindings.push((value, origin));
    }
}

/// How a raised block left: through a return, a fall-through yield, a loop exit,
/// or a `scf.condition`.
enum Tail {
    Return(Vec<ValueId>),
    Yield(Vec<ValueId>),
    Break(Vec<ValueId>),
    Continue(Vec<ValueId>),
    Condition(ValueId, Vec<ValueId>),
}

struct Raiser<'a> {
    context: &'a Context,
    graph: Graph,
    i1: TypeId,
}

impl Raiser<'_> {
    fn raise_block(&mut self, scope: &mut Scope, block: &Arc<Block>) -> Result<Tail, Error> {
        let ops = block.op_ids();
        let (&terminator, body) = ops
            .split_last()
            .ok_or_else(|| Error::new("a structured block must have a terminator"))?;
        for &id in body {
            let op = self.context.get_op(id);
            self.raise_op(scope, &op)?;
        }
        self.tail_of(&self.context.get_op(terminator))
    }

    fn tail_of(&self, op: &Arc<OpInstance>) -> Result<Tail, Error> {
        if op.is::<crate::builtin::ReturnOp>() {
            Ok(Tail::Return(op.operands.clone()))
        } else if op.is::<scf::YieldOp>() {
            Ok(Tail::Yield(op.operands.clone()))
        } else if op.is::<scf::BreakOp>() {
            Ok(Tail::Break(op.operands[1..].to_vec()))
        } else if op.is::<scf::ContinueOp>() {
            Ok(Tail::Continue(op.operands[1..].to_vec()))
        } else if op.is::<scf::ConditionOp>() {
            Ok(Tail::Condition(op.operands[0], op.operands[1..].to_vec()))
        } else {
            Err(Error::new(format!(
                "terminator {}.{} has no sea mapping",
                op.dialect(),
                op.name()
            )))
        }
    }

    fn raise_op(&mut self, scope: &mut Scope, op: &Arc<OpInstance>) -> Result<(), Error> {
        if op.is::<scf::IfOp>() {
            self.raise_if(scope, op)
        } else if op.is::<scf::WhileOp>() {
            self.raise_while(scope, op)
        } else if op.is::<scf::ForOp>() {
            Err(Error::new(
                "scf.for does not expose its induction variable as a body argument, so its \
                 trip test cannot be reconstructed for the tail-controlled normalization",
            ))
        } else {
            self.raise_simple(scope, op)
        }
    }

    fn raise_simple(&mut self, scope: &mut Scope, op: &Arc<OpInstance>) -> Result<(), Error> {
        if !op.regions.is_empty() {
            return Err(Error::new(format!(
                "{}.{} carries regions but is not a canonical structural kind",
                op.dialect(),
                op.name()
            )));
        }
        let stateful = is_stateful(op);
        let mut inputs = scope.resolve_all(&op.operands)?;
        let mut outputs: Vec<PortType> = op
            .results
            .iter()
            .map(|&result| PortType::Value(self.context.get_value(result).ty()))
            .collect();
        if stateful {
            inputs.push(scope.state);
            outputs.push(PortType::State);
        }

        let op_type = self
            .graph
            .op_type(op.dialect().as_str(), op.name().as_str());
        let node = self
            .graph
            .add_node(op_type, &inputs, &outputs, &[], op.attributes.clone())?;
        for (port, &result) in op.results.iter().enumerate() {
            scope.bind(result, Origin::output(node, port as u32));
        }
        if stateful {
            scope.state = Origin::output(node, op.results.len() as u32);
        }
        Ok(())
    }

    /// `scf.if` → γ. Region 0 is the `else` arm and region 1 the `then` arm: the
    /// predicate selects the region with its own index, and the condition is `i1`.
    fn raise_if(&mut self, scope: &mut Scope, op: &Arc<OpInstance>) -> Result<(), Error> {
        // Region 0 is the `else` arm and region 1 the `then` arm, so the γ
        // predicate selects the region with its own index.
        let arms: [IrRegion; 2] = [op.regions[1], op.regions[0]];
        let free = self.free_values(&arms, scope);
        let carried = scope.resolve_all(&free)?;

        let mut argument_types: Vec<PortType> =
            carried.iter().map(|&o| self.graph.origin_type(o)).collect();
        argument_types.push(PortType::State);

        let mut outputs: Vec<PortType> = op
            .results
            .iter()
            .map(|&result| PortType::Value(self.context.get_value(result).ty()))
            .collect();
        outputs.push(PortType::State);

        let mut regions = Vec::new();
        for arm in arms {
            let region = self.graph.open_region(&argument_types);
            let mut inner = self.arm_scope(region, &free);
            let block = self.entry_block(arm)?;
            let tail = self.raise_block(&mut inner, &block)?;
            let Tail::Yield(values) = tail else {
                return Err(Error::new(
                    "an scf.if arm leaves through a loop exit; exits are only mapped when the \
                     conditional immediately precedes the loop body's terminator",
                ));
            };
            let mut results = inner.resolve_all(&values)?;
            results.push(inner.state);
            self.graph.close_region(region, &results)?;
            regions.push(region);
        }

        let mut inputs = vec![scope.resolve(op.operands[0])?];
        inputs.extend_from_slice(&carried);
        inputs.push(scope.state);
        let gamma = self
            .graph
            .add_node(kinds::GAMMA, &inputs, &outputs, &regions, Vec::new())?;
        for (port, &result) in op.results.iter().enumerate() {
            scope.bind(result, Origin::output(gamma, port as u32));
        }
        scope.state = Origin::output(gamma, op.results.len() as u32);
        Ok(())
    }

    fn raise_while(&mut self, scope: &mut Scope, op: &Arc<OpInstance>) -> Result<(), Error> {
        let condition_region = op.regions[0];
        let body_region = op.regions[1];
        let condition_block = self.entry_block(condition_region)?;
        let body_block = self.entry_block(body_region)?;
        let ports = op.results.len();

        let carried_arguments: Vec<ValueId> =
            condition_block.arguments().iter().map(Value::id).collect();
        let token = TokenType::new(self.context);
        let body_arguments: Vec<ValueId> = body_block
            .arguments()
            .iter()
            .filter(|argument| argument.ty() != token)
            .map(Value::id)
            .collect();
        let loop_scope = body_block
            .arguments()
            .iter()
            .find(|argument| argument.ty() == token)
            .map(Value::id);

        let free = self.free_values(&[condition_region, body_region], scope);
        let invariants = scope.resolve_all(&free)?;
        let carried_types: Vec<PortType> = op
            .results
            .iter()
            .map(|&result| PortType::Value(self.context.get_value(result).ty()))
            .collect();
        let mut port_types = carried_types.clone();
        port_types.extend(invariants.iter().map(|&o| self.graph.origin_type(o)));
        port_types.push(PortType::State);

        let rotate = !self.is_trivially_true(&condition_block, &carried_arguments);
        let mut entry = scope.resolve_all(&op.operands)?;
        let mut predicate = None;
        if rotate {
            let (test, forwarded) = self.inline_condition(scope, &condition_block, &entry)?;
            predicate = Some(test);
            entry = forwarded;
        }
        entry.extend_from_slice(&invariants);
        entry.push(scope.state);

        let plan = LoopPlan {
            condition_region,
            ports,
            carried_arguments: body_arguments,
            loop_scope,
            free: free.clone(),
            port_types: port_types.clone(),
            body_block,
        };

        let mut outputs = carried_types;
        outputs.push(PortType::State);

        // A plain θ keeps the whole port tuple on its outputs; the rotating γ
        // publishes only the carried ports and the state.
        let state_port = match predicate {
            None => port_types.len() - 1,
            Some(_) => ports,
        };
        let node = match predicate {
            None => self.build_theta(&entry, &plan)?,
            Some(test) => {
                let taken = self.graph.open_region(&port_types);
                let theta_inputs: Vec<Origin> = (0..port_types.len())
                    .map(|index| Origin::argument(taken, index as u32))
                    .collect();
                let theta = self.build_theta(&theta_inputs, &plan)?;
                let mut results: Vec<Origin> = (0..ports)
                    .map(|port| Origin::output(theta, port as u32))
                    .collect();
                results.push(Origin::output(theta, (port_types.len() - 1) as u32));
                self.graph.close_region(taken, &results)?;

                let skipped = self.graph.open_region(&port_types);
                let mut results: Vec<Origin> = (0..ports)
                    .map(|port| Origin::argument(skipped, port as u32))
                    .collect();
                results.push(Origin::argument(skipped, (port_types.len() - 1) as u32));
                self.graph.close_region(skipped, &results)?;

                let mut inputs = vec![test];
                inputs.extend_from_slice(&entry);
                self.graph.add_node(
                    kinds::GAMMA,
                    &inputs,
                    &outputs,
                    &[skipped, taken],
                    Vec::new(),
                )?
            }
        };

        for (port, &result) in op.results.iter().enumerate() {
            scope.bind(result, Origin::output(node, port as u32));
        }
        scope.state = Origin::output(node, state_port as u32);
        Ok(())
    }

    /// Build the θ for a loop whose entry tuple is `entry`, inside `region`.
    fn build_theta(&mut self, entry: &[Origin], plan: &LoopPlan) -> Result<NodeId, Error> {
        let body = self.graph.open_region(&plan.port_types);
        let mut inner = Scope {
            bindings: Vec::new(),
            state: Origin::argument(body, (plan.port_types.len() - 1) as u32),
        };
        for (port, &value) in plan.carried_arguments.iter().enumerate() {
            inner.bind(value, Origin::argument(body, port as u32));
        }
        for (index, &value) in plan.free.iter().enumerate() {
            inner.bind(value, Origin::argument(body, (plan.ports + index) as u32));
        }

        let results = self.raise_loop_body(&mut inner, plan, body)?;
        self.graph.close_region(body, &results)?;

        let outputs = plan.port_types.clone();
        self.graph
            .add_node(kinds::THETA, entry, &outputs, &[body], Vec::new())
    }

    /// Raise a loop body into `body`, returning the θ region's result tuple: the
    /// continuation predicate followed by the next iteration's ports.
    fn raise_loop_body(
        &mut self,
        inner: &mut Scope,
        plan: &LoopPlan,
        body: RegionId,
    ) -> Result<Vec<Origin>, Error> {
        let ops = plan.body_block.op_ids();
        let (&terminator, rest) = ops
            .split_last()
            .ok_or_else(|| Error::new("a loop body must have a terminator"))?;
        let terminator = self.context.get_op(terminator);

        let exit_conditional = self.find_exit_conditional(plan, rest, &terminator)?;
        for &id in rest {
            let op = self.context.get_op(id);
            if Some(id) == exit_conditional {
                break;
            }
            self.raise_op(inner, &op)?;
        }

        let (broke, latched) = match exit_conditional {
            None => match self.tail_of(&terminator)? {
                Tail::Yield(values) | Tail::Continue(values) => (None, inner.resolve_all(&values)?),
                Tail::Break(values) => {
                    let left = self.constant(1)?;
                    (Some(left), inner.resolve_all(&values)?)
                }
                _ => {
                    return Err(Error::new(
                        "a loop body must end with scf.yield, scf.break or scf.continue",
                    ));
                }
            },
            Some(id) => {
                let Tail::Yield(values) = self.tail_of(&terminator)? else {
                    return Err(Error::new(
                        "a loop body holding a conditional exit must fall through to scf.yield",
                    ));
                };
                let gamma = self.raise_exit_conditional(inner, plan, id, &values)?;
                let carried = (0..plan.ports)
                    .map(|port| Origin::output(gamma, (port + 1) as u32))
                    .collect();
                inner.state = Origin::output(gamma, (plan.ports + 1) as u32);
                (Some(Origin::output(gamma, 0)), carried)
            }
        };

        let mut results = match broke {
            None => {
                let block = self.entry_block(plan.condition_region)?;
                let (predicate, forwarded) = self.inline_condition(inner, &block, &latched)?;
                let mut results = vec![predicate];
                results.extend(forwarded);
                results
            }
            Some(broke) => self.raise_continuation(inner, plan, broke, &latched)?,
        };
        for index in 0..plan.free.len() {
            results.push(Origin::argument(body, (plan.ports + index) as u32));
        }
        results.push(inner.state);
        Ok(results)
    }

    /// The γ carrying the loop's exits: one leading `broke` result, then the
    /// carried tuple each arm leaves with.
    fn raise_exit_conditional(
        &mut self,
        inner: &mut Scope,
        plan: &LoopPlan,
        conditional: OpId,
        latched: &[ValueId],
    ) -> Result<NodeId, Error> {
        let op = self.context.get_op(conditional);
        // Region 0 is the `else` arm and region 1 the `then` arm, so the γ
        // predicate selects the region with its own index.
        let arms: [IrRegion; 2] = [op.regions[1], op.regions[0]];
        // Each arm reports the tuple the loop carries on its own edge, so the
        // values feeding the body's yield are captured alongside the arms' own
        // free values. What the conditional produces is not captured: on the
        // falling-through edge those are that arm's yielded values, and on an
        // exiting edge they are never read.
        let mut free = self.free_values(&arms, inner);
        for &value in latched {
            if inner.lookup(value).is_some() && !free.contains(&value) {
                free.push(value);
            }
        }
        let captured = inner.resolve_all(&free)?;

        let mut argument_types: Vec<PortType> = captured
            .iter()
            .map(|&o| self.graph.origin_type(o))
            .collect();
        argument_types.push(PortType::State);

        let mut regions = Vec::new();
        for arm in arms {
            let region = self.graph.open_region(&argument_types);
            let mut arm_scope = self.arm_scope(region, &free);
            let block = self.entry_block(arm)?;
            let tail = self.raise_block(&mut arm_scope, &block)?;
            let (broke, carried) = match tail {
                Tail::Yield(values) if values.len() == op.results.len() => {
                    for (&result, &value) in op.results.iter().zip(&values) {
                        let origin = arm_scope.resolve(value)?;
                        arm_scope.bind(result, origin);
                    }
                    let zero = self.constant(0)?;
                    (zero, arm_scope.resolve_all(latched)?)
                }
                Tail::Break(values) => {
                    let one = self.constant(1)?;
                    (one, arm_scope.resolve_all(&values)?)
                }
                Tail::Continue(values) => {
                    let zero = self.constant(0)?;
                    (zero, arm_scope.resolve_all(&values)?)
                }
                _ => {
                    return Err(Error::new(
                        "an arm of the loop's exiting conditional does not end in a yield, \
                         break or continue",
                    ));
                }
            };
            let mut results = vec![broke];
            results.extend(carried);
            results.push(arm_scope.state);
            self.graph.close_region(region, &results)?;
            regions.push(region);
        }

        let mut inputs = vec![inner.resolve(op.operands[0])?];
        inputs.extend_from_slice(&captured);
        inputs.push(inner.state);
        let mut outputs = vec![PortType::Value(self.i1)];
        outputs.extend(plan.port_types[..plan.ports].iter().copied());
        outputs.push(PortType::State);
        self.graph
            .add_node(kinds::GAMMA, &inputs, &outputs, &regions, Vec::new())
    }

    /// The γ deciding the θ continuation predicate once a `broke` flag exists:
    /// re-evaluate the condition region on the path that did not leave, and stop
    /// on the path that did.
    fn raise_continuation(
        &mut self,
        inner: &mut Scope,
        plan: &LoopPlan,
        broke: Origin,
        latched: &[Origin],
    ) -> Result<Vec<Origin>, Error> {
        let condition_block = self.entry_block(plan.condition_region)?;
        let free = self.free_values(&[plan.condition_region], inner);
        let captured = inner.resolve_all(&free)?;

        let mut argument_types: Vec<PortType> = plan.port_types[..plan.ports].to_vec();
        argument_types.extend(captured.iter().map(|&o| self.graph.origin_type(o)));
        argument_types.push(PortType::State);
        let state_port = (argument_types.len() - 1) as u32;

        let running = self.graph.open_region(&argument_types);
        let entry: Vec<Origin> = (0..plan.ports)
            .map(|port| Origin::argument(running, port as u32))
            .collect();
        let mut running_scope = Scope {
            bindings: free
                .iter()
                .enumerate()
                .map(|(index, &value)| {
                    (
                        value,
                        Origin::argument(running, (plan.ports + index) as u32),
                    )
                })
                .collect(),
            state: Origin::argument(running, state_port),
        };
        let (predicate, forwarded) =
            self.inline_condition(&mut running_scope, &condition_block, &entry)?;
        let mut results = vec![predicate];
        results.extend(forwarded);
        results.push(running_scope.state);
        self.graph.close_region(running, &results)?;

        let left = self.graph.open_region(&argument_types);
        let zero = self.constant(0)?;
        let mut results = vec![zero];
        results.extend((0..plan.ports).map(|port| Origin::argument(left, port as u32)));
        results.push(Origin::argument(left, state_port));
        self.graph.close_region(left, &results)?;

        let mut inputs = vec![broke];
        inputs.extend_from_slice(latched);
        inputs.extend_from_slice(&captured);
        inputs.push(inner.state);
        let mut outputs = vec![PortType::Value(self.i1)];
        outputs.extend(plan.port_types[..plan.ports].iter().copied());
        outputs.push(PortType::State);
        let gamma = self.graph.add_node(
            kinds::GAMMA,
            &inputs,
            &outputs,
            &[running, left],
            Vec::new(),
        )?;

        inner.state = Origin::output(gamma, (plan.ports + 1) as u32);
        let mut results = vec![Origin::output(gamma, 0)];
        results.extend((0..plan.ports).map(|port| Origin::output(gamma, (port + 1) as u32)));
        Ok(results)
    }

    /// Raise a condition region's operations into the region `scope` names,
    /// binding its block arguments to `arguments`, and report the predicate and
    /// the forwarded tuple.
    fn inline_condition(
        &mut self,
        scope: &mut Scope,
        block: &Arc<Block>,
        arguments: &[Origin],
    ) -> Result<(Origin, Vec<Origin>), Error> {
        for (argument, &origin) in block.arguments().iter().zip(arguments) {
            scope.bind(argument.id(), origin);
        }
        let tail = self.raise_block(scope, block)?;
        let Tail::Condition(predicate, forwarded) = tail else {
            return Err(Error::new(
                "an scf.while condition region must end with scf.condition",
            ));
        };
        Ok((scope.resolve(predicate)?, scope.resolve_all(&forwarded)?))
    }

    /// The `scf.if` immediately before the body's terminator, when the body's
    /// exits live in its arms. `None` when the loop has no conditional exit.
    fn find_exit_conditional(
        &self,
        plan: &LoopPlan,
        rest: &[OpId],
        terminator: &Arc<OpInstance>,
    ) -> Result<Option<OpId>, Error> {
        let Some(token) = plan.loop_scope else {
            return Ok(None);
        };
        if exits_through(terminator, token) {
            return Ok(None);
        }
        let mut found = None;
        for &id in rest {
            let op = self.context.get_op(id);
            if !self.contains_exit(&op, token) {
                continue;
            }
            if found.is_some() || id != *rest.last().expect("non-empty") {
                return Err(Error::new(
                    "a loop exit that is not the body's terminator must be the sole exit and \
                     sit in the conditional immediately before it; anything deeper needs \
                     predicate lifting over the code following the exit",
                ));
            }
            if !op.is::<scf::IfOp>() {
                return Err(Error::new(format!(
                    "a loop exit nested in {}.{} has no mapping; only scf.if arms are lifted",
                    op.dialect(),
                    op.name()
                )));
            }
            for &arm in &op.regions {
                let block = self.entry_block(arm)?;
                let last = self
                    .context
                    .get_op(*block.op_ids().last().expect("terminator"));
                if self.contains_exit(&last, token) && !exits_through(&last, token) {
                    return Err(Error::new(
                        "a loop exit nested deeper than the arm's terminator needs predicate \
                         lifting",
                    ));
                }
                for &id in block.op_ids().iter().rev().skip(1) {
                    if self.contains_exit(&self.context.get_op(id), token) {
                        return Err(Error::new(
                            "a loop exit nested deeper than the arm's terminator needs \
                             predicate lifting",
                        ));
                    }
                }
            }
            found = Some(id);
        }
        Ok(found)
    }

    fn contains_exit(&self, op: &Arc<OpInstance>, token: ValueId) -> bool {
        if exits_through(op, token) {
            return true;
        }
        op.regions.iter().any(|&region| {
            self.context
                .get_region(region)
                .iter(self.context.clone())
                .any(|block| {
                    block
                        .op_ids()
                        .iter()
                        .any(|&id| self.contains_exit(&self.context.get_op(id), token))
                })
        })
    }

    /// A γ arm's scope: the captured values in argument order, then the state.
    fn arm_scope(&self, region: RegionId, free: &[ValueId]) -> Scope {
        Scope {
            bindings: free
                .iter()
                .enumerate()
                .map(|(index, &value)| (value, Origin::argument(region, index as u32)))
                .collect(),
            state: Origin::argument(region, free.len() as u32),
        }
    }

    fn constant(&mut self, value: i64) -> Result<Origin, Error> {
        let op = self.graph.op_type("builtin", "constant");
        let node = self.graph.add_node(
            op,
            &[],
            &[PortType::Value(self.i1)],
            &[],
            vec![NamedAttribute::new("value", AttributeValue::Int(value))],
        )?;
        Ok(Origin::output(node, 0))
    }

    /// Whether the condition region is a constant `true` forwarding its
    /// arguments — the shape fcc's `do` lowering emits, which needs no rotation.
    fn is_trivially_true(&self, block: &Arc<Block>, arguments: &[ValueId]) -> bool {
        let ops = block.op_ids();
        let [constant, condition] = ops[..] else {
            return false;
        };
        let constant = self.context.get_op(constant);
        let condition = self.context.get_op(condition);
        constant.is::<crate::builtin::ConstantOp>()
            && condition.is::<scf::ConditionOp>()
            && constant.results.first() == condition.operands.first()
            && condition.operands[1..] == *arguments
            && constant.attributes.iter().any(|attribute| {
                attribute.name == "value" && attribute.value == AttributeValue::Int(1)
            })
    }

    fn entry_block(&self, region: IrRegion) -> Result<Arc<Block>, Error> {
        let region = self.context.get_region(region);
        let mut blocks = region.iter(self.context.clone());
        let block = blocks
            .next()
            .ok_or_else(|| Error::new("a structured region must have an entry block"))?;
        if blocks.next().is_some() {
            return Err(Error::new(
                "a structured region with more than one block is unstructured control flow",
            ));
        }
        Ok(block)
    }

    /// Every value a set of regions reads from `scope`, in first-use order.
    fn free_values(&self, regions: &[IrRegion], scope: &Scope) -> Vec<ValueId> {
        let mut free = Vec::new();
        for &region in regions {
            self.collect_free(region, scope, &mut free);
        }
        free
    }

    fn collect_free(&self, region: IrRegion, scope: &Scope, free: &mut Vec<ValueId>) {
        for block in self.context.get_region(region).iter(self.context.clone()) {
            for id in block.op_ids() {
                let op = self.context.get_op(id);
                for &operand in &op.operands {
                    if scope.lookup(operand).is_some() && !free.contains(&operand) {
                        free.push(operand);
                    }
                }
                for &nested in &op.regions {
                    self.collect_free(nested, scope, free);
                }
            }
        }
    }
}

/// The per-loop facts the θ builder needs.
struct LoopPlan {
    condition_region: IrRegion,
    ports: usize,
    /// The body block's carried arguments, aligned with the loop's ports.
    carried_arguments: Vec<ValueId>,
    loop_scope: Option<ValueId>,
    free: Vec<ValueId>,
    port_types: Vec<PortType>,
    body_block: Arc<Block>,
}

fn exits_through(op: &Arc<OpInstance>, token: ValueId) -> bool {
    (op.is::<scf::BreakOp>() || op.is::<scf::ContinueOp>()) && op.operands[0] == token
}

/// Conservatively, an operation is stateful unless it is a pure computation over
/// its operands. Over-threading the state edge only over-orders; under-threading
/// it would lose a dependency.
fn is_stateful(op: &Arc<OpInstance>) -> bool {
    !op.has_interface::<dyn crate::ConstantFold>() && !op.has_interface::<dyn crate::ConstantLike>()
}
