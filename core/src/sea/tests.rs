use crate::attributes::{AttributeValue, NamedAttribute};
use crate::builtin::{IntegerType, UnitType};
use crate::{Context, TypeId};

use super::graph::{Graph, NodeId, Origin, PortType, RegionId};
use super::view::{View, ViewCache};
use super::{commit, kinds, mutate};

struct Fixture {
    context: Context,
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
        context,
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

    fn constant(&mut self, value: i64, ty: TypeId) -> Origin {
        let op = self.graph.op_type("builtin", "constant");
        let node = self
            .graph
            .add_node(
                op,
                &[],
                &[PortType::Value(ty)],
                &[],
                vec![NamedAttribute::new("value", AttributeValue::Int(value))],
            )
            .expect("a constant has no inputs to reject");
        Origin::output(node, 0)
    }

    fn addi(&mut self, lhs: Origin, rhs: Origin, ty: TypeId) -> Origin {
        let op = self.graph.op_type("builtin", "addi");
        let node = self
            .graph
            .add_node(op, &[lhs, rhs], &[PortType::Value(ty)], &[], Vec::new())
            .expect("both operands are scheduled earlier");
        Origin::output(node, 0)
    }

    /// A node on the state chain: it reads `state` after its operands and
    /// produces one value plus the state that follows it.
    fn stateful(
        &mut self,
        name: &'static str,
        operands: &[Origin],
        state: Origin,
        ty: TypeId,
    ) -> (Origin, Origin) {
        let op = self.graph.op_type("test", name);
        let mut inputs = operands.to_vec();
        inputs.push(state);
        let node = self
            .graph
            .add_node(
                op,
                &inputs,
                &[PortType::Value(ty), PortType::State],
                &[],
                Vec::new(),
            )
            .expect("every input is scheduled earlier");
        (Origin::output(node, 0), Origin::output(node, 1))
    }

    /// Seal a λ region with `results` and give it its λ node.
    fn close_lambda(&mut self, region: RegionId, results: &[Origin]) -> NodeId {
        self.graph
            .close_region(region, results)
            .expect("the results are visible in their own region");
        let function = PortType::Value(UnitType::new(&self.context));
        self.graph
            .add_node(kinds::LAMBDA, &[], &[function], &[region], Vec::new())
            .expect("a λ over a closed region with no context variables")
    }
}

/// A λ whose body adds zero to its argument, plus the origins the tests assert
/// on: the argument and the sum.
fn add_zero_lambda(f: &mut Fixture) -> (NodeId, RegionId, Origin, Origin) {
    let i32 = f.i32;
    let body = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let argument = Origin::argument(body, 0);
    let zero = f.constant(0, i32);
    let sum = f.addi(argument, zero, i32);
    let lambda = f.close_lambda(body, &[sum, Origin::argument(body, 1)]);
    (lambda, body, argument, sum)
}

/// A λ that forwards one of two constants, so a test has something to splice.
fn splicable_lambda(f: &mut Fixture) -> (NodeId, NodeId, Origin) {
    let i32 = f.i32;
    let body = f.graph.open_region(&[PortType::State]);
    let first = f.constant(1, i32);
    let second = f.constant(2, i32);
    let user = f.forward(first, i32).expect("the constant is earlier");
    let lambda = f.close_lambda(body, &[Origin::argument(body, 0)]);
    (lambda, user, second)
}

#[test]
fn the_view_proves_an_addition_of_zero_is_its_operand() {
    let mut f = fixture();
    let (lambda, body, argument, sum) = add_zero_lambda(&mut f);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut view = View::build(&f.context, &f.graph, body, None);
    assert_ne!(view.class(sum), view.class(argument), "before saturation");

    view.saturate(&f.context);

    assert_eq!(view.class(sum), view.class(argument));
}

#[test]
fn a_stateful_node_anchors_the_values_around_it() {
    let mut f = fixture();
    let i32 = f.i32;
    let body = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let address = Origin::argument(body, 0);
    let state = Origin::argument(body, 1);
    let (first, state) = f.stateful("load", &[address], state, i32);
    let (_, state) = f.stateful("store", &[address, first], state, i32);
    let (second, state) = f.stateful("load", &[address], state, i32);
    let lambda = f.close_lambda(body, &[second, state]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut view = View::build(&f.context, &f.graph, body, None);
    view.saturate(&f.context);

    assert_ne!(
        view.class(first),
        view.class(second),
        "two loads of one address split by a store are not the same value"
    );
}

#[test]
fn a_cached_view_survives_an_edit_to_a_sibling_lambda() {
    let mut f = fixture();
    let (first, body, argument, sum) = add_zero_lambda(&mut f);
    let (second, user, replacement) = splicable_lambda(&mut f);
    f.graph
        .finish(&[Origin::output(first, 0), Origin::output(second, 0)])
        .expect("both λs are exported");

    let mut cache = ViewCache::default();
    cache
        .view(&f.context, &f.graph, body, None)
        .saturate(&f.context);
    mutate::splice(&mut f.graph, user, 0, replacement).expect("same type, scheduled earlier");

    let view = cache.view(&f.context, &f.graph, body, None);
    assert_eq!(
        view.class(sum),
        view.class(argument),
        "the saturation survived, so the view was not rebuilt"
    );
}

#[test]
fn a_cached_view_is_rebuilt_after_an_edit_to_its_own_lambda() {
    let mut f = fixture();
    let i32 = f.i32;
    let body = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let argument = Origin::argument(body, 0);
    let zero = f.constant(0, i32);
    let one = f.constant(1, i32);
    let sum = f.addi(argument, zero, i32);
    let user = sum.node().expect("the sum is a node output");
    let lambda = f.close_lambda(body, &[sum, Origin::argument(body, 1)]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut cache = ViewCache::default();
    cache
        .view(&f.context, &f.graph, body, None)
        .saturate(&f.context);
    mutate::splice(&mut f.graph, user, 1, one).expect("same type, scheduled earlier");

    let view = cache.view(&f.context, &f.graph, body, None);
    assert_ne!(
        view.class(sum),
        view.class(argument),
        "the edit reached this region, so the view was rebuilt unsaturated"
    );
}

/// Two allocas nobody's pointer escapes, a store into the first and a load out
/// of the second: the load must not read the state the store produced, or the
/// two disjoint slots would be ordered against each other.
#[test]
fn disjoint_allocas_get_independent_state_chains() {
    let context = Context::with_default_dialects();
    let i32 = IntegerType::new(&context, 32);
    let slot_type = crate::ptr::PtrType::typed(&context, i32);
    let parameter = context.create_value(i32, None);
    let stored = parameter.id();
    let region = context.create_region();
    let block = context.create_block(vec![parameter]);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(&context, "f", i32, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let first = builder.insert(crate::ptr::ops::alloca(&context, 4u64, 4u64, slot_type).build());
    let second = builder.insert(crate::ptr::ops::alloca(&context, 4u64, 4u64, slot_type).build());
    builder.insert(crate::ptr::ops::store(&context, stored, first.result()).build());
    let load = builder.insert(crate::ptr::ops::load(&context, second.result(), i32).build());
    builder.insert(crate::builtin::ops::r#return(&context, load.result()).build());

    let raised = super::raise_function(&context, &func).expect("straight-line code raises");
    assert_eq!(
        raised.chains, 3,
        "two tracked slots plus the conservative one"
    );

    let body = raised.graph.subregions(raised.lambda)[0];
    let node_of = |name: &str, position: usize| {
        raised
            .graph
            .region_nodes(body)
            .iter()
            .copied()
            .filter(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == name)
            .nth(position)
            .expect("the raised node")
    };
    let (store_node, load_node) = (node_of("store", 0), node_of("load", 0));
    let load_state = *raised
        .graph
        .inputs(load_node)
        .last()
        .expect("a load reads a state chain");

    assert_ne!(
        load_state.node(),
        Some(store_node),
        "the load of one slot must not read the state the store to the other produced"
    );
    assert!(raised.graph.verify().is_ok());
}

/// A constant carries no type in the e-graph — a null pointer and an integer
/// zero of the same width are one class — so a commit must build one per type it
/// is read at, never hand a pointer to an integer port.
#[test]
fn a_constant_class_is_built_once_per_type_it_is_read_at() {
    let mut f = fixture();
    let context = f.context.clone();
    let i64 = IntegerType::new(&context, 64);
    let pointer = crate::ptr::PtrType::typed(&context, i64);

    let body = f
        .graph
        .open_region(&[PortType::Value(i64), PortType::State]);
    let null = f.constant(0, pointer);
    let (_, state) = f.stateful("keep", &[null], Origin::argument(body, 1), i64);
    let zero = f.constant(0, i64);
    let op = f.graph.op_type("builtin", "muli");
    let product = f
        .graph
        .add_node(
            op,
            &[Origin::argument(body, 0), zero],
            &[PortType::Value(i64)],
            &[],
            Vec::new(),
        )
        .expect("both operands are scheduled earlier");
    let lambda = f.close_lambda(body, &[Origin::output(product, 0), state]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut view = View::build(&context, &f.graph, body, None);
    view.saturate(&context);
    let replacement = commit::commit(&f.context, &mut f.graph, &view, lambda)
        .expect("the staged body is well formed")
        .expect("multiplying by zero is the zero constant");

    let staged = f.graph.subregions(replacement)[0];
    assert_eq!(
        f.graph.origin_type(f.graph.region_results(staged)[0]),
        PortType::Value(i64),
        "the integer result must not be given the pointer-typed null"
    );
    assert!(f.graph.verify().is_ok());
}

/// The Stager rebuilds op identities, constants and the origins gates and
/// leaves stand for. Semantic seeding puts terms of the *semantic* vocabulary on
/// those same classes, and none of them is anything a region can hold, so an
/// extraction must keep choosing the op identity.
#[test]
fn a_commit_rebuilds_the_op_a_semantic_term_shares_a_class_with() {
    let mut f = fixture();
    let (i32, i64) = (f.i32, IntegerType::new(&f.context, 64));

    let body = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let extsi = f.graph.op_type("builtin", "extsi");
    let extended = f
        .graph
        .add_node(
            extsi,
            &[Origin::argument(body, 0)],
            &[PortType::Value(i64)],
            &[],
            Vec::new(),
        )
        .expect("the argument is scheduled earlier");
    let zero = f.constant(0, i64);
    let sum = f.addi(Origin::output(extended, 0), zero, i64);
    let lambda = f.close_lambda(body, &[sum, Origin::argument(body, 1)]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut view = View::build(&f.context, &f.graph, body, None);
    view.saturate(&f.context);
    let replacement = commit::commit(&f.context, &mut f.graph, &view, lambda)
        .expect("the staged body is well formed")
        .expect("the extraction dropped the addition");

    let staged = f.graph.subregions(replacement)[0];
    let result = f.graph.region_results(staged)[0];
    let node = result.node().expect("the extension is a node output");
    assert_eq!(
        f.graph.op_type_name(f.graph.op_of(node)).1,
        "extsi",
        "the sign extension must be rebuilt as the op, not as its semantics"
    );
    assert!(f.graph.verify().is_ok());
}

#[test]
fn a_commit_replaces_the_lambda_it_canonicalized_and_nothing_else() {
    let mut f = fixture();
    let (first, body, _, _) = add_zero_lambda(&mut f);
    let (second, _, _) = splicable_lambda(&mut f);
    let omega = f.graph.omega_region();
    f.graph
        .finish(&[Origin::output(first, 0), Origin::output(second, 0)])
        .expect("both λs are exported");

    let mut view = View::build(&f.context, &f.graph, body, None);
    view.saturate(&f.context);
    let replacement = commit::commit(&f.context, &mut f.graph, &view, first)
        .expect("the staged body is well formed")
        .expect("the extraction dropped the addition");

    assert_eq!(
        f.graph.region_results(omega)[0],
        Origin::output(replacement, 0)
    );
    assert_eq!(
        f.graph.region_results(omega)[1],
        Origin::output(second, 0),
        "the sibling export still names the λ it always did"
    );
    assert_eq!(f.graph.version(second), 0, "the untouched sibling λ");
    assert!(f.graph.verify().is_ok());

    let staged = f.graph.subregions(replacement)[0];
    assert_eq!(
        f.graph.region_results(staged)[0],
        Origin::argument(staged, 0),
        "the body yields its argument directly"
    );
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

/// A λ holding a θ that carries one i32, incrementing it every iteration. Reports
/// the λ body, the θ node and its body region.
fn counting_loop(f: &mut Fixture) -> (RegionId, NodeId, RegionId) {
    let (i1, i32) = (f.i1, f.i32);
    let outer = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let init = Origin::argument(outer, 0);

    let body = f.graph.open_region(&[PortType::Value(i32)]);
    let one = f.constant(1, i32);
    let next = f.addi(Origin::argument(body, 0), one, i32);
    let keep_going = f.constant(1, i1);
    f.graph
        .close_region(body, &[keep_going, next])
        .expect("the predicate and the latched value are visible");

    let theta = f
        .graph
        .add_node(
            kinds::THETA,
            &[init],
            &[PortType::Value(i32)],
            &[body],
            Vec::new(),
        )
        .expect("the θ signature matches its region");
    let lambda = f.close_lambda(
        outer,
        &[Origin::output(theta, 0), Origin::argument(outer, 1)],
    );
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");
    (outer, theta, body)
}

#[test]
fn a_theta_output_is_never_the_value_the_loop_carries() {
    let mut f = fixture();
    let (outer, theta, body) = counting_loop(&mut f);

    let mut view = View::build(&f.context, &f.graph, outer, None);
    view.saturate(&f.context);

    let output = view.class(Origin::output(theta, 0));
    let carried = view.class(Origin::argument(body, 0));
    assert!(output.is_some() && carried.is_some());
    assert_ne!(
        output, carried,
        "the value a loop leaves behind is not the value its body reads"
    );
}

/// A γ whose first output is computed by a pure slice and whose second comes off
/// the state chain. Reports the λ body and the γ node.
fn half_pure_gamma(f: &mut Fixture) -> (RegionId, NodeId) {
    let (i1, i32) = (f.i1, f.i32);
    let outer = f
        .graph
        .open_region(&[PortType::Value(i32), PortType::State]);
    let address = Origin::argument(outer, 0);
    let state = Origin::argument(outer, 1);
    let predicate = f.constant(1, i1);

    let arm = |f: &mut Fixture| {
        let region = f
            .graph
            .open_region(&[PortType::Value(i32), PortType::State]);
        let argument = Origin::argument(region, 0);
        let doubled = f.addi(argument, argument, i32);
        let (loaded, next) = f.stateful("load", &[argument], Origin::argument(region, 1), i32);
        f.graph
            .close_region(region, &[doubled, loaded, next])
            .expect("every result is visible in the arm");
        region
    };
    let otherwise = arm(f);
    let taken = arm(f);

    let gamma = f
        .graph
        .add_node(
            kinds::GAMMA,
            &[predicate, address, state],
            &[PortType::Value(i32), PortType::Value(i32), PortType::State],
            &[otherwise, taken],
            Vec::new(),
        )
        .expect("both arms match the γ signature");
    let lambda = f.close_lambda(outer, &[Origin::output(gamma, 0), Origin::output(gamma, 2)]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");
    (outer, gamma)
}

#[test]
fn a_gamma_gates_the_ports_whose_arms_are_pure_and_anchors_the_rest() {
    let mut f = fixture();
    let (outer, gamma) = half_pure_gamma(&mut f);

    let view = View::build(&f.context, &f.graph, outer, None);

    assert!(
        view.models(Origin::output(gamma, 0)),
        "a port both arms compute purely is a term"
    );
    assert!(
        !view.models(Origin::output(gamma, 1)),
        "a port an arm computes off the state chain stays an anchor"
    );
}

/// `ptr.null` is the all-zero address at the pointer width the data layout in
/// scope declares, so the view seeds its semantics only where it knows one.
#[test]
fn a_null_pointer_seeds_its_semantics_from_the_layout_in_scope() {
    let mut f = fixture();
    let pointer = crate::ptr::PtrType::opaque(&f.context);
    let body = f.graph.open_region(&[]);
    let null = f.graph.op_type("ptr", "null");
    let node = f
        .graph
        .add_node(null, &[], &[PortType::Value(pointer)], &[], Vec::new())
        .expect("a null has no inputs to reject");
    let lambda = f.close_lambda(body, &[Origin::output(node, 0)]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let AttributeValue::Dict(layout) =
        crate::data_layout_spec(crate::Endianness::Little, 128, &[("p", 64, 64)])
    else {
        unreachable!("a layout spec is a dict")
    };
    let view = View::build(&f.context, &f.graph, body, Some(&layout));

    let class = view
        .class(Origin::output(node, 0))
        .expect("a null pointer is a term");
    assert!(
        view.egraph().nodes(class).iter().any(|node| {
            node.int()
                .is_some_and(|address| address.width() == 64 && address.to_u64() == 0)
        }),
        "the null address must be seeded at the layout's pointer width"
    );
}

/// The placeholder standing for a loop-carried value is a leaf like any other,
/// so an extraction reaching a nested class through it must resolve the origin
/// it stands for: the body argument the loop variable arrives on.
#[test]
fn a_loop_carried_placeholder_resolves_to_the_body_argument() {
    let mut f = fixture();
    let (outer, _, body) = counting_loop(&mut f);

    let view = View::build(&f.context, &f.graph, outer, None);

    let carried = Origin::argument(body, 0);
    let class = view.class(carried).expect("the carried value is a term");
    let placeholder = view
        .candidates(class)
        .find_map(|node| node.value())
        .expect("the class holds the placeholder leaf");
    assert_eq!(view.anchor(placeholder), Some(carried));
}

/// Extraction names one winner per class; a covering pass reads every form the
/// class holds, at whatever id it asks with.
#[test]
fn a_class_reports_its_canonical_members() {
    use crate::sem::SymKind;

    let mut f = fixture();
    let i32 = f.i32;
    let body = f.graph.open_region(&[PortType::Value(i32)]);
    let zero = f.constant(0, i32);
    let muli = f.graph.op_type("builtin", "muli");
    let product = f
        .graph
        .add_node(
            muli,
            &[Origin::argument(body, 0), zero],
            &[PortType::Value(i32)],
            &[],
            Vec::new(),
        )
        .expect("both operands are visible in the region");
    let product = Origin::output(product, 0);
    let lambda = f.close_lambda(body, &[product]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let mut view = View::build(&f.context, &f.graph, body, None);
    let seeded = view.class(product).expect("the product is a term");
    view.saturate(&f.context);

    let class = view.class(product).expect("the product is still a term");
    let members: Vec<_> = view.candidates(seeded).collect();
    assert_eq!(
        members.len(),
        view.egraph().nodes(class).len(),
        "an id from before the folding union reads the class it landed in"
    );
    assert!(
        members
            .iter()
            .any(|node| node.kind.ir().is_some_and(|op| op.name == "muli")),
        "the op identity the peephole rules match"
    );
    assert!(
        members.iter().any(|node| node.sym() == Some(SymKind::Mul)),
        "the semantic form the selection axioms match"
    );
    assert!(
        members
            .iter()
            .any(|node| node.int().is_some_and(|value| value.to_u64() == 0)),
        "the constant the peephole proved it equal to"
    );
}

/// The null address is the literal zero in the semantic vocabulary, but
/// `builtin.constant` holds an integer — so a class the region reads as a
/// pointer is rebuilt from the operation that names it, never from the literal.
#[test]
fn a_pointer_class_is_never_rebuilt_from_the_literal_address() {
    let mut f = fixture();
    let i32 = f.i32;
    let pointer = crate::ptr::PtrType::opaque(&f.context);
    let body = f.graph.open_region(&[PortType::Value(i32)]);
    let null = f.graph.op_type("ptr", "null");
    let address = f
        .graph
        .add_node(null, &[], &[PortType::Value(pointer)], &[], Vec::new())
        .expect("a null has no inputs to reject");
    let zero = f.constant(0, i32);
    let sum = f.addi(Origin::argument(body, 0), zero, i32);
    let lambda = f.close_lambda(body, &[Origin::output(address, 0), sum]);
    f.graph
        .finish(&[Origin::output(lambda, 0)])
        .expect("the λ is the only export");

    let AttributeValue::Dict(layout) =
        crate::data_layout_spec(crate::Endianness::Little, 128, &[("p", 64, 64)])
    else {
        unreachable!("a layout spec is a dict")
    };
    let mut view = View::build(&f.context, &f.graph, body, Some(&layout));
    view.saturate(&f.context);
    let replacement = commit::commit(&f.context, &mut f.graph, &view, lambda)
        .expect("the staged body is well formed")
        .expect("adding zero is an improvement worth committing");

    let staged = f.graph.subregions(replacement)[0];
    let spelled: Vec<_> = f
        .graph
        .region_nodes(staged)
        .iter()
        .map(|&node| f.graph.op_type_name(f.graph.op_of(node)))
        .collect();
    assert!(
        spelled.contains(&("ptr", "null")),
        "the null address must survive as its operation, got {spelled:?}"
    );
}

/// A function with one non-escaping slot: a store into it, then a load out of
/// it. Reports the raised graph, its body region, and the store and load nodes.
fn stored_then_loaded_slot(context: &Context) -> (super::raise::Raised, RegionId, NodeId, NodeId) {
    let i32 = IntegerType::new(context, 32);
    let slot_type = crate::ptr::PtrType::typed(context, i32);
    let parameter = context.create_value(i32, None);
    let stored = parameter.id();
    let region = context.create_region();
    let block = context.create_block(vec![parameter]);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(context, "f", i32, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let slot = builder.insert(crate::ptr::ops::alloca(context, 4u64, 4u64, slot_type).build());
    builder.insert(crate::ptr::ops::store(context, stored, slot.result()).build());
    let load = builder.insert(crate::ptr::ops::load(context, slot.result(), i32).build());
    builder.insert(crate::builtin::ops::r#return(context, load.result()).build());

    let raised = super::raise_function(context, &func).expect("straight-line code raises");
    let body = raised.graph.subregions(raised.lambda)[0];
    let node_of = |name: &str| {
        raised
            .graph
            .region_nodes(body)
            .iter()
            .copied()
            .find(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == name)
            .expect("the raised node")
    };
    let (store, load) = (node_of("store"), node_of("load"));
    (raised, body, store, load)
}

/// Memory is a term over the state it reads: the load's class carries the
/// `LoadMemory` shape, and its state operand is the class the store produced.
/// That operand is the whole of memory identity on the view path — two loads
/// merge only where they agree on it.
#[test]
fn a_load_seeds_the_state_it_reads_as_its_operand() {
    let context = Context::with_default_dialects();
    let (raised, body, store, load) = stored_then_loaded_slot(&context);

    let view = View::build(&context, &raised.graph, body, None);
    let loaded = view
        .class(Origin::output(load, 0))
        .expect("a load is a term over the state it reads");
    let stored_state = view
        .class(Origin::output(store, 0))
        .expect("a store's result is the state that follows it");

    let states: Vec<_> = view
        .egraph()
        .nodes(loaded)
        .iter()
        .filter(|node| node.kind == crate::sem::SymKind::LoadMemory)
        .map(|node| {
            view.egraph()
                .find(*node.children.last().expect("a state operand"))
        })
        .collect();
    assert_eq!(
        states,
        vec![stored_state],
        "the load must read the state the store produced"
    );
}

/// S1, load-over-store forwarding: a load whose chain is a store to the same
/// address at the same width is the value that store put there.
#[test]
fn a_load_reads_back_what_the_store_on_its_chain_put_there() {
    let context = Context::with_default_dialects();
    let (raised, body, store, load) = stored_then_loaded_slot(&context);

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);

    let stored = view
        .class(raised.graph.inputs(store)[0])
        .expect("the stored value is a term");
    assert_eq!(
        view.class(Origin::output(load, 0)),
        Some(stored),
        "the load must be the value the store put at that address"
    );
}

/// S3, disjoint-chain commutation: separate chains never interact, so no law
/// states it. A store to one non-escaping slot is not on the chain a load of
/// another reads, and forwarding reaches straight through it.
#[test]
fn a_store_to_a_disjoint_slot_does_not_block_forwarding() {
    let context = Context::with_default_dialects();
    let i32 = IntegerType::new(&context, 32);
    let slot_type = crate::ptr::PtrType::typed(&context, i32);
    let first_value = context.create_value(i32, None);
    let second_value = context.create_value(i32, None);
    let (stored, other) = (first_value.id(), second_value.id());
    let region = context.create_region();
    let block = context.create_block(vec![first_value, second_value]);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(&context, "f", i32, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let mine = builder.insert(crate::ptr::ops::alloca(&context, 4u64, 4u64, slot_type).build());
    let theirs = builder.insert(crate::ptr::ops::alloca(&context, 4u64, 4u64, slot_type).build());
    builder.insert(crate::ptr::ops::store(&context, stored, mine.result()).build());
    builder.insert(crate::ptr::ops::store(&context, other, theirs.result()).build());
    let load = builder.insert(crate::ptr::ops::load(&context, mine.result(), i32).build());
    builder.insert(crate::builtin::ops::r#return(&context, load.result()).build());

    let raised = super::raise_function(&context, &func).expect("straight-line code raises");
    let body = raised.graph.subregions(raised.lambda)[0];
    let node_of = |name: &str, position: usize| {
        raised
            .graph
            .region_nodes(body)
            .iter()
            .copied()
            .filter(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == name)
            .nth(position)
            .expect("the raised node")
    };
    let (store, load) = (node_of("store", 0), node_of("load", 0));

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);

    let mine = view
        .class(raised.graph.inputs(store)[0])
        .expect("the stored value is a term");
    assert_eq!(
        view.class(Origin::output(load, 0)),
        Some(mine),
        "a write to another slot's chain is not on this load's chain at all"
    );
}

/// S2, dead-store elimination: a write nothing reads before the next write to
/// the same extent leaves the chain where it found it.
#[test]
fn a_store_the_next_one_overwrites_leaves_the_chain_alone() {
    let context = Context::with_default_dialects();
    let i32 = IntegerType::new(&context, 32);
    let slot_type = crate::ptr::PtrType::typed(&context, i32);
    let first_value = context.create_value(i32, None);
    let second_value = context.create_value(i32, None);
    let (dead, live) = (first_value.id(), second_value.id());
    let region = context.create_region();
    let block = context.create_block(vec![first_value, second_value]);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(&context, "f", i32, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let slot = builder.insert(crate::ptr::ops::alloca(&context, 4u64, 4u64, slot_type).build());
    builder.insert(crate::ptr::ops::store(&context, dead, slot.result()).build());
    builder.insert(crate::ptr::ops::store(&context, live, slot.result()).build());
    let load = builder.insert(crate::ptr::ops::load(&context, slot.result(), i32).build());
    builder.insert(crate::builtin::ops::r#return(&context, load.result()).build());

    let raised = super::raise_function(&context, &func).expect("straight-line code raises");
    let body = raised.graph.subregions(raised.lambda)[0];
    let store_of = |position: usize| {
        raised
            .graph
            .region_nodes(body)
            .iter()
            .copied()
            .filter(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == "store")
            .nth(position)
            .expect("the raised store")
    };
    let overwritten = store_of(0);

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);

    assert_eq!(
        view.class(Origin::output(overwritten, 0)),
        view.class(*raised.graph.inputs(overwritten).last().expect("a chain")),
        "the overwritten store must leave the chain as it found it"
    );
}

/// A forwarded load has no operation to rebuild: the commit lands the value the
/// store put there and the load is gone, while the store — whose chain the
/// region exports — stays where the region held it.
#[test]
fn a_commit_drops_a_load_the_store_before_it_forwarded() {
    let context = Context::with_default_dialects();
    let (mut raised, body, _, _) = stored_then_loaded_slot(&context);

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);
    let replacement = commit::commit(&context, &mut raised.graph, &view, raised.lambda)
        .expect("the staged body is well formed")
        .expect("the extraction forwarded the load");

    let staged = raised.graph.subregions(replacement)[0];
    let names: Vec<_> = raised
        .graph
        .region_nodes(staged)
        .iter()
        .map(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1)
        .collect();
    assert!(
        !names.contains(&"load"),
        "the forwarded load has no operation to rebuild, got {names:?}"
    );
    assert!(
        names.contains(&"store"),
        "the write the region's chain exports must stay, got {names:?}"
    );
    let exported = *raised
        .graph
        .region_results(staged)
        .last()
        .expect("the region exports its chain");
    assert_eq!(
        raised.graph.origin_type(exported),
        PortType::State,
        "the chain the region exports must still be a chain"
    );
    assert!(raised.graph.verify().is_ok());
}

/// A function over two pointer parameters, so every access shares the
/// conservative chain. `accesses` names, in order, the access to build: a store
/// of the first or second value into the like-numbered pointer, or a load out of
/// one. Reports the raised graph, its body and the staged nodes' names.
fn conservative_chain(accesses: &[(&str, usize)]) -> (Context, super::raise::Raised, RegionId) {
    let context = Context::with_default_dialects();
    let i32 = IntegerType::new(&context, 32);
    let pointer = crate::ptr::PtrType::typed(&context, i32);
    let arguments: Vec<_> = [pointer, pointer, i32, i32]
        .into_iter()
        .map(|ty| context.create_value(ty, None))
        .collect();
    let values: Vec<_> = arguments.iter().map(|value| value.id()).collect();
    let region = context.create_region();
    let block = context.create_block(arguments);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(&context, "f", i32, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let mut loaded = None;
    for &(access, slot) in accesses {
        match access {
            "store" => {
                builder.insert(
                    crate::ptr::ops::store(&context, values[2 + slot], values[slot]).build(),
                );
            }
            _ => {
                loaded = Some(
                    builder
                        .insert(crate::ptr::ops::load(&context, values[slot], i32).build())
                        .result(),
                )
            }
        }
    }
    let returned = loaded.unwrap_or(values[2]);
    builder.insert(crate::builtin::ops::r#return(&context, returned).build());

    let raised = super::raise_function(&context, &func).expect("straight-line code raises");
    let body = raised.graph.subregions(raised.lambda)[0];
    (context, raised, body)
}

fn staged_names(graph: &Graph, region: RegionId) -> Vec<&'static str> {
    graph
        .region_nodes(region)
        .iter()
        .map(|&node| graph.op_type_name(graph.op_of(node)).1)
        .collect()
}

/// A read leaves the chain it read, so nothing in the state DAG orders it
/// against the write that follows. The schedule a commit lands is the region's
/// own node order, and it must keep the load between the two writes.
#[test]
fn a_commit_keeps_a_load_between_the_writes_it_sat_between() {
    let (context, mut raised, body) =
        conservative_chain(&[("store", 0), ("load", 1), ("store", 0)]);

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);
    let staged = match commit::commit(&context, &mut raised.graph, &view, raised.lambda)
        .expect("the staged body is well formed")
    {
        Some(replacement) => raised.graph.subregions(replacement)[0],
        None => body,
    };

    let accesses: Vec<_> = staged_names(&raised.graph, staged)
        .into_iter()
        .filter(|name| matches!(*name, "load" | "store"))
        .collect();
    assert_eq!(accesses, vec!["store", "load", "store"]);
    assert!(raised.graph.verify().is_ok());
}

/// Forwarding reads the chain, and a write to another address is on it: the
/// load's state is that write, not the one it would read back from, so no law
/// fires and the access stays.
#[test]
fn a_load_does_not_forward_past_a_write_on_its_own_chain() {
    let (context, raised, body) = conservative_chain(&[("store", 0), ("store", 1), ("load", 0)]);

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);

    let load = raised
        .graph
        .region_nodes(body)
        .iter()
        .copied()
        .find(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == "load")
        .expect("the raised load");
    let stored = view
        .class(raised.graph.inputs(load)[0])
        .expect("the address is a term");
    assert_ne!(
        view.class(Origin::output(load, 0)),
        Some(stored),
        "an intervening write to another address blocks forwarding"
    );
}

/// The vocabulary is bit-level: a byte count alone would let a slot written as
/// an integer be read back as the float a consumer spells it, which is a value
/// no region can hold. Forwarding is for the same access, types included.
#[test]
fn a_load_does_not_forward_a_store_of_another_type() {
    let context = Context::with_default_dialects();
    let i64 = IntegerType::new(&context, 64);
    let f64 = crate::builtin::FloatType::new(&context, 11, 52);
    let slot_type = crate::ptr::PtrType::opaque(&context);
    let parameter = context.create_value(i64, None);
    let stored = parameter.id();
    let region = context.create_region();
    let block = context.create_block(vec![parameter]);
    region.add_block(block.id());

    let func = crate::builtin::ops::func(&context, "f", f64, Some(region.id())).build();
    let mut builder = crate::IRBuilder::new(func.body());
    let slot = builder.insert(crate::ptr::ops::alloca(&context, 8u64, 8u64, slot_type).build());
    builder.insert(crate::ptr::ops::store(&context, stored, slot.result()).build());
    let load = builder.insert(crate::ptr::ops::load(&context, slot.result(), f64).build());
    builder.insert(crate::builtin::ops::r#return(&context, load.result()).build());

    let raised = super::raise_function(&context, &func).expect("straight-line code raises");
    let body = raised.graph.subregions(raised.lambda)[0];
    let load = raised
        .graph
        .region_nodes(body)
        .iter()
        .copied()
        .find(|&node| raised.graph.op_type_name(raised.graph.op_of(node)).1 == "load")
        .expect("the raised load");

    let mut view = View::build(&context, &raised.graph, body, None);
    view.saturate(&context);

    let loaded = view
        .class(Origin::output(load, 0))
        .expect("a load is a term");
    let bits = view
        .class(Origin::argument(body, 0))
        .expect("the stored value is a term");
    assert_ne!(
        loaded, bits,
        "a float read of an integer write is a reinterpretation, not the value"
    );
}
