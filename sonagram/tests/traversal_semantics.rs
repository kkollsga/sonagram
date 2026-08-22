//! Value-pin gate for var-length trail semantics over a cyclic `SIMILAR_TO`
//! graph.
//!
//! The agent guide's similarity recipe chains `-[:SIMILAR_TO*1..2]->` to reach
//! beyond the top-10 horizon, and `SIMILAR_TO` is a directed top-k
//! nearest-neighbour graph — mutual pairs and longer cycles are the norm, not
//! the exception. kglite 0.16.6 fixed a BREAKING semantics bug in exactly this
//! query family: the optimized var-length path answered *distance*
//! reachability instead of Cypher's *trail* reachability whenever the clause's
//! consumer collapsed row multiplicity (`DISTINCT`, an aggregate). On a cyclic
//! graph the two relations differ, so the fix changes query ANSWERS — and the
//! digest/golden gates are structurally blind to it, because both sides of any
//! build-A-vs-build-B comparison move together.
//!
//! This test pins HAND-DERIVED answer sets over a minimal cycle, computed from
//! trail semantics (edge-distinct paths) on paper, not from the engine.
//! Verified discriminating at introduction: RED against kglite 0.16.5
//! (crates.io, sibling patch disabled) and GREEN against 0.16.6 — the red run
//! is the proof this gate can fail. If it goes red on a future kglite bump,
//! the engine changed what these queries *answer*; that is upstream news to
//! route, never a reason to re-derive the pins from the new engine's output.

use std::collections::HashMap;

use kglite::api::session::{execute_mut, execute_read, ExecuteOptions};
use kglite::api::{DirGraph, Value};

/// A directed 3-cycle A→B→C→A of `Track` nodes over `SIMILAR_TO` — the
/// smallest graph on which trail reachability and distance reachability
/// disagree in both directions the changelog names.
fn cycle_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let params: HashMap<String, Value> = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(
        &mut graph,
        "CREATE (a:Track {title: 'A'}), (b:Track {title: 'B'}), (c:Track {title: 'C'}), \
         (a)-[:SIMILAR_TO]->(b), (b)-[:SIMILAR_TO]->(c), (c)-[:SIMILAR_TO]->(a)",
        &opts,
    )
    .expect("build the 3-cycle");
    graph
}

/// Run a read query and return its single string column, sorted.
fn titles(graph: &DirGraph, query: &str) -> Vec<String> {
    let params: HashMap<String, Value> = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let outcome = execute_read(graph, query, &opts).expect("cypher");
    let mut out: Vec<String> = outcome
        .result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::String(s) => s.clone(),
            other => panic!("expected a string title, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

/// A closed trail re-emits its source: from A, `*1..3` walks A→B (1 hop),
/// A→B→C (2 hops), and the closed trail A→B→C→A (3 hops, all edges distinct),
/// so A is reachable from itself. Hand-derived answer: {A, B, C}.
/// kglite 0.16.5 answered {B, C} — the source dropped from its own answer.
#[test]
fn closed_trail_reemits_the_source() {
    let graph = cycle_graph();
    assert_eq!(
        titles(
            &graph,
            "MATCH (a:Track {title: 'A'})-[:SIMILAR_TO*1..3]->(x) \
             RETURN DISTINCT x.title"
        ),
        ["A", "B", "C"]
    );
}

/// A minimum hop count of 2 must enumerate trails, not shortest distances:
/// undirected from A, the exactly-2-edge trails are A–(AB)–B–(BC)–C and
/// A–(CA)–C–(CB)–B (edge-distinct both). Hand-derived answer: {B, C}.
/// kglite 0.16.5 answered the empty set — both peers sit at shortest
/// distance 1, so the distance-set answer for `*2..2` was nothing.
#[test]
fn min_hop_two_enumerates_trails_not_distances() {
    let graph = cycle_graph();
    assert_eq!(
        titles(
            &graph,
            "MATCH (a:Track {title: 'A'})-[:SIMILAR_TO*2..2]-(x) \
             RETURN DISTINCT x.title"
        ),
        ["B", "C"]
    );
}
