//! Network structuring: four ways of connecting one station set, and what each one is.
//!
//! The hypothesis under test in the sweep is that a structured network beats a free-form one. What
//! these tests pin is narrower and more useful: that each construction is *the thing it claims to
//! be*, so a row in that comparison means what its label says. A minimum spanning tree that was
//! quietly not spanning, or a flow ranking that ignored the flows, would still produce a full set
//! of rows and a plausible chart.

use mas::data::Flows;
use mas::events::EventLog;
use mas::network::{self, Construction};
use mas::routing::Router;
use mas::scenario::{Scenario, Station};
use mas::sim::Sim;
use std::path::Path;

/// A station set with no three points collinear and no four cocircular, so the triangulation is
/// unambiguous: a square nudged out of square.
fn four_in_convex_position() -> Vec<Station> {
    stations(&[
        ("north west", 45.30, 5.00),
        ("north east", 45.31, 5.40),
        ("south east", 44.98, 5.42),
        ("south west", 45.00, 5.01),
    ])
}

fn stations(points: &[(&str, f64, f64)]) -> Vec<Station> {
    points
        .iter()
        .map(|(name, latitude, longitude)| Station {
            name: name.to_string(),
            latitude: *latitude,
            longitude: *longitude,
            area_code: None,
        })
        .collect()
}

fn with_areas(points: &[(&str, f64, f64, &str)]) -> Vec<Station> {
    points
        .iter()
        .map(|(name, latitude, longitude, area)| Station {
            name: name.to_string(),
            latitude: *latitude,
            longitude: *longitude,
            area_code: Some(area.to_string()),
        })
        .collect()
}

/// The sparsest connected structure there is: exactly one edge fewer than there are stations, and
/// every station reachable from every other.
#[test]
fn a_minimum_spanning_tree_has_one_edge_fewer_than_it_has_stations() {
    let set = four_in_convex_position();
    let pairs = network::pairs(&set, Construction::MinimumSpanningTree, None).unwrap();
    assert_eq!(pairs.len(), set.len() - 1, "got {pairs:?}");

    // Connected: grow a component from station zero and it must swallow the whole set.
    let mut reached = vec![false; set.len()];
    reached[0] = true;
    for _ in 0..set.len() {
        for (left, right) in &pairs {
            if reached[*left] || reached[*right] {
                reached[*left] = true;
                reached[*right] = true;
            }
        }
    }
    assert!(reached.iter().all(|seen| *seen), "the tree is not spanning");
}

/// Four points in convex position triangulate into two triangles: the four sides and one diagonal.
/// Five edges, not six - the other diagonal belongs to the triangulation this one is not.
#[test]
fn the_delaunay_triangulation_of_four_points_in_convex_position_has_five_edges() {
    let pairs = network::pairs(&four_in_convex_position(), Construction::Delaunay, None).unwrap();
    assert_eq!(pairs.len(), 5, "got {pairs:?}");
    // Exactly one of the two diagonals, which is what makes it a triangulation and not the
    // complete graph.
    let diagonals = pairs
        .iter()
        .filter(|pair| **pair == (0, 2) || **pair == (1, 3))
        .count();
    assert_eq!(diagonals, 1, "got {pairs:?}");
}

/// Flow-ranked takes the busiest pairs first. With one dominant pair and one thin one, the thin
/// one must never be reached: two edges connect three stations, and the ranking decides which two.
#[test]
fn flow_ranked_takes_the_busiest_pair_first() {
    let flows = Flows::load(Path::new("tests/fixtures/one-dominant-pair.csv")).unwrap();
    // Deliberately at odds with the geometry: Beta–Gamma is the *shortest* pair here and the
    // thinnest flow, so a construction reading distance instead of demand would take it.
    let set = with_areas(&[
        ("Alpha", 45.00, 5.00, "A001"),
        ("Beta", 45.10, 5.00, "A002"),
        ("Gamma", 45.09, 5.01, "A003"),
    ]);
    let pairs = network::pairs(&set, Construction::FlowRanked, Some(&flows)).unwrap();

    assert!(
        pairs.contains(&(0, 1)),
        "Alpha-Beta, 1000 commutes: {pairs:?}"
    );
    assert!(
        pairs.contains(&(0, 2)),
        "Alpha-Gamma, 500 commutes: {pairs:?}"
    );
    assert!(
        !pairs.contains(&(1, 2)),
        "Beta-Gamma is the thinnest flow and everything is already connected: {pairs:?}"
    );

    // The geometric constructions read none of that, which is the asymmetry the comparison turns
    // on: the minimum spanning tree takes the short thin edge the ranking refused.
    let tree = network::pairs(&set, Construction::MinimumSpanningTree, None).unwrap();
    assert!(tree.contains(&(1, 2)), "got {tree:?}");
    assert!(Construction::FlowRanked.reads_demand());
    assert!(!Construction::FlowRanked.directed());
    for blind in [
        Construction::FreeForm,
        Construction::Delaunay,
        Construction::MinimumSpanningTree,
    ] {
        assert!(!blind.reads_demand(), "{blind} must be pure geometry");
    }
}

/// Every construction connects the stations it was given and invents none. Each undirected pair
/// becomes two directed lines, so whatever a construction connects, it connects both ways.
#[test]
fn every_construction_returns_the_station_set_it_was_given() {
    let corridor = corridor_scenario("free-form");
    let names: Vec<&str> = corridor
        .environment
        .stations
        .iter()
        .map(|station| station.name.as_str())
        .collect();

    for construction in [
        "free-form",
        "delaunay",
        "minimum-spanning-tree",
        "flow-ranked",
    ] {
        let scenario = corridor_scenario(construction);
        let lines = &scenario.environment.networks[0].lines;
        assert_eq!(
            scenario.environment.networks.len(),
            1,
            "{construction}: one constructed network"
        );
        assert_eq!(
            lines.len() % 2,
            0,
            "{construction}: every pair is two directed lines"
        );
        for line in lines {
            assert!(
                names.contains(&line.origin.as_str()) && names.contains(&line.destination.as_str()),
                "{construction}: line {} -> {} names a station that was not given",
                line.origin,
                line.destination
            );
        }
        // Both directions of every pair, and no pair twice.
        for line in lines {
            assert_eq!(
                lines
                    .iter()
                    .filter(|other| other.origin == line.destination
                        && other.destination == line.origin)
                    .count(),
                1,
                "{construction}: {} -> {} has no single reverse",
                line.origin,
                line.destination
            );
        }
    }

    // Free-form is every station reachable from every other, which is the upper bound the other
    // three are measured against.
    let stations = names.len();
    assert_eq!(
        corridor.environment.networks[0].lines.len(),
        stations * (stations - 1)
    );
}

/// The directed construction takes flow-ranked's pairs and orients each one, so it must connect
/// exactly the same stations with exactly half the lines - and every line must run the way the
/// demand table says the commute runs. That is the one variable between the two, and if it were not
/// the only one the comparison in `FINDINGS.md` would not mean what it says.
#[test]
fn flow_directed_is_flow_ranked_one_way_only() {
    let ranked = corridor_scenario("flow-ranked");
    let directed = corridor_scenario("flow-directed");
    let ranked_lines = &ranked.environment.networks[0].lines;
    let lines = &directed.environment.networks[0].lines;

    assert_eq!(lines.len() * 2, ranked_lines.len(), "half the lines");
    for line in lines {
        // The same pair, and only one of its two directions.
        assert!(
            ranked_lines
                .iter()
                .any(|other| other.origin == line.origin && other.destination == line.destination),
            "{} -> {} is not a flow-ranked pair",
            line.origin,
            line.destination
        );
        assert!(
            !lines
                .iter()
                .any(|other| other.origin == line.destination && other.destination == line.origin),
            "{} -> {} runs both ways",
            line.origin,
            line.destination
        );
    }

    // And it is oriented by the demand rather than by the station order: every line runs the way
    // the bigger of the two flows between its areas runs. On this corridor that is radial -
    // 1,235 people a day travel from Trawscoed Bridge's area into Aberystwyth's and 216 back - and
    // a construction reading only the size of a flow could not tell those two apart.
    let flows = Flows::load(Path::new("data/flows.csv")).unwrap();
    let area = |name: &str| -> String {
        directed
            .environment
            .stations
            .iter()
            .find(|station| station.name == name)
            .and_then(|station| station.area_code.clone())
            .expect("a corridor station with an area code")
    };
    let commute = |from: &str, to: &str| -> u32 {
        flows
            .rows
            .iter()
            .filter(|row| row.origin_code == from && row.destination_code == to)
            .map(|row| row.total_count)
            .sum()
    };
    for line in lines {
        let (from, to) = (area(&line.origin), area(&line.destination));
        assert!(
            commute(&from, &to) >= commute(&to, &from),
            "{} -> {} runs against the demand: {} against {}",
            line.origin,
            line.destination,
            commute(&from, &to),
            commute(&to, &from)
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.origin == "Trawscoed Bridge" && line.destination == "Aberystwyth"),
        "the corridor's busiest pair, the way it runs: {lines:?}"
    );

    assert!(Construction::FlowDirected.directed());
    assert!(Construction::FlowDirected.reads_demand());
}

/// A construction is part of the scenario, so a run over one is as reproducible as any other:
/// the same seed and the same construction twice must agree agent for agent.
#[test]
fn the_same_construction_and_seed_produce_the_same_run() {
    let trace = |construction: &str| {
        let scenario = corridor_scenario(construction);
        let mut log = EventLog::discarding().unwrap();
        let mut sim = Sim::new(&scenario, 42, Router::new(&scenario));
        let summary = sim.run(&mut log).unwrap();
        let agents: Vec<(f64, f64, f64)> = sim
            .agents()
            .iter()
            .map(|agent| (agent.spawn_s, agent.distance_km, agent.loaded_km))
            .collect();
        (summary.ticks, summary.events, agents)
    };

    for construction in [
        "free-form",
        "delaunay",
        "minimum-spanning-tree",
        "flow-ranked",
        "flow-directed",
    ] {
        assert_eq!(
            trace(construction),
            trace(construction),
            "{construction} is not reproducible"
        );
    }
    // And the constructions are not all the same network wearing four labels.
    assert_ne!(trace("minimum-spanning-tree").2, trace("free-form").2);
}

/// Declaring both a construction and the lines it would derive means one of the two is being
/// ignored, and a flow-ranked network with no demand table cannot rank anything.
#[test]
fn a_contradictory_construction_is_a_load_error() {
    let both = r#"{
      "name": "both", "tick_s": 1, "max_time_s": 7200,
      "environment": {
        "stations": [
          { "name": "one", "latitude": 45.00, "longitude": 5.00 },
          { "name": "two", "latitude": 45.27, "longitude": 5.00 }
        ],
        "construction": "delaunay",
        "networks": [{
          "name": "by hand", "operator": "someone",
          "lines": [{ "origin": "one", "destination": "two" }]
        }]
      },
      "cohorts": []
    }"#;
    let error = format!("{:#}", Scenario::from_json(both).unwrap_err());
    assert!(error.contains("being ignored"), "unexpected error: {error}");

    let no_flows = both
        .replace(
            r#""construction": "delaunay","#,
            r#""construction": "flow-ranked","#,
        )
        .replace(
            r#""networks": [{
          "name": "by hand", "operator": "someone",
          "lines": [{ "origin": "one", "destination": "two" }]
        }]"#,
            r#""networks": []"#,
        );
    let error = format!("{:#}", Scenario::from_json(&no_flows).unwrap_err());
    assert!(
        error.contains("environment.flows"),
        "unexpected error: {error}"
    );
}

fn corridor_scenario(construction: &str) -> Scenario {
    let text = std::fs::read_to_string("scenarios/network.json").unwrap();
    let patched = text.replace(
        r#""construction": "free-form""#,
        &format!(r#""construction": "{construction}""#),
    );
    Scenario::from_json(&patched).unwrap_or_else(|error| panic!("{construction}: {error:#}"))
}
