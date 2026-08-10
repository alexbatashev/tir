use crate::builtin::IntegerType;
use crate::{Context, TypeId};

use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::{kinds, mutate};

struct Fixture {
    graph: Graph,
    i1: TypeId,
    i32: TypeId,
}

fn fixture() -> Fixture {
    let context = Context::with_default_dialects();
    let i1 = IntegerType::new(&context, 1);
    let i32 = IntegerType::new(&context, 32);
    Fixture {
        graph: Graph::new(i1, &[]),
        i1,
        i32,
    }
}

impl Fixture {
    /// A source of one value of `ty`, standing in for any simple op.
    fn source(&mut self, ty: TypeId) -> NodeId {
        let op = self.graph.op_type("test", "source");
        self.graph
            .add_node(op, &[], &[PortType::Value(ty)], &[], Vec::new())
            .expect("a source has no inputs to reject")
    }

    /// A simple node forwarding `input`.
    fn forward(&mut self, input: Origin, ty: TypeId) -> Result<NodeId, super::Error> {
        let op = self.graph.op_type("test", "forward");
        self.graph
            .add_node(op, &[input], &[PortType::Value(ty)], &[], Vec::new())
    }
}

#[test]
fn an_input_cannot_read_an_origin_from_another_region() {
    let mut f = fixture();
    let outer = f.source(f.i32);

    let region = f.graph.open_region(&[]);
    let error = f
        .forward(Origin::output(outer, 0), f.i32)
        .expect_err("the enclosing region's node is not visible inside a nested region");

    assert!(
        error.message().contains("routed through region arguments"),
        "{error}"
    );
    assert!(f.graph.region_nodes(region).is_empty());
}

/// A γ arm forwarding its single argument.
fn identity_arm(f: &mut Fixture, ty: TypeId) -> RegionId {
    let region = f.graph.open_region(&[PortType::Value(ty)]);
    f.graph
        .close_region(region, &[Origin::argument(region, 0)])
        .expect("an argument is visible in its own region");
    region
}

/// A γ arm forwarding its argument through a node, plus a spare source to splice
/// that node onto.
fn forwarding_arm(f: &mut Fixture) -> (RegionId, NodeId, NodeId) {
    let ty = f.i32;
    let region = f.graph.open_region(&[PortType::Value(ty)]);
    let replacement = f.source(ty);
    let user = f
        .forward(Origin::argument(region, 0), ty)
        .expect("the region's own argument is visible");
    f.graph
        .close_region(region, &[Origin::output(user, 0)])
        .expect("the region's own node is visible");
    (region, replacement, user)
}

#[test]
fn gamma_regions_must_match_the_node_signature() {
    let mut f = fixture();
    let predicate = f.source(f.i1);
    let carried = f.source(f.i32);

    let (i1, i32) = (f.i1, f.i32);
    let matching = identity_arm(&mut f, i32);
    let mismatched = identity_arm(&mut f, i1);

    let error = f
        .graph
        .add_node(
            kinds::GAMMA,
            &[Origin::output(predicate, 0), Origin::output(carried, 0)],
            &[PortType::Value(f.i32)],
            &[matching, mismatched],
            Vec::new(),
        )
        .expect_err("the second region takes a different argument tuple");

    let message = error.message();
    assert!(message.contains("γ region 1 arguments"), "{message}");
    assert!(
        message.contains("the γ inputs after the predicate"),
        "{message}"
    );
}

#[test]
fn theta_must_yield_the_continuation_predicate_first() {
    let mut f = fixture();
    let init = f.source(f.i32);

    let body = f.graph.open_region(&[PortType::Value(f.i32)]);
    f.graph
        .close_region(
            body,
            &[Origin::argument(body, 0), Origin::argument(body, 0)],
        )
        .expect("arguments are visible in their own region");

    let error = f
        .graph
        .add_node(
            kinds::THETA,
            &[Origin::output(init, 0)],
            &[PortType::Value(f.i32)],
            &[body],
            Vec::new(),
        )
        .expect_err("result 0 carries a value, not the continuation predicate");

    assert!(
        error
            .message()
            .contains("θ region result 0 must be the continuation predicate"),
        "{error}"
    );
}

#[test]
fn editing_a_region_leaves_sibling_versions_alone() {
    let mut f = fixture();
    let predicate = f.source(f.i1);
    let carried = f.source(f.i32);

    let (then_region, replacement, user) = forwarding_arm(&mut f);
    let (else_region, _, _) = forwarding_arm(&mut f);

    let gamma = f
        .graph
        .add_node(
            kinds::GAMMA,
            &[Origin::output(predicate, 0), Origin::output(carried, 0)],
            &[PortType::Value(f.i32)],
            &[then_region, else_region],
            Vec::new(),
        )
        .expect("both regions match the signature");
    let sibling = f.source(f.i32);
    let omega = f.graph.finish(&[]).expect("ω takes no exports here");

    mutate::splice(&mut f.graph, user, 0, Origin::output(replacement, 0))
        .expect("the replacement has the same type and is scheduled earlier");

    assert_eq!(f.graph.version(user), 1, "the edited node");
    assert_eq!(f.graph.version(gamma), 1, "the spine up to ω");
    assert_eq!(f.graph.version(omega), 1, "the spine up to ω");
    assert_eq!(f.graph.version(sibling), 0, "an untouched sibling");
    assert_eq!(f.graph.version(carried), 0, "an untouched operand");
    assert!(f.graph.verify().is_ok());
}

#[test]
fn a_rejected_replacement_changes_nothing() {
    let mut f = fixture();
    let old = f.source(f.i32);
    let user = f
        .forward(Origin::output(old, 0), f.i32)
        .expect("the source is scheduled earlier");
    let narrower = f.source(f.i1);
    f.graph.finish(&[]).expect("ω takes no exports here");

    let error = mutate::replace_subtree(&mut f.graph, old, narrower)
        .expect_err("the replacement produces a different output tuple");

    assert!(
        error.message().contains("different output tuple"),
        "{error}"
    );
    assert_eq!(f.graph.inputs(user), [Origin::output(old, 0)]);
    assert_eq!(f.graph.version(user), 0);
    assert_eq!(f.graph.version(narrower), 0);
    assert!(f.graph.verify().is_ok());
}

#[test]
fn replacing_a_subtree_reschedules_it_before_the_users() {
    let mut f = fixture();
    let old = f.source(f.i32);
    let user = f
        .forward(Origin::output(old, 0), f.i32)
        .expect("the source is scheduled earlier");
    let new = f.source(f.i32);
    f.graph.finish(&[]).expect("ω takes no exports here");

    mutate::replace_subtree(&mut f.graph, old, new).expect("the output tuples match");

    assert_eq!(f.graph.inputs(user), [Origin::output(new, 0)]);
    assert_eq!(f.graph.position(new), Some(1));
    assert_eq!(f.graph.position(user), Some(2));
    assert!(f.graph.verify().is_ok());
}
