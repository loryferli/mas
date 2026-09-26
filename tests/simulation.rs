//! The tick loop and the geometry it moves agents with.

use mas::agents::{State, TransitionReason};
use mas::events::EventLog;
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use mas::world::{haversine_km, Coord, Journey};

/// Three points up a meridian, ten kilometres apart, walked at a known speed.
#[test]
fn movement_along_a_polyline_lands_where_the_distance_says() {
    let start = Coord::new(45.00, 5.00);
    let middle = Coord::new(45.09, 5.00);
    let end = Coord::new(45.18, 5.00);

    let first_leg_km = haversine_km(start, middle);
    let second_leg_km = haversine_km(middle, end);

    const SPEED_MPS: f64 = 10.0;
    const TICK_S: f64 = 60.0;
    let per_tick_km = SPEED_MPS * TICK_S / 1000.0;

    let mut journey = Journey::new(vec![start, middle, end], SPEED_MPS);
    assert_eq!(journey.position(), start);
    assert!(!journey.finished());

    // Twenty-five ticks at 0.6 km each: past the middle point and partway along the second leg.
    let ticks = 25.0;
    for _ in 0..25 {
        journey.advance(TICK_S);
    }
    let expected_km = ticks * per_tick_km;
    assert!(
        (journey.traveled_km() - expected_km).abs() < 1e-9,
        "expected {expected_km} km travelled, got {}",
        journey.traveled_km()
    );

    let into_second_leg = expected_km - first_leg_km;
    assert!(
        into_second_leg > 0.0 && into_second_leg < second_leg_km,
        "the test intends to stop partway along the second leg"
    );
    let expected_latitude =
        middle.latitude + (end.latitude - middle.latitude) * (into_second_leg / second_leg_km);
    assert!(
        (journey.position().latitude - expected_latitude).abs() < 1e-9,
        "expected latitude {expected_latitude}, got {}",
        journey.position().latitude
    );
    assert!((journey.position().longitude - 5.00).abs() < 1e-12);

    // Overrunning the end stops at the end and never travels further than the polyline is long.
    for _ in 0..100 {
        journey.advance(TICK_S);
    }
    assert!(journey.finished());
    assert_eq!(journey.position(), end);
    assert!((journey.traveled_km() - (first_leg_km + second_leg_km)).abs() < 1e-9);
    assert_eq!(journey.advance(TICK_S), 0.0, "a finished journey stays put");
}

const TWO_STATIONS: &str = r#"
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

#[test]
fn a_run_with_no_agents_terminates_immediately() {
    let scenario = Scenario::from_json(&format!(
        r#"{{ "name": "empty", "tick_s": 1, "max_time_s": 3600, {TWO_STATIONS}, "cohorts": [] }}"#
    ))
    .unwrap();

    let mut log = EventLog::discarding().unwrap();
    let summary = Sim::new(&scenario, 7, Router::new(&scenario))
        .run(&mut log)
        .unwrap();

    assert_eq!(summary.ticks, 0, "nothing to simulate, so no ticks");
    assert_eq!(summary.agents_spawned, 0);
    assert_eq!(summary.events, 0);
    assert!(!summary.stopped_by_clock, "the clock cap was not reached");
}

/// Ghost riders walk to a station, wait, and give up - so a run of only ghost riders ends on its
/// own well before the clock cap.
#[test]
fn ghost_riders_give_up_and_the_run_ends_on_its_own() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "ghosts", "tick_s": 1, "max_time_s": 7200, {TWO_STATIONS},
          "cohorts": [{{
            "kind": "GhostRider", "name": "walk-up riders", "count": 3,
            "spawn_window": {{ "start_s": 0, "end_s": 60 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00, "jitter_radius_km": 0.2 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "max_wait_s": 300
          }}]
        }}"#
    ))
    .unwrap();

    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, 7, Router::new(&scenario));
    let summary = sim.run(&mut log).unwrap();

    assert!(!summary.stopped_by_clock, "the run should end on its own");
    assert_eq!(summary.agents_spawned, 3);
    assert_eq!(summary.canceled, 3);
    // Spawn window, a short walk, then the wait: nowhere near the two-hour cap.
    assert!(
        sim.time_s() < 1200.0,
        "expected the run to end shortly after the wait ran out, got {} s",
        sim.time_s()
    );

    for agent in sim.agents() {
        assert_eq!(agent.state, State::Canceled);
        assert_eq!(agent.reason, TransitionReason::WaitTimedOut);
        assert!(
            agent.distance_km > 0.0,
            "every rider walked to the station, so some distance is on the clock"
        );
        // Every rider started within the jitter radius and ended standing at the station.
        assert_eq!(agent.position(), Coord::new(45.00, 5.00));
    }
}

/// The seed is the only source of variation: same scenario, same seed, same run.
#[test]
fn the_same_seed_produces_the_same_run() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "ghosts", "tick_s": 1, "max_time_s": 7200, {TWO_STATIONS},
          "cohorts": [{{
            "kind": "GhostRider", "name": "walk-up riders", "count": 5,
            "spawn_window": {{ "start_s": 0, "end_s": 600 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00, "jitter_radius_km": 1 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00, "jitter_radius_km": 1 }},
            "max_wait_s": 300
          }}]
        }}"#
    ))
    .unwrap();

    let trace = |seed: u64| {
        let mut log = EventLog::discarding().unwrap();
        let mut sim = Sim::new(&scenario, seed, Router::new(&scenario));
        let summary = sim.run(&mut log).unwrap();
        let agents: Vec<(f64, f64)> = sim
            .agents()
            .iter()
            .map(|agent| (agent.spawn_s, agent.distance_km))
            .collect();
        (summary.ticks, agents)
    };

    assert_eq!(trace(11), trace(11), "same seed, same run");
    assert_ne!(
        trace(11).1,
        trace(12).1,
        "a different seed should scatter the riders differently"
    );
}

/// Every event row carries the seats in use in the agent's vehicle and how far it has come, so a
/// time series of occupancy and of loaded and empty kilometres is read off the log. A rider has no
/// vehicle and leaves the seats empty; nobody's distance ever goes down.
#[test]
fn every_event_row_carries_seats_used_and_distance_so_far() {
    let scenario =
        mas::scenario::Scenario::load(std::path::Path::new("scenarios/minimal.json")).unwrap();
    let path = std::env::temp_dir().join("mas-event-columns.csv");
    let mut log = mas::events::EventLog::to_file(&path).unwrap();
    mas::sim::Sim::new(&scenario, 42, mas::routing::Router::new(&scenario))
        .run(&mut log)
        .unwrap();
    drop(log);
    let mut reader = csv::Reader::from_path(&path).unwrap();
    let header = reader.headers().unwrap().clone();
    let column = |name: &str| header.iter().position(|h| h == name).unwrap();
    let (kind, agent, seats, distance) = (
        column("kind"),
        column("agent_id"),
        column("seats_used"),
        column("distance_km"),
    );
    let mut last_km = std::collections::HashMap::new();
    let mut drivers_with_seats = 0;
    for row in reader.records() {
        let row = row.unwrap();
        let km: f64 = row[distance].parse().unwrap();
        let previous = last_km.insert(row[agent].to_string(), km).unwrap_or(0.0);
        assert!(
            km >= previous,
            "distance went down for agent {}",
            &row[agent]
        );
        match row[kind].ends_with("Rider") {
            true => assert_eq!(&row[seats], "", "a rider has no vehicle"),
            false => {
                assert!(
                    row[seats].parse::<u32>().unwrap() >= 1,
                    "the driver sits in its own car"
                );
                drivers_with_seats += 1;
            }
        }
    }
    assert!(drivers_with_seats > 0);
}
