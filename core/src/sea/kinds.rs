//! The canonical structural op types and their signature laws.
//!
//! Per the RVSDG specification (Reissmann et al. 2020), restated in
//! `docs/design/sea.md`:
//!
//! * **γ (`sea.gamma`)** — decision. `k+1` regions of matching signature; the
//!   first input is an integer predicate selecting one of them; every region's
//!   argument tuple equals the remaining inputs and every region's result tuple
//!   equals the node's outputs. n-ary from day one: a switch is one γ.
//! * **θ (`sea.theta`)** — loop. One region; the input, output and region
//!   argument tuples are equal; the region's *first* result is the continuation
//!   predicate and the rest are the next iteration's carried values. Tail
//!   controlled: the body always runs once.
//! * **λ (`sea.lambda`)** — function. One region whose leading arguments are the
//!   node's context variables and whose remaining arguments are the function's
//!   parameters; the region's results are the function's results. The node's
//!   single output *is* the function value, fed to apply nodes by ordinary
//!   edges.
//! * **ω (`sea.omega`)** — translation unit. One region; its arguments are the
//!   imports and its results the exports. No inputs, no outputs.
//!
//! δ (globals with initializer regions) and φ (mutual recursion) are the two
//! remaining inter-procedural kinds. They slot in here as two more interned op
//! types with their own laws — δ as a single-region node with one output and no
//! predicate, φ as a single-region node whose region arguments and results are
//! the recursion group. Nothing outside this file would change; this increment
//! has no globals or recursion consumer, so they are not defined yet.

use super::Error;
use super::graph::{Graph, NodeId, OpTypeId, OpTypeTable, PortType};

pub const GAMMA: OpTypeId = OpTypeId::new(0);
pub const THETA: OpTypeId = OpTypeId::new(1);
pub const LAMBDA: OpTypeId = OpTypeId::new(2);
pub const OMEGA: OpTypeId = OpTypeId::new(3);

pub const DIALECT: &str = "sea";

/// Intern the canonical kinds first, so their ids are the constants above.
pub(super) fn intern_all(table: &mut OpTypeTable) {
    assert_eq!(table.intern(DIALECT, "gamma"), GAMMA);
    assert_eq!(table.intern(DIALECT, "theta"), THETA);
    assert_eq!(table.intern(DIALECT, "lambda"), LAMBDA);
    assert_eq!(table.intern(DIALECT, "omega"), OMEGA);
}

pub fn is_structural(op: OpTypeId) -> bool {
    op == GAMMA || op == THETA || op == LAMBDA || op == OMEGA
}

/// Check the signature laws of `node`'s kind. Simple nodes have none: their
/// arity and types are the dialect op's business, verified by the op's own
/// verifier once the graph is destructed back to ops.
pub fn verify_node(graph: &Graph, node: NodeId) -> Result<(), Error> {
    let op = graph.op_of(node);
    let result = if op == GAMMA {
        verify_gamma(graph, node)
    } else if op == THETA {
        verify_theta(graph, node)
    } else if op == LAMBDA {
        verify_lambda(graph, node)
    } else if op == OMEGA {
        verify_omega(graph, node)
    } else {
        Ok(())
    };
    result.map_err(|e| {
        let (dialect, name) = graph.op_type_name(op);
        e.context(format!("{dialect}.{name} node {}", node.number()))
    })
}

fn verify_gamma(graph: &Graph, node: NodeId) -> Result<(), Error> {
    let regions = graph.subregions(node);
    if regions.len() < 2 {
        return Err(Error::new(format!(
            "γ must have at least two regions, has {}",
            regions.len()
        )));
    }
    let inputs = graph.input_types(node);
    match inputs.first() {
        Some(PortType::Value(_)) => {}
        Some(PortType::State) => {
            return Err(Error::new("γ predicate input must be a value, not state"));
        }
        None => return Err(Error::new("γ must have a predicate input")),
    }
    let outputs = graph.outputs(node).to_vec();
    for (index, &region) in regions.iter().enumerate() {
        expect_tuple(
            graph.region_arguments(region),
            &inputs[1..],
            &format!("γ region {index} arguments"),
            "the γ inputs after the predicate",
        )?;
        expect_tuple(
            &graph.region_result_types(region),
            &outputs,
            &format!("γ region {index} results"),
            "the γ outputs",
        )?;
    }
    Ok(())
}

fn verify_theta(graph: &Graph, node: NodeId) -> Result<(), Error> {
    let regions = graph.subregions(node);
    let [region] = regions else {
        return Err(Error::new(format!(
            "θ must have exactly one region, has {}",
            regions.len()
        )));
    };
    let region = *region;
    let inputs = graph.input_types(node);
    let outputs = graph.outputs(node).to_vec();
    expect_tuple(
        &outputs,
        &inputs,
        "θ outputs",
        "the θ inputs (the signature is loop-invariant)",
    )?;
    expect_tuple(
        graph.region_arguments(region),
        &inputs,
        "θ region arguments",
        "the θ inputs",
    )?;

    let results = graph.region_result_types(region);
    let Some(&predicate) = results.first() else {
        return Err(Error::new(
            "θ region must produce a continuation predicate as its first result",
        ));
    };
    if predicate != PortType::Value(graph.predicate_type()) {
        return Err(Error::new(format!(
            "θ region result 0 must be the continuation predicate, but is {}",
            describe(predicate)
        )));
    }
    expect_tuple(
        &results[1..],
        &inputs,
        "θ region results after the predicate",
        "the θ inputs",
    )
}

fn verify_lambda(graph: &Graph, node: NodeId) -> Result<(), Error> {
    let regions = graph.subregions(node);
    let [region] = regions else {
        return Err(Error::new(format!(
            "λ must have exactly one region, has {}",
            regions.len()
        )));
    };
    let region = *region;
    let outputs = graph.outputs(node);
    match outputs {
        [PortType::Value(_)] => {}
        _ => {
            return Err(Error::new(format!(
                "λ must have exactly one value output — the function itself — but has {}",
                outputs.len()
            )));
        }
    }
    let context_variables = graph.input_types(node);
    let arguments = graph.region_arguments(region);
    if arguments.len() < context_variables.len() {
        return Err(Error::new(format!(
            "λ has {} context variables but its region takes only {} arguments",
            context_variables.len(),
            arguments.len()
        )));
    }
    expect_tuple(
        &arguments[..context_variables.len()],
        &context_variables,
        "λ region leading arguments",
        "the λ context variables",
    )
}

fn verify_omega(graph: &Graph, node: NodeId) -> Result<(), Error> {
    if graph.subregions(node).len() != 1 {
        return Err(Error::new(format!(
            "ω must have exactly one region, has {}",
            graph.subregions(node).len()
        )));
    }
    if !graph.inputs(node).is_empty() || !graph.outputs(node).is_empty() {
        return Err(Error::new(
            "ω is the translation unit: imports are region arguments and exports are \
             region results, so it has no inputs and no outputs",
        ));
    }
    Ok(())
}

fn expect_tuple(
    found: &[PortType],
    expected: &[PortType],
    what: &str,
    against: &str,
) -> Result<(), Error> {
    if found.len() != expected.len() {
        return Err(Error::new(format!(
            "{what} has {} ports but {against} has {}",
            found.len(),
            expected.len()
        )));
    }
    for (index, (&found, &expected)) in found.iter().zip(expected).enumerate() {
        if found != expected {
            return Err(Error::new(format!(
                "{what} port {index} is {}, but {against} port {index} is {}",
                describe(found),
                describe(expected)
            )));
        }
    }
    Ok(())
}

fn describe(port: PortType) -> String {
    match port {
        PortType::Value(ty) => format!("value of type #{}", ty.number()),
        PortType::State => "state".to_string(),
    }
}
