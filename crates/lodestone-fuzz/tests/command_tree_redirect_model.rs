//! Deterministic, shrinkable model check for command-tree redirect expansion.
//!
//! `CommandTree::effective_children` is the client chat completion consumer's
//! same-token redirect walk. The generated graphs are intentionally only
//! bounded integer adjacency lists; the expected traversal does not construct
//! a production tree or call a production helper. A fixed seed makes a failed
//! graph reproducible, while vector/list shrinking removes redirects and
//! children until the smallest still-wrong graph remains. The detector control
//! omits redirects and must fail, proving this comparison observes the
//! production redirect seam rather than accepting every graph.

use std::cell::Cell;

use lodestone_model::command_tree::{CommandTree, NodeKind, RawCommandNode};
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 192;
const NODES: usize = 8;
const SEED: u64 = 0x43_4f_4d_4d_41_4e_44_53;

#[derive(Clone, Debug)]
struct NodeSpec {
    children: Vec<u8>,
    redirect: Option<u8>,
}

#[derive(Clone, Debug)]
struct Graph {
    nodes: Vec<NodeSpec>,
    start: usize,
}

fn node_strategy() -> impl Strategy<Value = NodeSpec> {
    (
        collection::vec(0_u8..NODES as u8, 0..5),
        prop::option::of(0_u8..NODES as u8),
    )
        .prop_map(|(children, redirect)| NodeSpec { children, redirect })
}

fn graph_strategy() -> impl Strategy<Value = Graph> {
    (collection::vec(node_strategy(), NODES), 0_usize..NODES)
        .prop_map(|(nodes, start)| Graph { nodes, start })
}

/// A graph class where ignoring redirects is necessarily wrong: node zero
/// redirects to node one, whose non-empty child list must be included.
fn detector_graph_strategy() -> impl Strategy<Value = Graph> {
    (
        collection::vec(0_u8..NODES as u8, 0..5),
        collection::vec(0_u8..NODES as u8, 1..5),
        collection::vec(node_strategy(), NODES - 2),
    )
        .prop_map(|(first_children, redirected_children, mut rest)| {
            let mut nodes = vec![
                NodeSpec {
                    children: first_children,
                    redirect: Some(1),
                },
                NodeSpec {
                    children: redirected_children,
                    redirect: None,
                },
            ];
            nodes.append(&mut rest);
            Graph { nodes, start: 0 }
        })
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    })
}

fn production_children(graph: &Graph) -> Vec<usize> {
    let nodes = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| RawCommandNode {
            kind: NodeKind::Literal {
                name: format!("node_{index}"),
            },
            executable: false,
            restricted: false,
            redirect: node.redirect.map(usize::from),
            children: node.children.iter().copied().map(usize::from).collect(),
        })
        .collect();
    let tree = CommandTree::new(nodes, 0).expect("generated graph indices are bounded");
    tree.effective_children(graph.start)
}

/// Independent graph walk: every same-token redirect contributes children in
/// order, while revisiting a node terminates a redirect cycle.
fn expected_children(graph: &Graph) -> Vec<usize> {
    let mut visited = [false; NODES];
    let mut output = Vec::new();
    let mut current = Some(graph.start);

    while let Some(index) = current {
        if visited[index] {
            break;
        }
        visited[index] = true;
        let node = &graph.nodes[index];
        output.extend(node.children.iter().copied().map(usize::from));
        current = node.redirect.map(usize::from);
    }

    output
}

fn own_children_only(graph: &Graph) -> Vec<usize> {
    graph.nodes[graph.start]
        .children
        .iter()
        .copied()
        .map(usize::from)
        .collect()
}

fn graph(nodes: Vec<(Vec<u8>, Option<u8>)>) -> Graph {
    Graph {
        nodes: nodes
            .into_iter()
            .map(|(children, redirect)| NodeSpec { children, redirect })
            .collect(),
        start: 0,
    }
}

#[test]
fn literal_redirect_graph_corpus_matches_the_independent_walk() {
    let corpus = [
        graph(vec![
            (vec![1, 2], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
        ]),
        graph(vec![
            (vec![2], Some(1)),
            (vec![3, 4], Some(5)),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![6], None),
            (vec![], None),
            (vec![], None),
        ]),
        graph(vec![
            (vec![2], Some(1)),
            (vec![3], Some(0)),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
            (vec![], None),
        ]),
    ];

    for graph in corpus {
        assert_eq!(production_children(&graph), expected_children(&graph));
    }
}

#[test]
fn generated_command_redirects_match_the_independent_walk() {
    runner(CASES)
        .run(&graph_strategy(), |graph| {
            prop_assert_eq!(
                production_children(&graph),
                expected_children(&graph),
                "generated graph: {graph:?}",
            );
            Ok(())
        })
        .expect("production redirect traversal must match the independent walk");
}

#[test]
fn redirect_omission_detector_is_rejected_and_shrunk() {
    let evaluations = Cell::new(0usize);
    let failure = runner(CASES)
        .run(&detector_graph_strategy(), |graph| {
            evaluations.set(evaluations.get() + 1);
            prop_assert_eq!(
                own_children_only(&graph),
                expected_children(&graph),
                "redirect omission detector control: {graph:?}",
            );
            Ok(())
        })
        .expect_err("a traversal that omits redirects must be rejected");

    match failure {
        TestError::Fail(_, minimal) => {
            assert_eq!(minimal.start, 0);
            assert_eq!(minimal.nodes[0].redirect, Some(1));
            assert!(
                !minimal.nodes[1].children.is_empty(),
                "the shrunk witness must retain a redirected child"
            );
            assert_ne!(own_children_only(&minimal), expected_children(&minimal));
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed detector control must evaluate shrink candidates"
    );
}
