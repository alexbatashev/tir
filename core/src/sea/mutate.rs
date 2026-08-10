//! The mutation API. Single-writer discipline is `&mut Graph`, not locks.
//!
//! Every mutation is atomic: it validates the whole edit before touching a
//! column, so a rejected edit leaves the graph exactly as it was. Each one bumps
//! the version stamp of the nodes it rewrote and of the structural nodes on the
//! spine from there to ω, so a view keyed by `(node, version)` stays warm
//! everywhere the edit did not reach.

use super::Error;
use super::graph::{Graph, NodeId, OpTypeId, Origin, RegionId};

/// Every use of `node`'s outputs inside its region: node inputs as
/// `(user, input index)`, and region results as their index.
fn users(graph: &Graph, node: NodeId, region: RegionId) -> (Vec<(NodeId, usize)>, Vec<usize>) {
    let mut inputs = Vec::new();
    for &other in graph.region_nodes(region) {
        for (index, &origin) in graph.inputs(other).iter().enumerate() {
            if origin.node() == Some(node) {
                inputs.push((other, index));
            }
        }
    }
    let results = graph
        .region_results(region)
        .iter()
        .enumerate()
        .filter(|(_, origin)| origin.node() == Some(node))
        .map(|(index, _)| index)
        .collect();
    (inputs, results)
}

/// Redirect every user of `old`'s outputs to the matching output of `new`, which
/// must produce the same tuple in the same region.
///
/// `new` is rescheduled to sit immediately after `old`, together with any node it
/// depends on that was built after `old`; their relative order is preserved. That
/// is what keeps the region's schedule topological: everything that used `old`
/// was scheduled after it, so it is still scheduled after `new`.
pub fn replace_subtree(graph: &mut Graph, old: NodeId, new: NodeId) -> Result<(), Error> {
    if old == new {
        return Ok(());
    }
    let region = graph
        .parent_region(old)
        .ok_or_else(|| Error::new("cannot replace a node that belongs to no region"))?;
    if graph.parent_region(new) != Some(region) {
        return Err(Error::new(
            "a replacement must be built in the same region as the node it replaces",
        ));
    }
    if graph.outputs(new) != graph.outputs(old) {
        return Err(Error::new(format!(
            "replacement node {} produces a different output tuple than node {}",
            new.number(),
            old.number()
        )));
    }

    let anchor = graph.position(old).expect("scheduled node");
    let moved = dependencies_after(graph, new, region, anchor);
    let (input_users, result_users) = users(graph, old, region);

    for (position, &node) in moved.iter().enumerate() {
        graph.reschedule(node, anchor + 1 + position);
    }
    for &(user, index) in &input_users {
        let port = graph.inputs(user)[index].port();
        graph.set_input(user, index, Origin::output(new, port));
        graph.bump(user);
    }
    for &index in &result_users {
        let port = graph.region_results(region)[index].port();
        graph.set_region_result(region, index, Origin::output(new, port));
    }
    graph.bump(new);
    bump_spine(graph, region);
    Ok(())
}

/// Retarget one edge: `user`'s input `index` now reads `origin`.
pub fn splice(graph: &mut Graph, user: NodeId, index: usize, origin: Origin) -> Result<(), Error> {
    let region = graph
        .parent_region(user)
        .ok_or_else(|| Error::new("cannot splice an input of a node that belongs to no region"))?;
    let old = *graph
        .inputs(user)
        .get(index)
        .ok_or_else(|| Error::new(format!("node {} has no input {index}", user.number())))?;
    if graph.origin_type(origin) != graph.origin_type(old) {
        return Err(Error::new(format!(
            "spliced origin {origin:?} does not have the type node {} input {index} reads",
            user.number()
        )));
    }
    if let Some(source) = origin.node() {
        let position = graph.position(user).expect("scheduled node");
        match graph.position(source) {
            Some(at) if at < position && graph.parent_region(source) == Some(region) => {}
            _ => {
                return Err(Error::new(format!(
                    "spliced origin {origin:?} is not scheduled before node {} in its region",
                    user.number()
                )));
            }
        }
    } else if origin.region() != Some(region) {
        return Err(Error::new(format!(
            "spliced origin {origin:?} is not an argument of the region node {} lives in",
            user.number()
        )));
    }

    graph.set_input(user, index, origin);
    graph.bump(user);
    bump_spine(graph, region);
    Ok(())
}

/// Move `node` into a fresh region of a new single-region structural node of type
/// `op`, which takes the node's place. The wrapper reads what `node` read and
/// produces what `node` produced, and the region routes those through its
/// arguments and results — the shape a γ arm or a θ body starts from.
pub fn wrap(graph: &mut Graph, node: NodeId, op: OpTypeId) -> Result<NodeId, Error> {
    let region = graph
        .parent_region(node)
        .ok_or_else(|| Error::new("cannot wrap a node that belongs to no region"))?;
    let anchor = graph.position(node).expect("scheduled node");
    let inputs = graph.inputs(node).to_vec();
    let outputs = graph.outputs(node).to_vec();
    let argument_types = graph.input_types(node);

    let body = graph.open_region(&argument_types);
    graph.move_into(node, body);
    for index in 0..inputs.len() {
        graph.set_input(node, index, Origin::argument(body, index as u32));
    }
    let results: Vec<Origin> = (0..outputs.len())
        .map(|port| Origin::output(node, port as u32))
        .collect();
    graph.close_region(body, &results)?;

    let wrapper = graph.insert_node(region, anchor, op, &inputs, &outputs, &[body], Vec::new())?;
    let (input_users, result_users) = users(graph, node, region);
    for (user, index) in input_users {
        let port = graph.inputs(user)[index].port();
        graph.set_input(user, index, Origin::output(wrapper, port));
        graph.bump(user);
    }
    for index in result_users {
        let port = graph.region_results(region)[index].port();
        graph.set_region_result(region, index, Origin::output(wrapper, port));
    }
    bump_spine(graph, region);
    Ok(wrapper)
}

/// `node` plus everything it transitively reads in `region` that is scheduled
/// after `anchor`, in schedule order.
fn dependencies_after(graph: &Graph, node: NodeId, region: RegionId, anchor: usize) -> Vec<NodeId> {
    let mut selected = vec![node];
    let mut queue = vec![node];
    while let Some(current) = queue.pop() {
        for &origin in graph.inputs(current) {
            let Some(source) = origin.node() else {
                continue;
            };
            let Some(position) = graph.position(source) else {
                continue;
            };
            if position <= anchor || selected.contains(&source) {
                continue;
            }
            selected.push(source);
            queue.push(source);
        }
    }
    let mut order: Vec<NodeId> = graph
        .region_nodes(region)
        .iter()
        .copied()
        .filter(|node| selected.contains(node))
        .collect();
    order.dedup();
    order
}

/// Bump every structural node from `region`'s owner up to ω.
fn bump_spine(graph: &mut Graph, region: RegionId) {
    let mut current = Some(region);
    while let Some(region) = current {
        let Some(owner) = graph.region_owner(region) else {
            break;
        };
        graph.bump(owner);
        current = graph.parent_region(owner);
    }
}
