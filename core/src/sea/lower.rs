//! Destruction: rebuilding `scf` from the sea.
//!
//! γ becomes `scf.if` — region 1 is the `then` arm, region 0 the `else` arm, so
//! the predicate still selects the region with its own index. θ becomes the
//! `do`-shaped `scf.while` that matches its semantics exactly: a condition
//! region that is constantly true, and a body that ends by testing the
//! continuation predicate and breaking when it is false.
//!
//! Round-tripping is therefore textually stable for `do` loops and for
//! straight-line and conditional code, and turns a pre-tested `scf.while` into
//! the rotated `scf.if { scf.while … }` its θ normalization means. That
//! difference is semantic-preserving by construction, which is what the fcc
//! end-to-end suite gates.

use std::sync::Arc;

use crate::builtin::{IntegerType, TokenType};
use crate::dialects::scf;
use crate::{Block, Context, OpInstance, Operation, TypeId, Value, ValueId};

use super::Error;
use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::kinds;
use super::raise::Raised;

/// Rebuild `raised`'s function body into `block`, whose arguments are the
/// function's parameters. `block` must be empty.
pub fn lower_function(context: &Context, raised: &Raised, block: &Arc<Block>) -> Result<(), Error> {
    let graph = &raised.graph;
    let region = graph.subregions(raised.lambda)[0];
    let arguments: Vec<Option<ValueId>> = block
        .arguments()
        .iter()
        .map(|value| Some(value.id()))
        .chain(std::iter::once(None))
        .collect();
    if arguments.len() != graph.region_arguments(region).len() {
        return Err(Error::new(
            "the λ region takes a different number of arguments than the function block",
        ));
    }

    let lowerer = Lowerer { context, graph };
    let results = lowerer.lower_region(region, &arguments, block)?;
    let returned = results.iter().flatten().copied().collect::<Vec<_>>();
    let operand = match returned.as_slice() {
        [] => crate::Operand::none(),
        [value] => crate::Operand::some(*value),
        _ => return Err(Error::new("a function may only return one value")),
    };
    append(
        context,
        block,
        crate::builtin::ops::r#return(context, operand).build(),
    );
    Ok(())
}

struct Lowerer<'a> {
    context: &'a Context,
    graph: &'a Graph,
}

/// The value each origin carries, or `None` for the state edges, which have no
/// `scf` spelling.
type Bindings = Vec<(Origin, Option<ValueId>)>;

impl Lowerer<'_> {
    /// Emit `region`'s nodes into `block` and report what its results carry.
    fn lower_region(
        &self,
        region: RegionId,
        arguments: &[Option<ValueId>],
        block: &Arc<Block>,
    ) -> Result<Vec<Option<ValueId>>, Error> {
        let mut bindings: Bindings = arguments
            .iter()
            .enumerate()
            .map(|(index, &value)| (Origin::argument(region, index as u32), value))
            .collect();

        for &node in self.graph.region_nodes(region) {
            let op = self.graph.op_of(node);
            let outputs = if op == kinds::GAMMA {
                self.lower_gamma(node, &bindings, block)?
            } else if op == kinds::THETA {
                self.lower_theta(node, &bindings, block)?
            } else if kinds::is_structural(op) {
                let (dialect, name) = self.graph.op_type_name(op);
                return Err(Error::new(format!("{dialect}.{name} has no scf spelling")));
            } else {
                self.lower_simple(node, &bindings, block)?
            };
            for (port, value) in outputs.into_iter().enumerate() {
                bindings.push((Origin::output(node, port as u32), value));
            }
        }

        self.graph
            .region_results(region)
            .iter()
            .map(|&origin| resolve(&bindings, origin))
            .collect()
    }

    fn lower_simple(
        &self,
        node: NodeId,
        bindings: &Bindings,
        block: &Arc<Block>,
    ) -> Result<Vec<Option<ValueId>>, Error> {
        let (dialect, name) = self.graph.op_type_name(self.graph.op_of(node));
        let mut operands = Vec::new();
        for &origin in self.graph.inputs(node) {
            if let Some(value) = resolve(bindings, origin)? {
                operands.push(value);
            }
        }
        let outputs: Vec<Option<ValueId>> = self
            .graph
            .outputs(node)
            .iter()
            .map(|port| {
                port.value_type()
                    .map(|ty| self.context.create_value(ty, None).id())
            })
            .collect();
        let results: Vec<ValueId> = outputs.iter().flatten().copied().collect();

        let instance = OpInstance::new_dynamic(
            (dialect, name),
            self.context.as_context_ref(),
            operands,
            results,
            Vec::new(),
            self.graph.attributes(node).to_vec(),
        );
        let instance = self.context.add_operation(instance);
        block.insert(block.len(), instance.id);
        Ok(outputs)
    }

    fn lower_gamma(
        &self,
        node: NodeId,
        bindings: &Bindings,
        block: &Arc<Block>,
    ) -> Result<Vec<Option<ValueId>>, Error> {
        let inputs = self.graph.inputs(node);
        let condition = resolve(bindings, inputs[0])?
            .ok_or_else(|| Error::new("a γ predicate cannot be a state edge"))?;
        let captured: Vec<Option<ValueId>> = inputs[1..]
            .iter()
            .map(|&origin| resolve(bindings, origin))
            .collect::<Result<_, _>>()?;

        let regions = self.graph.subregions(node);
        let [otherwise, taken] = regions else {
            return Err(Error::new(format!(
                "an scf.if is a two-way γ, but this γ has {} regions",
                regions.len()
            )));
        };
        let then_body = self.lower_arm(*taken, &captured)?;
        let else_body = self.lower_arm(*otherwise, &captured)?;

        let result_types = self.value_types(self.graph.outputs(node));
        let op = scf::ops::r#if(
            self.context,
            condition,
            result_types,
            Some(then_body),
            Some(else_body),
        )
        .build();
        let results = append(self.context, block, op);
        Ok(self.distribute(self.graph.outputs(node), &results))
    }

    /// Build one γ arm as an `scf.if` region: a single block that ends in the
    /// `scf.yield` carrying the region's value results.
    fn lower_arm(
        &self,
        region: RegionId,
        captured: &[Option<ValueId>],
    ) -> Result<crate::RegionId, Error> {
        let (ir_region, block) = self.new_region(Vec::new());
        let results = self.lower_region(region, captured, &block)?;
        let yielded: Vec<ValueId> = results.into_iter().flatten().collect();
        append(
            self.context,
            &block,
            scf::ops::r#yield(self.context, yielded).build(),
        );
        Ok(ir_region)
    }

    fn lower_theta(
        &self,
        node: NodeId,
        bindings: &Bindings,
        block: &Arc<Block>,
    ) -> Result<Vec<Option<ValueId>>, Error> {
        let ports = self.graph.outputs(node).to_vec();
        let carried_types = self.value_types(&ports);
        let inits: Vec<ValueId> = self
            .graph
            .inputs(node)
            .iter()
            .map(|&origin| resolve(bindings, origin))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();

        let condition_region = self.always_true_condition(&carried_types);

        let token = self
            .context
            .create_value(TokenType::new(self.context), None);
        let mut body_arguments = vec![token.clone()];
        body_arguments.extend(
            carried_types
                .iter()
                .map(|&ty| self.context.create_value(ty, None)),
        );
        let (body_region, body_block) = self.new_region(body_arguments.clone());

        let arguments = self.distribute(
            &ports,
            &body_arguments[1..]
                .iter()
                .map(Value::id)
                .collect::<Vec<_>>(),
        );
        let results = self.lower_region(self.graph.subregions(node)[0], &arguments, &body_block)?;
        let predicate = results
            .first()
            .copied()
            .flatten()
            .ok_or_else(|| Error::new("a θ region must yield a continuation predicate"))?;
        let latched: Vec<ValueId> = results[1..].iter().flatten().copied().collect();

        let (repeat, repeat_block) = self.new_region(Vec::new());
        append(
            self.context,
            &repeat_block,
            scf::ops::r#yield(self.context, Vec::new()).build(),
        );
        let (exit, exit_block) = self.new_region(Vec::new());
        append(
            self.context,
            &exit_block,
            scf::ops::r#break(self.context, token.id(), latched.clone()).build(),
        );
        append(
            self.context,
            &body_block,
            scf::ops::r#if(
                self.context,
                predicate,
                Vec::new(),
                Some(repeat),
                Some(exit),
            )
            .build(),
        );
        append(
            self.context,
            &body_block,
            scf::ops::r#yield(self.context, latched).build(),
        );

        let op = scf::ops::r#while(
            self.context,
            inits,
            carried_types,
            Some(condition_region),
            Some(body_region),
        )
        .build();
        let results = append(self.context, block, op);
        Ok(self.distribute(&ports, &results))
    }

    /// The `scf.while` condition region a θ needs: constantly true, forwarding
    /// its arguments, so the loop is the tail-controlled one θ describes.
    fn always_true_condition(&self, carried: &[TypeId]) -> crate::RegionId {
        let arguments: Vec<Value> = carried
            .iter()
            .map(|&ty| self.context.create_value(ty, None))
            .collect();
        let (region, block) = self.new_region(arguments.clone());

        let i1 = IntegerType::new(self.context, 1);
        let constant = crate::builtin::ops::constant(self.context, 1, i1).build();
        let true_value = self.context.get_op(constant.id()).results[0];
        append(self.context, &block, constant);
        append(
            self.context,
            &block,
            scf::ops::condition(
                self.context,
                true_value,
                arguments.iter().map(Value::id).collect(),
            )
            .build(),
        );
        region
    }

    fn new_region(&self, arguments: Vec<Value>) -> (crate::RegionId, Arc<Block>) {
        let region = self.context.create_region();
        let block = self.context.create_block(arguments);
        region.add_block(block.id());
        (region.id(), block)
    }

    fn value_types(&self, ports: &[PortType]) -> Vec<TypeId> {
        ports.iter().filter_map(|port| port.value_type()).collect()
    }

    /// Spread `values`, which cover only the value ports, back over the full port
    /// tuple.
    fn distribute(&self, ports: &[PortType], values: &[ValueId]) -> Vec<Option<ValueId>> {
        let mut values = values.iter();
        ports
            .iter()
            .map(|port| port.value_type().and(values.next().copied()))
            .collect()
    }
}

fn resolve(bindings: &Bindings, origin: Origin) -> Result<Option<ValueId>, Error> {
    bindings
        .iter()
        .rev()
        .find(|(bound, _)| *bound == origin)
        .map(|(_, value)| *value)
        .ok_or_else(|| Error::new(format!("origin {origin:?} has no lowered value")))
}

fn append<T: Operation>(context: &Context, block: &Arc<Block>, op: T) -> Vec<ValueId> {
    let id = op.id();
    block.insert(block.len(), id);
    context.get_op(id).results.clone()
}
