//! Distribution-valued knobs: a bare number still means one agent repeated, and a distribution
//! makes a cohort a population without costing the run its determinism.

use mas::agents::State;
use mas::events::EventLog;
use mas::routing::Router;
use mas::scenario::{Distribution, Scenario, Value};
use mas::sim::Sim;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

const STATIONS: &str = r#"
    "environment": {
      "stations": [
        { "name": "Northgate Park and Ride", "latitude": 45.00, "longitude": 5.00 },
        { "name": "Central Terminus", "latitude": 45.27, "longitude": 5.00 }
      ],
      "networks": [{
        "name": "Valley Corridor",
        "operator": "Valley Mobility",
        "lines": [{ "origin": "Northgate Park and Ride", "destination": "Central Terminus" }]
      }]
    }
"#;

/// Eight riders spawning on the same tick at the same point, so the only thing that can separate
/// them is what `max_wait_s` draws.
fn identical_riders(max_wait_s: &str) -> Scenario {
    Scenario::from_json(&format!(
        r#"{{
          "name": "patience", "tick_s": 1, "max_time_s": 14400, {STATIONS},
          "cohorts": [{{
            "kind": "GhostRider", "name": "walk-up riders", "count": 8,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "max_wait_s": {max_wait_s}
          }}]
        }}"#
    ))
    .unwrap_or_else(|error| panic!("loading a scenario with max_wait_s {max_wait_s}: {error:#}"))
}

/// The distinct ticks the cohort gave up on, in order.
fn give_up_ticks(scenario: &Scenario, seed: u64) -> Vec<f64> {
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(scenario, seed, Router::new(scenario));
    let summary = sim.run(&mut log).unwrap();
    assert!(!summary.stopped_by_clock, "the run should end on its own");

    let mut ticks: Vec<f64> = sim
        .agents()
        .iter()
        .filter(|agent| agent.state == State::Canceled)
        .map(|agent| agent.state_since_s)
        .collect();
    assert_eq!(ticks.len(), 8, "every rider should have given up");
    ticks.sort_by(f64::total_cmp);
    ticks.dedup();
    ticks
}

fn event_log_bytes(scenario: &Scenario, seed: u64, name: &str) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("mas-distributions-{name}.csv"));
    let mut log = EventLog::to_file(&path).unwrap();
    Sim::new(scenario, seed, Router::new(scenario))
        .run(&mut log)
        .unwrap();
    drop(log);
    std::fs::read(&path).unwrap()
}

/// A bare number in JSON is a `Fixed`, and a cohort of them is one agent repeated: identical
/// riders standing at the same station give up on the same tick.
#[test]
fn a_bare_number_parses_as_fixed_and_leaves_the_cohort_uniform() {
    let scenario = identical_riders("300");
    assert_eq!(scenario.cohorts[0].max_wait_s, Value::Fixed(300.0));
    assert_eq!(
        scenario.walk_speed_mps,
        Value::Fixed(1.4),
        "an unstated knob defaults to a fixed value too"
    );

    let ticks = give_up_ticks(&scenario, 11);
    assert_eq!(
        ticks.len(),
        1,
        "one give-up tick for eight riders: {ticks:?}"
    );
}

/// The Weibull is what rider give-up comes out of: patience drawn per rider, so the cohort
/// abandons over a range of ticks rather than all at once, and with no per-tick draw anywhere.
#[test]
fn a_weibull_max_wait_spreads_the_give_up_times() {
    let scenario = identical_riders(r#"{"weibull": {"scale": 900, "shape": 2}}"#);
    let ticks = give_up_ticks(&scenario, 11);
    assert!(
        ticks.len() > 4,
        "eight riders should abandon over a range of ticks, got {ticks:?}"
    );
    assert!(
        ticks.iter().all(|&tick| tick > 0.0 && tick.is_finite()),
        "{ticks:?}"
    );
}

/// A distribution-valued scenario is still a pure function of its seed.
#[test]
fn the_same_seed_reproduces_a_distribution_run_byte_for_byte() {
    let scenario = identical_riders(r#"{"weibull": {"scale": 900, "shape": 2}}"#);

    let first = event_log_bytes(&scenario, 7, "first");
    assert!(!first.is_empty(), "the run should have logged something");
    assert_eq!(
        first,
        event_log_bytes(&scenario, 7, "second"),
        "same seed, same bytes"
    );
    assert_ne!(
        first,
        event_log_bytes(&scenario, 8, "other-seed"),
        "a different seed should draw different patience"
    );
}

/// A normal will eventually hand back a negative number, and a zero speed is a hang rather than
/// an error. Both are clamped at the draw.
#[test]
fn a_draw_never_yields_a_negative_duration_or_a_zero_speed() {
    let mut rng = ChaCha8Rng::seed_from_u64(3);
    let duration = Value::Drawn(Distribution::Normal {
        mean: 0.0,
        sd: 600.0,
    });
    let speed = Value::Drawn(Distribution::Normal {
        mean: 0.05,
        sd: 1.0,
    });

    let mut unclamped_negatives = 0;
    for _ in 0..10_000 {
        let duration_s = duration.draw_non_negative(&mut rng);
        assert!(
            duration_s >= 0.0 && duration_s.is_finite(),
            "{duration_s} s"
        );

        if duration.draw(&mut rng) < 0.0 {
            unclamped_negatives += 1;
        }

        let speed_mps = speed.draw_speed(&mut rng);
        assert!(speed_mps > 0.0 && speed_mps.is_finite(), "{speed_mps} m/s");
    }
    assert!(
        unclamped_negatives > 4_000,
        "the premise of this test: an unclamped draw from a normal centred on zero goes negative \
         about half the time, and did {unclamped_negatives} times in 10000"
    );
}

/// A distribution's parameters are a trust boundary: a shape of zero puts an infinity through the
/// inverse, and bounds the wrong way round draw outside themselves. Both fail at load, along with
/// a misspelt shape name - silently falling back to a default would quietly change the experiment.
///
/// ponytail: a misspelt shape is reported by serde as "no variant of untagged enum Value" plus a
/// line and column, not as "unknown distribution `weibul`". Naming it would take a hand-written
/// `Deserialize`; the location is enough to find the typo.
#[test]
fn a_nonsensical_distribution_is_rejected_at_load() {
    let expected = [
        (r#"{"weibull": {"scale": 900, "shape": 0}}"#, "shape"),
        (r#"{"uniform": {"low": 900, "high": 300}}"#, "low first"),
        (r#"{"normal": {"mean": 900, "sd": -1}}"#, "sd not negative"),
        (
            r#"{"normal": {"mean": -900, "sd": 1}}"#,
            "must not be negative",
        ),
        (
            r#"{"weibul": {"scale": 900, "shape": 2}}"#,
            "untagged enum Value at line",
        ),
        (r#"{"bernoulli": {"p": 1.5}}"#, "is a probability"),
    ];

    for (knob, fragment) in expected {
        let json = format!(
            r#"{{
              "name": "broken", {stations},
              "cohorts": [{{
                "kind": "GhostRider", "name": "riders", "count": 1,
                "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                "max_wait_s": {knob}
              }}]
            }}"#,
            stations = STATIONS
        );
        let Err(error) = Scenario::from_json(&json) else {
            panic!("max_wait_s {knob} should be rejected");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains(fragment),
            "max_wait_s {knob}: error should mention {fragment:?}, got: {message}"
        );
    }
}

/// A cohort of riders with the given `additional_passengers` and `spawn_window`, run to the end.
fn riders_with(additional_passengers: &str, spawn_window: &str, count: u32) -> Sim {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "parties", "tick_s": 10, "max_time_s": 14400, {STATIONS},
          "cohorts": [{{
            "kind": "GhostRider", "name": "riders", "count": {count},
            "spawn_window": {spawn_window},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "max_wait_s": 60,
            "additional_passengers": {additional_passengers}
          }}]
        }}"#
    ))
    .unwrap_or_else(|error| panic!("loading the parties scenario: {error:#}"));
    let mut sim = Sim::new(&scenario, 5, Router::new(&scenario));
    sim.run(&mut EventLog::discarding().unwrap()).unwrap();
    sim
}

/// A Bernoulli draws nothing but zero or one, in about the share it was asked for, which is what
/// makes it the companion rule: seven cars in a hundred carrying one more person.
#[test]
fn a_bernoulli_party_is_zero_or_one_companion_in_the_share_asked_for() {
    let window = r#"{ "start_s": 0, "end_s": 0 }"#;
    let sim = riders_with(r#"{"bernoulli": {"p": 0.25}}"#, window, 400);
    let seats: Vec<u32> = sim.agents().iter().map(|agent| agent.seats).collect();
    assert!(
        seats.iter().all(|&seats| seats == 1 || seats == 2),
        "{seats:?}"
    );
    let with_companion = seats.iter().filter(|&&seats| seats == 2).count();
    assert!(
        (70..=130).contains(&with_companion),
        "a quarter of 400 riders should bring a companion, {with_companion} did"
    );

    for (p, expected) in [("0", 1), ("1", 2)] {
        let sim = riders_with(&format!(r#"{{"bernoulli": {{"p": {p}}}}}"#), window, 20);
        assert!(
            sim.agents().iter().all(|agent| agent.seats == expected),
            "p = {p} is certain"
        );
    }
}

/// A spawn time drawn from a normal is a peak rather than a flat window, and a draw from its tail
/// is clamped to the run instead of spawning an agent before the clock starts.
#[test]
fn a_drawn_spawn_window_makes_a_peak_and_stays_inside_the_run() {
    let sim = riders_with("0", r#"{"normal": {"mean": 600, "sd": 900}}"#, 300);
    let spawns: Vec<f64> = sim.agents().iter().map(|agent| agent.spawn_s).collect();
    assert_eq!(spawns.len(), 300);
    assert!(spawns
        .iter()
        .all(|&spawn_s| (0.0..=14_400.0).contains(&spawn_s)));
    assert!(
        spawns.iter().filter(|&&spawn_s| spawn_s == 0.0).count() > 50,
        "a normal centred 600 s in with a 900 s spread puts a quarter of its mass before zero, \
         which the clamp puts at zero"
    );
    let mean_s = spawns.iter().sum::<f64>() / spawns.len() as f64;
    assert!((600.0..1100.0).contains(&mean_s), "mean spawn {mean_s} s");

    // A bare number is a fixed spawn time: every agent at once, and no randomness consumed.
    let sim = riders_with("0", "120", 5);
    assert!(sim.agents().iter().all(|agent| agent.spawn_s == 120.0));
}

/// The window form still draws exactly what it drew before the drawn form existed: the committed
/// runs pin that byte for byte, and this pins the rejection of a drawn time centred before zero.
#[test]
fn a_drawn_spawn_window_centred_before_zero_is_a_load_error() {
    let json = format!(
        r#"{{
          "name": "broken", {STATIONS},
          "cohorts": [{{
            "kind": "GhostRider", "name": "riders", "count": 1,
            "spawn_window": {{"normal": {{"mean": -60, "sd": 10}}}},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }}
          }}]
        }}"#
    );
    let error = Scenario::from_json(&json).expect_err("a spawn centred before zero is rejected");
    assert!(
        format!("{error:#}").contains("centred before zero"),
        "{error:#}"
    );
}
