use tir::graph::{Dag, MutDag};
use tir::sem::{
    ExtendSemBytes, SemBlobBuilder, SemGraph, SemOp, SemPayloadDesc, SymKind, decode_sem_ops,
    int_payload,
};

fn assert_same_graph(actual: &SemGraph, expected: &SemGraph, root: tir::graph::NodeId) {
    for node in expected.postorder(root) {
        assert_eq!(actual.get_node(node), expected.get_node(node));
        assert_eq!(actual.get_leaf_data(node), expected.get_leaf_data(node));
        assert_eq!(
            actual.children(node).collect::<Vec<_>>(),
            expected.children(node).collect::<Vec<_>>()
        );
    }
}

fn add_program() -> Vec<SemOp> {
    vec![
        SemOp::Node(SymKind::Symbol),
        SemOp::Payload(SemPayloadDesc::SymbolId(0)),
        SemOp::Node(SymKind::Constant),
        SemOp::Payload(SemPayloadDesc::Int {
            width: 32,
            value: 7,
            signed: false,
        }),
        SemOp::Node(SymKind::Add),
        SemOp::Edge(2, 0),
        SemOp::Edge(2, 1),
    ]
}

#[test]
fn extend_sem_bytes_builds_the_same_graph_as_imperative_construction() {
    let mut expected = SemGraph::new();
    let a = expected.add_node(SymKind::Symbol);
    expected.set_leaf_data(a, tir::sem::SymPayload::SymbolId(0));
    let b = expected.add_node(SymKind::Constant);
    expected.set_leaf_data(b, int_payload(32, 7, false));
    let add = expected.add_node(SymKind::Add);
    expected.add_edge(add, a);
    expected.add_edge(add, b);

    let mut builder = SemBlobBuilder::new();
    let offset = builder.intern(&add_program());
    let (blob, kinds) = builder.finish();

    let mut decoded = SemGraph::new();
    let root = decoded.extend_sem_bytes(&kinds, &blob, offset);

    assert_eq!(root, add);
    assert_same_graph(&decoded, &expected, root);
}

#[test]
fn every_op_round_trips_through_the_blob() {
    let ops = vec![
        SemOp::Node(SymKind::Constant),
        SemOp::Payload(SemPayloadDesc::Int {
            width: 12,
            value: (-3i64) as u64,
            signed: true,
        }),
        SemOp::Node(SymKind::Constant),
        SemOp::Payload(SemPayloadDesc::Float(-0.5)),
        SemOp::Node(SymKind::Symbol),
        SemOp::Payload(SemPayloadDesc::Value(9)),
        SemOp::Node(SymKind::FAdd),
        SemOp::Typed(64),
        SemOp::Edge(3, 1),
        SemOp::Edge(3, 2),
    ];

    let mut builder = SemBlobBuilder::new();
    let offset = builder.intern(&ops);
    let (blob, kinds) = builder.finish();

    assert_eq!(
        format!("{:?}", decode_sem_ops(&blob, offset, &kinds)),
        format!("{ops:?}")
    );
}

#[test]
fn identical_programs_share_one_record() {
    let mut builder = SemBlobBuilder::new();
    let first = builder.intern(&add_program());
    let second = builder.intern(&add_program());
    let third = builder.intern(&[SemOp::Node(SymKind::Symbol)]);

    assert_eq!(first, second);
    assert_ne!(first, third);
}
