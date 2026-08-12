//! Loop restructuring: every strongly connected component becomes one
//! tail-controlled loop with a single entry and a single exit.
//!
//! The component keeps all its nodes. What changes is where its edges land: an
//! edge entering it goes to a new head, naming the entry it wanted in a
//! dispatch variable; an edge closing it or leaving it goes to a new tail,
//! naming whether to repeat and, when leaving, which exit it wanted. The body
//! is then restructured again, so an inner component becomes an inner loop.

use std::collections::{BTreeMap, BTreeSet};

use super::cfg::{Cfg, Edge, Loop, Node, NodeId, Rhs, Src, Term, VarId};

/// An edge to retarget: its position in the node's terminator, where it should
/// land instead, and what it assigns on the way.
type Rewrite = (usize, NodeId, Vec<(VarId, Rhs)>);

pub fn restructure(cfg: &mut Cfg) {
    let nodes = reachable(cfg, cfg.entry);
    let mut entry = cfg.entry;
    restructure_region(cfg, nodes, &mut entry, cfg.sink);
    cfg.entry = entry;
}

/// The nodes reachable from `from`, a loop tail's edges excluded: they leave
/// the body they terminate.
fn reachable(cfg: &Cfg, from: NodeId) -> BTreeSet<NodeId> {
    let mut seen = BTreeSet::from([from]);
    let mut pending = vec![from];
    while let Some(node) = pending.pop() {
        for successor in cfg.successors(node) {
            if seen.insert(successor) {
                pending.push(successor);
            }
        }
    }
    seen
}

fn restructure_region(cfg: &mut Cfg, nodes: BTreeSet<NodeId>, entry: &mut NodeId, exit: NodeId) {
    let mut nodes = nodes;
    for component in components(cfg, &nodes) {
        restructure_loop(cfg, &mut nodes, entry, exit, component);
    }
}

/// The successors of `node` inside `nodes`. A loop tail closes the body it
/// belongs to: its edges lead out of the region being restructured.
fn inner_successors(cfg: &Cfg, node: NodeId, nodes: &BTreeSet<NodeId>) -> Vec<NodeId> {
    if matches!(cfg.nodes[node].term, Term::LoopTail { .. }) {
        return vec![];
    }
    cfg.successors(node)
        .into_iter()
        .filter(|successor| nodes.contains(successor))
        .collect()
}

/// The cyclic strongly connected components of the subgraph on `nodes`, in
/// ascending order of their smallest node.
fn components(cfg: &Cfg, nodes: &BTreeSet<NodeId>) -> Vec<Vec<NodeId>> {
    let mut state = Tarjan {
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        on_stack: BTreeSet::new(),
        stack: Vec::new(),
        next: 0,
        components: Vec::new(),
    };
    for &node in nodes {
        if !state.index.contains_key(&node) {
            state.run(cfg, nodes, node);
        }
    }
    let mut components = state
        .components
        .into_iter()
        .filter(|component| {
            component.len() > 1
                || inner_successors(cfg, component[0], nodes).contains(&component[0])
        })
        .map(|mut component| {
            component.sort_unstable();
            component
        })
        .collect::<Vec<_>>();
    components.sort_unstable();
    components
}

struct Tarjan {
    index: BTreeMap<NodeId, usize>,
    low: BTreeMap<NodeId, usize>,
    on_stack: BTreeSet<NodeId>,
    stack: Vec<NodeId>,
    next: usize,
    components: Vec<Vec<NodeId>>,
}

impl Tarjan {
    /// Iterative Tarjan: `work` holds each open node with the index of the
    /// successor to visit next.
    fn run(&mut self, cfg: &Cfg, nodes: &BTreeSet<NodeId>, root: NodeId) {
        let mut work = vec![(root, 0usize)];
        self.open(root);
        while let Some((node, step)) = work.pop() {
            let successors = inner_successors(cfg, node, nodes);
            if step < successors.len() {
                work.push((node, step + 1));
                let successor = successors[step];
                if !self.index.contains_key(&successor) {
                    self.open(successor);
                    work.push((successor, 0));
                } else if self.on_stack.contains(&successor) {
                    let low = self.low[&node].min(self.index[&successor]);
                    self.low.insert(node, low);
                }
                continue;
            }
            if self.low[&node] == self.index[&node] {
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(&member);
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                self.components.push(component);
            }
            if let Some(&(parent, _)) = work.last() {
                let low = self.low[&parent].min(self.low[&node]);
                self.low.insert(parent, low);
            }
        }
    }

    fn open(&mut self, node: NodeId) {
        self.index.insert(node, self.next);
        self.low.insert(node, self.next);
        self.next += 1;
        self.stack.push(node);
        self.on_stack.insert(node);
    }
}

/// Rewire one strongly connected component into a single-entry, single-exit
/// tail-controlled loop, then restructure its body.
fn restructure_loop(
    cfg: &mut Cfg,
    nodes: &mut BTreeSet<NodeId>,
    entry: &mut NodeId,
    exit: NodeId,
    component: Vec<NodeId>,
) {
    let members = component.iter().copied().collect::<BTreeSet<_>>();
    let entries = entry_vertices(cfg, nodes, &members, *entry);
    let exits = exit_targets(cfg, &members);

    let entry_var = (entries.len() > 1).then(|| cfg.add_var(cfg.int_type(32)));
    let exit_var = (exits.len() > 1).then(|| cfg.add_var(cfg.int_type(32)));
    let repeat_var = cfg.add_var(cfg.int_type(1));

    let body_entry = dispatch_node(cfg, entry_var, &entries);
    let exit_node = match exits.len() {
        0 => exit,
        _ => dispatch_node(cfg, exit_var, &exits),
    };
    let tail = cfg.add_node(Node {
        block: None,
        assigns: Vec::new(),
        term: Term::LoopTail {
            pred: Src::Var(repeat_var),
            repeat: Edge::new(body_entry),
            exit: Edge::new(exit_node),
        },
        loop_body: None,
    });
    let head = cfg.add_node(Node {
        block: None,
        assigns: Vec::new(),
        term: Term::Jump(Edge::new(body_entry)),
        loop_body: Some(cfg.loops.len()),
    });
    cfg.loops.push(Loop { body_entry, tail });

    let index_of = |list: &[NodeId], node: NodeId| list.iter().position(|&it| it == node).unwrap();
    for &node in nodes.iter() {
        let inside = members.contains(&node);
        let mut rewrites: Vec<Rewrite> = Vec::new();
        for (position, edge) in cfg.edges(node).into_iter().enumerate() {
            let target = edge.target;
            if entries.contains(&target) {
                let mut assigns = Vec::new();
                if let Some(var) = entry_var {
                    assigns.push((var, Rhs::Const(index_of(&entries, target) as i64)));
                }
                if inside {
                    assigns.push((repeat_var, Rhs::Const(1)));
                    rewrites.push((position, tail, assigns));
                } else {
                    rewrites.push((position, head, assigns));
                }
            } else if inside && !members.contains(&target) {
                let mut assigns = vec![(repeat_var, Rhs::Const(0))];
                if let Some(var) = exit_var {
                    assigns.push((var, Rhs::Const(index_of(&exits, target) as i64)));
                }
                rewrites.push((position, tail, assigns));
            }
        }
        for (position, target, assigns) in rewrites {
            let edge = cfg.edges_mut(node).into_iter().nth(position).unwrap();
            edge.target = target;
            edge.assigns.extend(assigns);
        }
    }

    if members.contains(entry) {
        if let Some(var) = entry_var {
            cfg.nodes[head].assigns = vec![(var, Rhs::Const(index_of(&entries, *entry) as i64))];
        }
        *entry = head;
    }

    let mut body = members.clone();
    body.insert(body_entry);
    body.insert(tail);
    for &node in &members {
        nodes.remove(&node);
    }
    nodes.insert(head);
    nodes.insert(exit_node);

    let mut body_entry_slot = body_entry;
    restructure_region(cfg, body, &mut body_entry_slot, tail);
    cfg.loops.last_mut().unwrap().body_entry = body_entry_slot;
    if let Term::Jump(edge) = &mut cfg.nodes[head].term {
        edge.target = body_entry_slot;
    }
    if let Term::LoopTail { repeat, .. } = &mut cfg.nodes[tail].term {
        repeat.target = body_entry_slot;
    }
}

/// The members of `component` an edge from outside it lands on.
fn entry_vertices(
    cfg: &Cfg,
    nodes: &BTreeSet<NodeId>,
    members: &BTreeSet<NodeId>,
    entry: NodeId,
) -> Vec<NodeId> {
    let mut entries = BTreeSet::new();
    if members.contains(&entry) {
        entries.insert(entry);
    }
    for &node in nodes {
        if members.contains(&node) {
            continue;
        }
        for successor in cfg.successors(node) {
            if members.contains(&successor) {
                entries.insert(successor);
            }
        }
    }
    entries.into_iter().collect()
}

/// The nodes outside `members` that an edge from inside lands on.
fn exit_targets(cfg: &Cfg, members: &BTreeSet<NodeId>) -> Vec<NodeId> {
    let mut exits = BTreeSet::new();
    for &node in members {
        for successor in cfg.successors(node) {
            if !members.contains(&successor) {
                exits.insert(successor);
            }
        }
    }
    exits.into_iter().collect()
}

/// One node selecting between `targets` on `var`, or the target itself when
/// there is only one.
fn dispatch_node(cfg: &mut Cfg, var: Option<VarId>, targets: &[NodeId]) -> NodeId {
    let Some(var) = var else {
        return targets[0];
    };
    let (&default, arms) = targets.split_last().expect("a dispatch has targets");
    let arms = arms
        .iter()
        .enumerate()
        .map(|(index, &target)| (index as i64, Edge::new(target)))
        .collect();
    cfg.add_node(Node {
        block: None,
        assigns: Vec::new(),
        term: Term::Dispatch {
            var,
            arms,
            default: Edge::new(default),
        },
        loop_body: None,
    })
}
