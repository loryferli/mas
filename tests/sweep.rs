//! The sweep: one row per (axis combination, seed), and the same config twice writing the same
//! bytes.

use mas::scenario::Scenario;
use std::path::{Path, PathBuf};

/// A two-point axis over the driver count, one seed: two runs, two rows.
const TWO_POINT: &str = r#"{
  "scenario": "SCENARIO",
  "seeds": { "start": 42, "count": 1 },
  "axes": [
    { "name": "drivers", "points": [
      { "value": 2, "set": { "/cohorts/0/count": 2 } },
      { "value": 4, "set": { "/cohorts/0/count": 4 } }
    ] }
  ]
}"#;

/// The config goes in the temp directory, so its scenario path has to be absolute: a relative
/// one resolves against the config file's own directory.
fn config_text(text: &str) -> String {
    let scenario = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/baseline.json");
    text.replace("SCENARIO", scenario.to_str().unwrap())
}

fn sweep_to_file(name: &str, text: &str) -> Vec<u8> {
    let config = std::env::temp_dir().join(format!("mas-sweep-{name}.json"));
    let out = std::env::temp_dir().join(format!("mas-sweep-{name}.csv"));
    std::fs::write(&config, config_text(text)).unwrap();
    mas::sweep::run(
        &config,
        &out,
        &PathBuf::from("data/routes.json"),
        &mas::incentives::NoIncentive,
    )
    .unwrap();
    std::fs::read(&out).unwrap()
}

fn rows(csv: &[u8]) -> Vec<String> {
    String::from_utf8(csv.to_vec())
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

#[test]
fn a_two_point_sweep_produces_exactly_two_rows() {
    let written = sweep_to_file("two-point", TWO_POINT);
    let rows = rows(&written);
    assert_eq!(rows.len(), 2, "one row per axis point at one seed");
    assert!(rows[0].starts_with("baseline,2,42,"), "{}", rows[0]);
    assert!(rows[1].starts_with("baseline,4,42,"), "{}", rows[1]);
    assert_ne!(rows[0], rows[1], "two driver counts, two different runs");
}

#[test]
fn the_same_config_twice_writes_the_same_bytes() {
    let first = sweep_to_file("first", TWO_POINT);
    assert_eq!(first, sweep_to_file("second", TWO_POINT), "same rows");
}

/// A pointer that does not resolve stops the sweep rather than running the base scenario
/// unpatched: a silent no-op patch is a different experiment than the config describes.
#[test]
fn a_pointer_that_does_not_resolve_is_an_error() {
    let config = std::env::temp_dir().join("mas-sweep-bad-pointer.json");
    std::fs::write(
        &config,
        config_text(TWO_POINT).replace("/cohorts/0/count", "/cohorts/0/drivers"),
    )
    .unwrap();
    let error = mas::sweep::run(
        &config,
        &std::env::temp_dir().join("mas-sweep-bad-pointer.csv"),
        &PathBuf::from("data/routes.json"),
        &mas::incentives::NoIncentive,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("/cohorts/0/drivers"),
        "the error should name the pointer: {error:#}"
    );
}

/// **Every** committed sweep config still describes runs the engine can do.
///
/// A pointer that does not resolve is caught at run time, which for these configs is minutes of
/// sweeping - and a config whose base scenario has since moved a field would run a different
/// experiment from the one it describes with nothing downstream able to tell. This walks all of
/// them in milliseconds.
///
/// ponytail: one axis point at a time rather than the cross product of every axis. A pointer is
/// what goes stale, and every pointer appears in some single point; a combination that is invalid
/// only in company - a declaration lead meeting margins from another axis - is still a load error
/// the sweep itself raises on its first run.
#[test]
fn every_committed_sweep_config_loads() {
    let mut checked = 0;
    let mut configs: Vec<std::path::PathBuf> = std::fs::read_dir("sweeps")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    configs.sort();
    assert!(configs.len() >= 7, "the committed configs: {configs:?}");

    for path in configs {
        let config = mas::sweep::SweepConfig::load(&path)
            .unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
        let scenario_path = config.scenario_path(&path);
        assert!(
            scenario_path.exists(),
            "{}: base scenario {} is missing",
            path.display(),
            scenario_path.display()
        );
        let base: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&scenario_path).unwrap()).unwrap();
        assert!(!config.axes.is_empty(), "{}: no axes", path.display());
        for axis in &config.axes {
            assert!(
                !axis.points.is_empty(),
                "{}: axis {} has no points",
                path.display(),
                axis.name
            );
            for point in &axis.points {
                let patched = mas::sweep::patch(&base, &[point]).unwrap_or_else(|error| {
                    panic!("{} axis {}: {error:#}", path.display(), axis.name)
                });
                Scenario::from_json(&patched.to_string()).unwrap_or_else(|error| {
                    panic!(
                        "{} axis {} point {}: {error:#}",
                        path.display(),
                        axis.name,
                        point.value
                    )
                });
                checked += 1;
            }
        }
    }
    assert!(checked > 30, "only {checked} axis points checked");
}
