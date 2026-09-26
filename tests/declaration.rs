//! Advanced time declaration: announcing a trip before setting off, the departure times an agent
//! will accept, and the advance match that follows from both.

use mas::agents::{Agent, State, TransitionReason};
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::operator::Operator;
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use mas::world::{haversine_km, Coord, World};
use std::path::Path;

fn load(name: &str) -> Scenario {
    Scenario::load(Path::new("scenarios").join(name).as_path())
        .unwrap_or_else(|error| panic!("loading {name}: {error:#}"))
}

fn run(scenario: &Scenario, seed: u64) -> (Sim, Metrics, u64) {
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(scenario, seed, Router::new(scenario));
    let summary = sim.run(&mut log).unwrap();
    let metrics = Metrics::collect(sim.agents(), &summary, &NoIncentive);
    (sim, metrics, summary.ticks)
}

/// Two stations serving one destination, so a rider between them has a first choice and a second.
/// None of these coordinates are in the committed cache, so every leg is the straight-line
/// fallback and the arithmetic below is the whole of the geometry.
const TWO_WAYS_IN: &str = r#"
    "environment": {
      "stations": [
        { "name": "Ashford Green",     "latitude": 46.000, "longitude": 6.00 },
        { "name": "Bellwood Cross",    "latitude": 46.020, "longitude": 6.00 },
        { "name": "Cathedral Square",  "latitude": 46.270, "longitude": 6.00 }
      ],
      "networks": [{
        "name": "Cathedral Corridor",
        "operator": "Cathedral Mobility",
        "lines": [
          { "origin": "Ashford Green",  "destination": "Cathedral Square" },
          { "origin": "Bellwood Cross", "destination": "Cathedral Square" }
        ]
      }]
    }
"#;

/// One driver standing at Ashford and one rider nearer Bellwood, both declaring ten minutes ahead
/// of the same departure time and neither accepting any margin. The rider accepts both lines; the
/// driver is only ever going to drive the Ashford one.
fn declared_pair() -> Scenario {
    Scenario::from_json(&format!(
        r#"{{
          "name": "declared-pair", "tick_s": 1, "max_time_s": 14400, {TWO_WAYS_IN},
          "cohorts": [
            {{
              "kind": "CarpoolDriver", "name": "the declaring driver", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 46.000, "longitude": 6.00 }},
              "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
              "departure_offset_s": 600,
              "advanced_declaration_lead_s": 600,
              "max_wait_s": 3600,
              "vehicle": {{ "capacity": 5 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "the declaring rider", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 46.015, "longitude": 6.00 }},
              "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
              "departure_offset_s": 600,
              "advanced_declaration_lead_s": 600
            }}
          ]
        }}"#
    ))
    .expect("valid scenario")
}

/// The scenarios that declare nothing must produce the run they produced before declaration
/// existed. These are the figures from that run, against the committed cache, which is what
/// `cargo run -- run --scenario ... --seed 42` prints: a change to any of them means a knob
/// nobody turned has moved.
#[test]
fn a_scenario_that_declares_nothing_runs_exactly_as_it_did_before() {
    // The only test that reads the committed cache, because these are the figures the documented
    // command produces and the point is that they have not moved.
    let run = |scenario: &Scenario| {
        let mut router = Router::new(scenario);
        router
            .load_cache(Path::new("data/routes.json"))
            .expect("the committed route cache");
        let mut log = EventLog::discarding().unwrap();
        let mut sim = Sim::new(scenario, 42, router);
        let summary = sim.run(&mut log).unwrap();
        (
            Metrics::collect(sim.agents(), &summary, &NoIncentive),
            summary.ticks,
        )
    };

    let (minimal, ticks) = run(&load("minimal.json"));
    assert_eq!(ticks, 5462);
    assert_eq!(minimal.passengers_served, 1);
    assert!((minimal.vehicle_km_total - 46.87903669852693).abs() < 1e-9);
    assert!((minimal.vehicle_km_loaded - 38.747212859351464).abs() < 1e-9);
    assert_eq!(minimal.mean_wait_s, 521.0);

    let (baseline, ticks) = run(&load("baseline.json"));
    assert_eq!(ticks, 7017);
    assert_eq!(baseline.passengers_served, 25);
    assert!((baseline.vehicle_km_total - 187.83781934543276).abs() < 1e-9);
    assert!((baseline.vehicle_km_empty - 93.01345245208708).abs() < 1e-9);
    assert_eq!(baseline.mean_wait_s, 154.28);
}

/// The acceptance window is an interval around the declared departure time, and a sign error in
/// either margin would invert it.
#[test]
fn the_acceptance_window_is_an_interval_around_the_declared_departure() {
    let scenario = load("advanced-declaration.json");
    // Both cohorts in the committed scenario carry these, and the test is about them.
    let (earliness_s, lateness_s) = (300.0, 900.0);
    for cohort in &scenario.cohorts {
        assert_eq!(cohort.earliness_margin_s, earliness_s);
        assert_eq!(cohort.lateness_margin_s, lateness_s);
    }

    let (sim, _, _) = run(&scenario, 42);
    for agent in sim.agents() {
        assert!(
            agent.window.earliest_s <= agent.window.latest_s,
            "agent {:?} accepts departing between {} s and {} s, which is nothing at all",
            agent.id,
            agent.window.earliest_s,
            agent.window.latest_s
        );
        assert!((agent.window.earliest_s - (agent.departure_s - earliness_s)).abs() < 1e-9);
        assert!((agent.window.latest_s - (agent.departure_s + lateness_s)).abs() < 1e-9);
    }
}

/// With every agent declaring and no margin either side, the window is a point: an agent that is
/// matched in advance still sets off exactly when it said it would.
#[test]
fn declaring_agents_with_no_margins_depart_at_their_declared_time() {
    let (sim, metrics, _) = run(&declared_pair(), 42);
    assert_eq!(metrics.passengers_served, 1, "the pair travelled together");

    for agent in sim.agents() {
        assert!(agent.declares_ahead());
        assert_eq!(
            agent.window.earliest_s, agent.departure_s,
            "no margin means the window is the declared time itself"
        );
        assert_eq!(agent.window.latest_s, agent.departure_s);
        assert_eq!(
            agent.departed_s,
            Some(agent.departure_s),
            "agent {:?} declared {} s and set off at {:?}",
            agent.id,
            agent.departure_s,
            agent.departed_s
        );
        assert_eq!(
            agent.declared_s,
            Some(0.0),
            "both spawn at 0 and declare their whole lead ahead of a 600 s departure"
        );
    }
}

/// A declared rider sits in the pool of every line it would accept and walks to whichever one a
/// driver takes it from - here its second choice, because that is the only line the driver drives.
#[test]
fn a_declared_rider_walks_to_the_line_the_driver_took_it_from() {
    let scenario = declared_pair();
    let router = Router::new(&scenario);
    let world = World::from_environment(&scenario.environment, &router);
    let operator = Operator::new();

    let station = |name: &str| {
        world
            .stations()
            .iter()
            .find(|station| station.name == name)
            .expect("declared")
            .coord
    };
    let line_from = |name: &str| {
        world
            .lines()
            .iter()
            .position(|line| world.station(line.origin).name == name)
            .expect("declared")
    };
    let ashford = line_from("Ashford Green");
    let bellwood = line_from("Bellwood Cross");

    // The premise: the rider prefers Bellwood and would accept Ashford, and the driver standing
    // at Ashford prefers Ashford. Without that this test proves nothing.
    let centre = |area: &mas::scenario::SpawnArea| Coord::new(area.latitude, area.longitude);
    let ranked = operator.ranked_lines(
        &world,
        &router,
        centre(&scenario.cohorts[1].origin),
        centre(
            scenario.cohorts[1]
                .destination
                .as_ref()
                .expect("a rider has somewhere to be"),
        ),
    );
    assert_eq!(ranked.len(), 2, "both lines are offered");
    assert_eq!(ranked[0].line.0 as usize, bellwood, "first choice");
    assert_eq!(ranked[1].line.0 as usize, ashford, "second choice");
    assert!(
        ranked[1].walk_s <= 1800.0,
        "and the second choice is inside the rider's default walk of {} s",
        ranked[1].walk_s
    );
    let driver_ranked = operator.ranked_lines(
        &world,
        &router,
        centre(&scenario.cohorts[0].origin),
        centre(
            scenario.cohorts[0]
                .destination
                .as_ref()
                .expect("a rider has somewhere to be"),
        ),
    );
    assert_eq!(driver_ranked[0].line.0 as usize, ashford);

    let (sim, metrics, _) = run(&scenario, 42);
    assert_eq!(metrics.passengers_served, 1);

    let driver = sim
        .agents()
        .iter()
        .find(|agent| agent.kind.is_driver())
        .expect("the scenario declares a driver");
    let rider = sim
        .agents()
        .iter()
        .find(|agent| agent.kind.is_rider())
        .expect("the scenario declares a rider");
    assert_eq!(rider.claimed_by, Some(driver.id), "taken before it set off");
    assert_eq!(rider.line, driver.line, "and onto the driver's line");
    assert_eq!(rider.line.expect("claimed").0 as usize, ashford);
    assert_eq!(rider.state, State::EndJourney);
    assert_eq!(rider.reason, TransitionReason::ArrivedAtDestination);

    // It was carried along the Ashford line, and the distance it covered on foot is the walk to
    // Ashford rather than the shorter one to Bellwood it would have taken unmatched.
    let carried_km = world.line(rider.line.expect("claimed")).route.distance_km;
    assert!((rider.loaded_km - carried_km).abs() < 1e-9);
    let walked_km = rider.distance_km - rider.loaded_km;
    let to_ashford_km = haversine_km(rider.origin, station("Ashford Green"));
    let to_bellwood_km = haversine_km(rider.origin, station("Bellwood Cross"));
    assert!(to_bellwood_km < to_ashford_km, "the premise, in kilometres");
    assert!(
        (walked_km - to_ashford_km).abs() < 1e-9,
        "expected the {to_ashford_km} km walk to Ashford, got {walked_km} km"
    );

    // Seated from one pool and gone from all of them: nothing is left registered, and the run
    // ended on its own rather than a leaked registration keeping it going.
    assert_eq!(sim.operator().registered_riders(), 0);
    assert_eq!(sim.operator().registered_drivers(), 0);
}

/// Adoption changes matching. The committed scenario declares on both sides; the same scenario
/// with the declaration taken off is matched on the spot, and the waits are the difference.
#[test]
fn declaring_ahead_changes_how_the_same_scenario_is_matched() {
    let declared = load("advanced-declaration.json");
    let text = std::fs::read_to_string("scenarios/advanced-declaration.json").unwrap();
    // The same file with the declaration turned off, so nothing but adoption differs.
    let on_the_spot = Scenario::from_json(
        &text
            .replace(
                "\"advanced_declaration_lead_s\": 600",
                "\"advanced_declaration_lead_s\": 0",
            )
            .replace("\"earliness_margin_s\": 300", "\"earliness_margin_s\": 0")
            .replace("\"lateness_margin_s\": 900", "\"lateness_margin_s\": 0"),
    )
    .expect("the same scenario without the declaration");

    let (declared_sim, declared_metrics, _) = run(&declared, 42);
    let (_, spot_metrics, _) = run(&on_the_spot, 42);

    // Declaring costs three of the twenty-five: a rider nobody claims never walks at all.
    assert_eq!(declared_metrics.passengers_served, 22);
    assert_eq!(spot_metrics.passengers_served, 25);
    assert!(
        declared_metrics.mean_wait_s < spot_metrics.mean_wait_s / 2.0,
        "a rider told when to walk barely waits: {} s declared against {} s on the spot",
        declared_metrics.mean_wait_s,
        spot_metrics.mean_wait_s
    );
    assert!(
        declared_sim
            .agents()
            .iter()
            .any(|agent| agent.claimed_by.is_some()),
        "and it got there by being taken in advance"
    );
}

/// `mixed-declaration.json` at full adoption **is** `advanced-declaration.json`.
///
/// The base scenario of the adoption sweep splits every cohort in two - one half declaring, one
/// half not - and the sweep moves the split. That only measures adoption if the ends of the axis
/// are the scenarios they claim to be: at 100% the plain halves are empty and what is left has to
/// agree with the committed full-adoption scenario agent for agent, including the order the
/// generator drew them in. A cohort ordering that quietly changed the draws would still produce a
/// full set of rows and a plausible curve.
#[test]
fn full_adoption_of_the_mixed_scenario_is_the_declaring_scenario() {
    let mut mixed = load("mixed-declaration.json");
    // Indices 0-3 are the declaring drivers, 4-7 the plain ones; 8-11 and 12-15 the same for
    // riders. Full adoption moves every count into the declaring half.
    let full: Vec<u32> = vec![4, 2, 2, 5, 0, 0, 0, 0, 10, 9, 4, 9, 0, 0, 0, 0];
    assert_eq!(mixed.cohorts.len(), full.len(), "sixteen cohorts");
    for (cohort, count) in mixed.cohorts.iter_mut().zip(&full) {
        cohort.count = *count;
    }

    let (_, mixed_metrics, mixed_ticks) = run(&mixed, 42);
    let (_, declaring_metrics, declaring_ticks) = run(&load("advanced-declaration.json"), 42);
    assert_eq!(mixed_ticks, declaring_ticks);
    assert_eq!(
        serde_json::to_value(&mixed_metrics).unwrap(),
        serde_json::to_value(&declaring_metrics).unwrap(),
        "full adoption is not the committed full-adoption scenario"
    );

    // And the other end, which every difference in the sweep is measured against: nobody
    // declaring is `baseline.json`, down to the last kilometre.
    let mut none = load("mixed-declaration.json");
    let empty: Vec<u32> = vec![0, 0, 0, 0, 4, 2, 2, 5, 0, 0, 0, 0, 10, 9, 4, 9];
    for (cohort, count) in none.cohorts.iter_mut().zip(&empty) {
        cohort.count = *count;
    }
    let (_, none_metrics, _) = run(&none, 42);
    let (_, baseline_metrics, _) = run(&load("baseline.json"), 42);
    assert_eq!(
        serde_json::to_value(&none_metrics).unwrap(),
        serde_json::to_value(&baseline_metrics).unwrap(),
        "nobody declaring is not the baseline"
    );
}

/// One declaring carpool driver, set off with the given departure noise: it declares 600 s ahead
/// of a departure 1200 s after it spawns.
fn noisy_driver(departure_noise_s: &str) -> Sim {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "noisy", "tick_s": 1, "max_time_s": 14400, {TWO_WAYS_IN},
          "cohorts": [{{
            "kind": "CarpoolDriver", "name": "the late driver", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 46.000, "longitude": 6.00 }},
            "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
            "departure_offset_s": 1200, "advanced_declaration_lead_s": 600,
            "departure_noise_s": {departure_noise_s},
            "max_wait_s": 0, "vehicle": {{ "capacity": 4 }}
          }}]
        }}"#
    ))
    .unwrap_or_else(|error| panic!("loading the noisy driver: {error:#}"));
    let mut sim = Sim::new(&scenario, 3, Router::new(&scenario));
    sim.run(&mut EventLog::discarding().unwrap()).unwrap();
    sim
}

/// Departure noise moves the leaving and nothing else: the operator was told a time, matches on the
/// window around that time, and the agent turns up late or early anyway.
#[test]
fn departure_noise_moves_the_leaving_and_not_the_declaration() {
    let on_time = noisy_driver("0");
    assert_eq!(on_time.agents()[0].departed_s, Some(1200.0));

    let late = noisy_driver("300");
    let driver = &late.agents()[0];
    assert_eq!(driver.departed_s, Some(1500.0), "declared 1200, 300 s late");
    assert_eq!(
        driver.departure_s, 1200.0,
        "the declared time does not move"
    );
    assert_eq!(driver.window.earliest_s, 1200.0, "nor does the window");
    assert_eq!(
        driver.declared_s,
        Some(0.0),
        "it registers when it spawns, as ever"
    );

    // Early by more than its lead: it cannot leave before it spawned, so the lead is the floor.
    let early = noisy_driver("-900");
    assert_eq!(early.agents()[0].departed_s, Some(600.0));
}

/// A fleet taxi has no trip to be late for.
#[test]
fn departure_noise_on_a_fleet_taxi_is_a_load_error() {
    let error = Scenario::from_json(
        r#"{
          "name": "broken",
          "environment": { "stations": [{ "name": "Depot", "latitude": 46.0, "longitude": 6.0 }] },
          "cohorts": [{
            "kind": "AutonomousTaxi", "name": "fleet", "count": 1,
            "spawn_window": { "start_s": 0, "end_s": 0 },
            "origin": { "latitude": 46.0, "longitude": 6.0 },
            "departure_noise_s": { "normal": { "mean": 0, "sd": 60 } },
            "vehicle": { "capacity": 4 }
          }]
        }"#,
    )
    .expect_err("noise on a fleet taxi is rejected");
    assert!(
        format!("{error:#}").contains("no trip of its own"),
        "{error:#}"
    );
}

/// A declaring driver and a declaring rider for Cathedral Square, both departing at 1200 s and
/// declaring 600 s ahead, with half an hour of lateness each. The rider walks about 1070 s to
/// Ashford from 1.5 km south of it; the driver starts `driver_south_km` south of Ashford.
fn timed_pair(driver_times: bool, rider_times: bool, driver_south_km: f64) -> Sim {
    let driver_latitude = 46.000 - driver_south_km / 111.32;
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "timed", "tick_s": 1, "max_time_s": 14400, {TWO_WAYS_IN},
          "cohorts": [
            {{
              "kind": "CarpoolDriver", "name": "driver", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": {driver_latitude}, "longitude": 6.00 }},
              "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
              "departure_offset_s": 1200, "advanced_declaration_lead_s": 600,
              "lateness_margin_s": 1800, "times_to_counterpart": {driver_times},
              "max_wait_s": 1800, "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "rider", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.98653, "longitude": 6.00 }},
              "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
              "departure_offset_s": 1200, "advanced_declaration_lead_s": 600,
              "lateness_margin_s": 1800, "times_to_counterpart": {rider_times},
              "max_wait_s": 1800
            }}
          ]
        }}"#
    ))
    .unwrap_or_else(|error| panic!("loading the timed pair: {error:#}"));
    let mut sim = Sim::new(&scenario, 3, Router::new(&scenario));
    let summary = sim.run(&mut EventLog::discarding().unwrap()).unwrap();
    assert!(!summary.stopped_by_clock);
    sim
}

fn driver(sim: &Sim) -> &Agent {
    sim.agents()
        .iter()
        .find(|agent| agent.kind.is_driver())
        .unwrap()
}

fn rider(sim: &Sim) -> &Agent {
    sim.agents()
        .iter()
        .find(|agent| agent.kind.is_rider())
        .unwrap()
}

/// A driver timing itself to the rider it claimed leaves so as to reach the station when the rider
/// does, instead of arriving on time and waiting out the rider's walk there - which is where an
/// untimed driver spends the rider's quarter of an hour on foot.
#[test]
fn a_driver_timing_to_its_rider_leaves_to_meet_it() {
    let untimed = timed_pair(false, false, 0.0);
    let timed = timed_pair(true, false, 0.0);
    for sim in [&untimed, &timed] {
        assert!(rider(sim).arrived(), "the rider is carried either way");
    }
    assert_eq!(driver(&untimed).departed_s, Some(1200.0));
    let rider_there_s = rider(&untimed).departed_s.unwrap() + 1000.0;
    let left_s = driver(&timed).departed_s.unwrap();
    assert!(
        left_s > rider_there_s,
        "it waited for the rider's walk at home, leaving at {left_s} s"
    );
    assert!(
        rider(&timed).wait_s.unwrap() <= 5.0,
        "and the rider finds it there: waited {:?} s",
        rider(&timed).wait_s
    );
}

/// A rider timing itself to the driver that claimed it leaves so as to reach the station when the
/// driver does - later than its own declared time when the driver has further to come.
#[test]
fn a_rider_timing_to_its_driver_leaves_to_meet_it() {
    let untimed = timed_pair(false, false, 10.0);
    let timed = timed_pair(false, true, 10.0);
    assert_eq!(rider(&untimed).departed_s, Some(1200.0));
    let left_s = rider(&timed).departed_s.unwrap();
    assert!(
        left_s > 1250.0 && left_s < 1500.0,
        "10 km of driving against 1.5 km of walking puts it about 100 s late, left at {left_s} s"
    );
    assert!(rider(&timed).arrived());
}

/// Timing a leaving to an advance match needs an advance match to time it to.
#[test]
fn timing_to_the_counterpart_without_a_lead_is_a_load_error() {
    let error = Scenario::from_json(&format!(
        r#"{{
          "name": "broken", {TWO_WAYS_IN},
          "cohorts": [{{
            "kind": "CarpoolRider", "name": "rider", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 46.0, "longitude": 6.0 }},
            "destination": {{ "latitude": 46.27, "longitude": 6.0 }},
            "times_to_counterpart": true
          }}]
        }}"#
    ))
    .expect_err("no lead, nothing to time to");
    assert!(
        format!("{error:#}").contains("advanced_declaration_lead_s"),
        "{error:#}"
    );
}

/// A rider that declared ahead and was never claimed - the only driver does not declare - sets off
/// at its time anyway if it walks unclaimed, and is picked up at the station; otherwise it waits
/// at home for a claim that never comes.
#[test]
fn an_unclaimed_declaring_rider_that_walks_anyway_is_picked_up_at_the_station() {
    let served = |walks: bool| {
        let scenario = Scenario::from_json(&format!(
            r#"{{
              "name": "unclaimed", "tick_s": 1, "max_time_s": 14400, {TWO_WAYS_IN},
              "cohorts": [
                {{
                  "kind": "CarpoolDriver", "name": "driver", "count": 1,
                  "spawn_window": {{ "start_s": 1500, "end_s": 1500 }},
                  "origin": {{ "latitude": 46.000, "longitude": 6.00 }},
                  "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
                  "max_wait_s": 600, "vehicle": {{ "capacity": 4 }}
                }},
                {{
                  "kind": "CarpoolRider", "name": "declaring rider", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 46.000, "longitude": 6.00 }},
                  "destination": {{ "latitude": 46.270, "longitude": 6.00 }},
                  "departure_offset_s": 1200, "advanced_declaration_lead_s": 600,
                  "walks_unclaimed": {walks}, "max_wait_s": 3600
                }}
              ]
            }}"#
        ))
        .unwrap();
        run(&scenario, 1).1.passengers_served
    };
    assert_eq!(served(false), 0);
    assert_eq!(served(true), 1);
}
