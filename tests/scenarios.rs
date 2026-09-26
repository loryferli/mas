//! Every committed scenario loads, and every deliberately broken one is rejected with a
//! message that names the problem.

use mas::scenario::{AgentKind, Scenario};
use std::path::{Path, PathBuf};

fn json_files(dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("reading {}: {e}", root.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no scenarios found in {}",
        root.display()
    );
    files
}

#[test]
fn every_committed_scenario_loads() {
    for path in json_files("scenarios") {
        let scenario = Scenario::load(&path)
            .unwrap_or_else(|e| panic!("{} should load: {e:#}", path.display()));

        // A scenario nobody can be served by is a broken experiment, not a valid config.
        assert!(
            scenario.tick_s > 0.0 && scenario.max_time_s > scenario.tick_s,
            "{}: clock settings make no sense",
            path.display()
        );
    }
}

#[test]
fn broken_scenarios_are_rejected_with_a_useful_message() {
    // Each fixture is paired with a fragment its error must contain, so a regression that
    // still fails but stops explaining why is caught too.
    let expected = [
        ("undeclared-station.json", "Nowhere In Particular"),
        ("rider-with-vehicle.json", "must not declare a vehicle"),
        ("unknown-field.json", "unexpected_field"),
        ("ghost-declaring-ahead.json", "does not use the app"),
        ("margins-without-a-lead.json", "advanced_declaration_lead_s"),
        ("taxi-cohort.json", "no cohort spawns one"),
    ];

    for (name, fragment) in expected {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let Err(error) = Scenario::load(&path) else {
            panic!("{name} should be rejected");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains(fragment),
            "{name}: error should mention {fragment:?}, got: {message}"
        );
    }
}

#[test]
fn minimal_scenario_has_one_driver_and_one_rider() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/minimal.json");
    let scenario = Scenario::load(&path).unwrap();

    assert_eq!(scenario.total_agents(), 2);
    assert_eq!(scenario.cohorts.len(), 2);
    assert!(scenario.cohorts[0].kind.is_driver());
    assert!(scenario.cohorts[1].kind.is_rider());
    assert!(scenario.unimplemented_kinds().is_empty());
}

#[test]
fn every_kind_in_the_mixed_scenario_has_a_behaviour() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/mixed-behaviours.json");
    let scenario = Scenario::load(&path).unwrap();
    assert!(scenario.unimplemented_kinds().is_empty());
    assert!(scenario
        .cohorts
        .iter()
        .any(|cohort| cohort.kind == AgentKind::PolynomialDriver));
}
