//! Carpool matching end to end: the operator's line query, and a rider carried from its origin
//! to its destination.

use mas::agents::{State, TransitionReason};
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::operator::Operator;
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use mas::world::{angle_between_degrees, bearing_degrees, Coord, World};
use std::path::Path;

fn load(name: &str) -> Scenario {
    Scenario::load(Path::new("scenarios").join(name).as_path())
        .unwrap_or_else(|error| panic!("loading {name}: {error:#}"))
}

/// One driver, one rider, one line: the rider must be picked up, carried and set down at its own
/// destination, and the metrics must say so.
#[test]
fn the_minimal_scenario_serves_its_one_rider() {
    let scenario = load("minimal.json");
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, 42, Router::new(&scenario));
    let summary = sim.run(&mut log).unwrap();

    assert!(!summary.stopped_by_clock, "the run should end on its own");
    assert_eq!(summary.stranded, 0);

    let metrics = Metrics::collect(sim.agents(), &summary, &NoIncentive);
    assert_eq!(metrics.passengers_served, 1);
    assert_eq!(metrics.drivers_active, 1);
    assert_eq!(metrics.service_rate, 1.0);

    let rider = sim
        .agents()
        .iter()
        .find(|agent| agent.kind.is_rider())
        .expect("the scenario declares a rider");
    let driver = sim
        .agents()
        .iter()
        .find(|agent| agent.kind.is_driver())
        .expect("the scenario declares a driver");

    assert_eq!(rider.state, State::EndJourney);
    assert_eq!(rider.reason, TransitionReason::ArrivedAtDestination);
    assert_eq!(driver.pickups, 1);
    assert_eq!(rider.line, driver.line, "both matched onto the same line");

    // Arrival strictly after departure, and after the wait that preceded it.
    let wait_s = rider.wait_s.expect("the rider was picked up");
    assert!(wait_s > 0.0, "the rider waited for the driver to arrive");
    assert!(
        rider.state_since_s > rider.departure_s + wait_s,
        "arrival at {} s must follow departure at {} s plus a {wait_s} s wait",
        rider.state_since_s,
        rider.departure_s
    );

    // Carried kilometres are real road kilometres, and every one of them is loaded for the
    // driver too: they are the same kilometres seen from the two ends of the same ride.
    assert!(rider.loaded_km > 0.0);
    assert!((metrics.passenger_km - metrics.vehicle_km_loaded).abs() < 1e-9);
    assert!(
        metrics.vehicle_km_empty > 0.0,
        "the driver drove to the station and away from it with nobody aboard"
    );
    assert!(
        (metrics.vehicle_km_total - (metrics.vehicle_km_loaded + metrics.vehicle_km_empty)).abs()
            < 1e-9
    );
}

/// The operator ranks lines by travel time, so the line that is shorter in kilometres loses when
/// reaching it means a long walk. Standing at Borth, the Glan Rheidol line is by far the shorter
/// drive - a mile and a bit into Aberystwyth - and the Borth line is still the faster trip.
#[test]
fn the_operator_picks_the_line_that_takes_the_least_time() {
    let scenario = load("baseline.json");
    let router = Router::new(&scenario);
    let world = World::from_environment(&scenario.environment, &router);
    let operator = Operator::new();

    let station = |name: &str| {
        world
            .stations()
            .iter()
            .find(|s| s.name == name)
            .expect("declared")
            .coord
    };
    let borth = station("Borth");
    let glan_rheidol = station("Glan Rheidol");
    let aberystwyth = station("Aberystwyth");

    let line_from = |name: &str| {
        world
            .lines()
            .iter()
            .find(|line| world.station(line.origin).name == name)
            .expect("declared")
    };
    assert!(
        line_from("Glan Rheidol").route.distance_km < line_from("Borth").route.distance_km,
        "the premise of this test: the Glan Rheidol line is the shorter drive"
    );

    let ranked = operator.ranked_lines(&world, &router, borth, aberystwyth);
    assert_eq!(ranked.len(), world.lines().len(), "every line is offered");
    assert!(
        ranked
            .windows(2)
            .all(|pair| pair[0].cost_s <= pair[1].cost_s),
        "quickest first"
    );
    let best = ranked[0];
    assert_eq!(
        world.station(world.line(best.line).origin).name,
        "Borth",
        "the ten-kilometre walk down the coast costs more time than the shorter drive saves"
    );
    assert_eq!(
        best.walk_s, 0.0,
        "the trip starts and ends on the line's own stations"
    );
    assert!((best.cost_s - world.line(best.line).route.duration_s).abs() < 1e-9);

    // Standing at Glan Rheidol instead, the line that starts there costs nothing to reach and is
    // the shorter drive, so it wins on both terms.
    let from_glan_rheidol = operator.ranked_lines(&world, &router, glan_rheidol, aberystwyth)[0];
    assert_eq!(
        world
            .station(world.line(from_glan_rheidol.line).origin)
            .name,
        "Glan Rheidol"
    );
}

/// The walk refusal is the rider's: a line it would have to walk too far to reach is one it
/// declines, and a rider left with none of them never registers with the operator.
#[test]
fn a_rider_declines_a_line_it_would_have_to_walk_too_far_to_reach() {
    // The one station is eight kilometres west of the rider - a hundred minutes on foot - and the
    // rider will walk five.
    let text = r#"{
      "name": "walk-refusal",
      "max_time_s": 600,
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
      },
      "cohorts": [{
        "kind": "CarpoolRider",
        "name": "the unwilling walker",
        "count": 1,
        "spawn_window": { "start_s": 0, "end_s": 0 },
        "origin": { "latitude": 45.00, "longitude": 4.90 },
        "destination": { "latitude": 45.27, "longitude": 5.00 },
        "max_walk_s": 300
      }]
    }"#;
    let scenario = Scenario::from_json(text).expect("valid scenario");
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, 42, Router::new(&scenario));
    let summary = sim.run(&mut log).unwrap();

    assert!(
        !summary.stopped_by_clock,
        "the refusal ends the run at once"
    );
    assert_eq!(summary.canceled, 1);

    let rider = &sim.agents()[0];
    assert_eq!(rider.state, State::Canceled);
    assert_eq!(rider.reason, TransitionReason::NoLineAvailable);
    assert_eq!(rider.line, None, "it never took a line");
    assert_eq!(sim.operator().registered_riders(), 0);
    assert_eq!(
        Metrics::collect(sim.agents(), &summary, &NoIncentive).passengers_served,
        0
    );

    // The same rider willing to walk the distance does register, so the refusal is the threshold
    // and not the world.
    let patient = Scenario::from_json(&text.replace("\"max_walk_s\": 300", "\"max_walk_s\": 9000"))
        .expect("valid scenario");
    let mut sim = Sim::new(&patient, 42, Router::new(&patient));
    sim.run(&mut EventLog::discarding().unwrap()).unwrap();
    assert!(
        sim.agents()[0].line.is_some(),
        "a rider willing to walk takes the line"
    );
}

/// Bearings, and the 45-degree cone the direction filter is built on. A sign error here is
/// silent and would quietly stop every match.
#[test]
fn bearings_measure_the_way_an_agent_is_heading() {
    let here = Coord::new(45.00, 5.00);
    let north = Coord::new(45.10, 5.00);
    let east = Coord::new(45.00, 5.10);
    let south = Coord::new(44.90, 5.00);

    assert!((bearing_degrees(here, north) - 0.0).abs() < 1e-6);
    assert!((bearing_degrees(here, east) - 90.0).abs() < 0.1);
    assert!((bearing_degrees(here, south) - 180.0).abs() < 1e-6);

    assert!((angle_between_degrees(0.0, 350.0) - 10.0).abs() < 1e-9);
    assert!((angle_between_degrees(350.0, 0.0) - 10.0).abs() < 1e-9);
    assert!((angle_between_degrees(10.0, 200.0) - 170.0).abs() < 1e-9);
    assert!(angle_between_degrees(0.0, 180.0) <= 180.0);
}
