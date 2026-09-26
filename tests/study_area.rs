//! The committed study area: the demand table, the boarding points, and the route cache that
//! has to cover them.
//!
//! Every test here reads files that are already on disk. Nothing reaches `scripts/fetch-data.py`
//! or the network - that is the point of committing the extract, and a test that fetched would
//! make the suite depend on a census API staying up.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use mas::data::{Flows, Stations};
use mas::routing::{Profile, Router};
use mas::scenario::Scenario;
use mas::world::Coord;

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn flows() -> Flows {
    Flows::load(&repo("data/flows.csv")).expect("the committed demand table loads")
}

fn stations() -> Stations {
    Stations::load(&repo("data/stations.json")).expect("the committed boarding points load")
}

/// Every committed scenario, so a station or a route added to one of them is checked too.
fn scenarios() -> Vec<Scenario> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(repo("scenarios"))
        .expect("the scenarios directory is there")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| Scenario::load(path).expect("a committed scenario loads"))
        .collect()
}

#[test]
fn every_flow_row_resolves_to_a_committed_area() {
    let flows = flows();
    let stations = stations();
    let corridor: HashSet<&str> = stations.corridor.iter().map(String::as_str).collect();
    for row in &flows.rows {
        for code in [row.origin_code.as_str(), row.destination_code.as_str()] {
            assert!(
                corridor.contains(code),
                "{code} is in the demand table but not in the corridor the stations were \
                 selected for"
            );
        }
        // A centroid at the null island is a join that silently failed rather than a place.
        for (latitude, longitude) in [
            (row.origin_latitude, row.origin_longitude),
            (row.destination_latitude, row.destination_longitude),
        ] {
            assert!(
                (49.0..=56.0).contains(&latitude) && (-6.0..=2.0).contains(&longitude),
                "{} -> {}: centroid {latitude},{longitude} is not in England or Wales",
                row.origin_code,
                row.destination_code
            );
        }
    }
    // The extract is the full cross product of the corridor, so a missing pair is a lost row.
    let areas = flows.areas().len();
    assert_eq!(flows.rows.len(), areas * areas);
}

#[test]
fn the_mode_columns_sum_to_the_row_total() {
    for row in &flows().rows {
        assert_eq!(
            row.mode_total(),
            row.total_count,
            "{} -> {}: modes sum to {}, the source's total is {}",
            row.origin_code,
            row.destination_code,
            row.mode_total(),
            row.total_count
        );
    }
}

#[test]
fn every_station_stands_in_a_corridor_area() {
    let stations = stations();
    for point in &stations.stations {
        assert!(
            stations.corridor.contains(&point.area_code),
            "{} is attributed to {}, which is not in the corridor",
            point.name,
            point.area_code
        );
        assert!(
            !point.selected_by.is_empty(),
            "{} does not say which rule selected it",
            point.name
        );
    }
}

#[test]
fn every_scenario_station_resolves_to_a_committed_station() {
    let stations = stations();
    for scenario in scenarios() {
        // `minimal.json` is deliberately synthetic: it is the fast test fixture and has no
        // business depending on a real corridor.
        if scenario.name == "minimal" {
            continue;
        }
        for station in &scenario.environment.stations {
            let committed = stations.named(&station.name).unwrap_or_else(|| {
                panic!(
                    "{}: station {:?} is not in data/stations.json",
                    scenario.name, station.name
                )
            });
            assert_eq!(
                (committed.latitude, committed.longitude),
                (station.latitude, station.longitude),
                "{}: station {:?} has drifted from the committed coordinate",
                scenario.name,
                station.name
            );
        }
    }
}

#[test]
fn the_cache_covers_every_station_pair_a_scenario_can_ask_for() {
    let scenarios = scenarios();
    let mut router = Router::new(&scenarios[0]);
    router
        .load_cache(&repo("data/routes.json"))
        .expect("the committed route cache loads");

    for scenario in &scenarios {
        for from in &scenario.environment.stations {
            for to in &scenario.environment.stations {
                if from.name == to.name {
                    continue;
                }
                let route = router.route(
                    Coord::new(from.latitude, from.longitude),
                    Coord::new(to.latitude, to.longitude),
                    Profile::Road,
                );
                // A straight-line fallback is two points; real road geometry is hundreds. This
                // is the only way to tell a hit from a miss without opening the cache itself.
                assert!(
                    route.points.len() > 2,
                    "{}: {} -> {} falls back to the straight line; run `build-cache`",
                    scenario.name,
                    from.name,
                    to.name
                );
            }
        }
    }
}

#[test]
fn nothing_the_suite_runs_reaches_the_network() {
    // The extract is committed precisely so the suite does not depend on a census API staying
    // up. The hosts the two by-hand fetchers reach are forbidden everywhere but `routing.rs`,
    // which owns `build-cache` - run by hand, never from a test.
    //
    // Spawning a process is a separate question with a second exception. `build-cache` shells out
    // to `curl`, so in `routing.rs` a spawn is how the network is reached; in `llm.rs` a spawn is
    // how the *model* is reached, and the process it starts is what holds the SDK. That is the
    // whole point of the sidecar - the engine gains no HTTP client - so `llm.rs` may spawn and
    // still may not name a host, and a test pointing it at a stub script reaches nothing at all.
    let hosts = [
        "nomisweb.co.uk",
        "arcgis.com",
        "overpass-api.de",
        "router.project-osrm.org",
        "api.anthropic.com",
    ];
    let mut sources: Vec<PathBuf> = Vec::new();
    for directory in ["src", "src/agents", "tests"] {
        for entry in std::fs::read_dir(repo(directory)).expect("a source directory") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    for path in sources {
        if path.ends_with("study_area.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        if !path.ends_with("routing.rs") {
            for needle in hosts {
                assert!(
                    !text.contains(needle),
                    "{} mentions {needle}; a run and a test stay offline",
                    path.display()
                );
            }
        }
        if !path.ends_with("routing.rs") && !path.ends_with("llm.rs") {
            assert!(
                !text.contains("Command::new"),
                "{} spawns a process; only `build-cache`'s curl and the model's sidecar do",
                path.display()
            );
        }
    }
}
