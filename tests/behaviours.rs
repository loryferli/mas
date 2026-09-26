//! The driver behaviours beyond the carpool one: the ghost that is not registered, the
//! neglectful one that never carries anyone, and the vehicle the departure guarantee dispatches.
//! Plus the kinds whose behaviour is not written yet, which must stay visible rather than quietly
//! doing nothing.

use mas::agents::{Agent, State, TransitionReason};
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::routing::Router;
use mas::scenario::{AgentKind, Scenario, Value};
use mas::sim::{RunSummary, Sim};
use std::path::Path;

/// The committed corridor's two ends, thirty kilometres apart up a meridian.
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

fn run(scenario: &Scenario, seed: u64) -> (Sim, Metrics, RunSummary) {
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(scenario, seed, Router::new(scenario));
    let summary = sim.run(&mut log).unwrap();
    let metrics = Metrics::collect(sim.agents(), &summary, &NoIncentive);
    (sim, metrics, summary)
}

fn of_kind(sim: &Sim, kind: AgentKind) -> Vec<&Agent> {
    sim.agents().iter().filter(|a| a.kind == kind).collect()
}

/// A driver that is registered and never carries anyone is still supply as far as the operator is
/// concerned: it takes a line, stands at the station and drives off empty. Nobody is served.
#[test]
fn a_fleet_of_neglectful_drivers_serves_nobody() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "neglect", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
          "cohorts": [
            {{
              "kind": "NeglectfulDriver", "name": "signed up but never carrying", "count": 2,
              "spawn_window": {{ "start_s": 0, "end_s": 60 }},
              "origin": {{ "latitude": 45.00, "longitude": 4.95 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.05 }},
              "max_wait_s": 300, "vehicle": {{ "capacity": 5 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "the hopeful", "count": 2,
              "spawn_window": {{ "start_s": 0, "end_s": 60 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
              "max_wait_s": 600
            }}
          ]
        }}"#
    ))
    .unwrap();

    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(!summary.stopped_by_clock, "the run should end on its own");
    assert_eq!(metrics.passengers_served, 0);
    assert_eq!(metrics.drivers_active, 0);
    assert_eq!(metrics.passenger_km, 0.0);
    assert!(
        metrics.vehicle_km_total > 0.0,
        "the vehicles still drove the line, which is what makes the empty share the story"
    );
    assert_eq!(metrics.empty_distance_share, 1.0);

    for driver in of_kind(&sim, AgentKind::NeglectfulDriver) {
        assert!(
            driver.line.is_some(),
            "a neglectful driver registers - that is what separates it from a ghost"
        );
        assert_eq!(driver.pickups, 0);
        assert_eq!(driver.state, State::EndJourney);
        assert_eq!(
            driver.station, None,
            "it drove the trip it was making anyway and never went to a station"
        );
    }
    for rider in of_kind(&sim, AgentKind::CarpoolRider) {
        assert_eq!(rider.state, State::Canceled);
        assert_eq!(rider.reason, TransitionReason::WaitTimedOut);
    }
}

/// What a ghost withholds is the registration, not the operator's ranking: it takes a line like
/// anyone else and simply never tells anybody, so no query the operator answers can offer it. The
/// consequence is behavioural, and this pins it from both sides - a registered driver standing at
/// the same station cannot see a ghost rider, and a ghost driver, which looks at who is actually
/// there, carries it.
#[test]
fn a_ghost_is_invisible_to_the_registry_and_visible_at_the_station() {
    let scenario = |driver_kind: &str| {
        Scenario::from_json(&format!(
            r#"{{
              "name": "ghosts-meet", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
              "cohorts": [
                {{
                  "kind": "{driver_kind}", "name": "the driver", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 45.00, "longitude": 4.95 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.05 }},
                  "departure_offset_s": 60, "max_wait_s": 1800, "vehicle": {{ "capacity": 5 }}
                }},
                {{
                  "kind": "GhostRider", "name": "walk-up riders", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                  "max_wait_s": 1800
                }}
              ]
            }}"#
        ))
        .unwrap()
    };

    // A registered driver is offered the registry, and a ghost rider is not in it: they stand at
    // the same station for half an hour and nothing happens.
    let (blind, blind_metrics, _) = run(&scenario("CarpoolDriver"), 42);
    assert_eq!(blind_metrics.passengers_served, 0);
    let unseen = of_kind(&blind, AgentKind::GhostRider).remove(0);
    assert_eq!(unseen.state, State::Canceled);
    assert_eq!(unseen.reason, TransitionReason::WaitTimedOut);
    assert_eq!(
        unseen.line,
        of_kind(&blind, AgentKind::CarpoolDriver).remove(0).line,
        "the premise: the same line, so the same station to stand at"
    );

    // A ghost driver looks at who is there instead, so the same pair travels.
    let (sim, metrics, summary) = run(&scenario("GhostDriver"), 42);
    assert!(!summary.stopped_by_clock, "the run should end on its own");
    assert_eq!(metrics.passengers_served, 1);
    assert_eq!(metrics.drivers_active, 1);

    let driver = of_kind(&sim, AgentKind::GhostDriver).remove(0);
    let rider = of_kind(&sim, AgentKind::GhostRider).remove(0);
    assert_eq!(driver.pickups, 1);
    assert_eq!(
        driver.line, rider.line,
        "both read the same public ranking and took the same line"
    );
    assert!(driver.line.is_some(), "a ghost still has a line to drive");
    assert_eq!(rider.reason, TransitionReason::ArrivedAtDestination);
    // The two sides of the same ride, from the rider's seat and from the driver's odometer.
    assert!((rider.loaded_km - driver.loaded_km).abs() < 1e-9);
    assert!(rider.loaded_km > 0.0);
}

/// The rider's refusal is the rider's, and it happens before it sets off: a rider no line leaves
/// within walking distance of its destination never registers and never walks, so no driver - ghost
/// or otherwise - can carry it thirty kilometres past where it was going.
#[test]
fn a_rider_no_line_serves_never_sets_off() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "wrong-way", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
          "cohorts": [
            {{
              "kind": "GhostDriver", "name": "unregistered drivers", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 4.95 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.05 }},
              "departure_offset_s": 60, "max_wait_s": 600, "vehicle": {{ "capacity": 5 }}
            }},
            {{
              "kind": "GhostRider", "name": "going nowhere much", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.001, "longitude": 5.00 }},
              "max_wait_s": 600
            }}
          ]
        }}"#
    ))
    .unwrap();

    let (sim, metrics, _) = run(&scenario, 42);

    assert_eq!(metrics.passengers_served, 0);
    let driver = of_kind(&sim, AgentKind::GhostDriver).remove(0);
    let rider = of_kind(&sim, AgentKind::GhostRider).remove(0);
    assert_eq!(driver.pickups, 0);
    assert_eq!(rider.state, State::Canceled);
    assert_eq!(rider.reason, TransitionReason::NoLineAvailable);
    assert_eq!(rider.distance_km, 0.0, "it never even walked to a station");
    assert!(rider.line.is_none(), "and never took a line");
}

/// The driver's refusal is the driver's: `max_detour_pct` is what lets it decline a line the
/// operator ranked first, and a driver that declines every line drives the trip it was making
/// anyway rather than disappearing off the road.
#[test]
fn a_driver_that_will_not_detour_drives_its_own_trip_and_carries_nobody() {
    // The committed corridor's station dogleg is about 126% of driving direct, so a tenth is not
    // enough and a third is.
    let scenario = |limit_pct: u32| {
        Scenario::from_json(&format!(
            r#"{{
              "name": "detour", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
              "cohorts": [
                {{
                  "kind": "CarpoolDriver", "name": "the driver", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 45.00, "longitude": 4.95 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.05 }},
                  "departure_offset_s": 60, "max_detour_pct": {limit_pct},
                  "vehicle": {{ "capacity": 5 }}
                }},
                {{
                  "kind": "CarpoolRider", "name": "the rider", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                  "max_wait_s": 1800
                }}
              ]
            }}"#
        ))
        .unwrap()
    };

    let (refused, refused_metrics, _) = run(&scenario(110), 42);
    let driver = of_kind(&refused, AgentKind::CarpoolDriver).remove(0);
    assert_eq!(refused_metrics.passengers_served, 0);
    assert_eq!(driver.pickups, 0);
    assert!(driver.line.is_none(), "it never took a line");
    assert_eq!(driver.reason, TransitionReason::ArrivedAtDestination);
    assert!(
        driver.distance_km > 0.0,
        "it still drove its own trip: {} km",
        driver.distance_km
    );
    assert_eq!(
        refused.operator().registered_drivers(),
        0,
        "and the operator was never told about a seat it could not have"
    );

    // The same driver with room for a third more takes the line and serves the rider.
    let (_, accepted, _) = run(&scenario(135), 42);
    assert_eq!(accepted.passengers_served, 1);
}

/// One rider, no fleet at all, and a line that promises a vehicle after twenty minutes: exactly
/// one guarantee vehicle is dispatched, and it serves the rider.
#[test]
fn the_departure_guarantee_dispatches_one_vehicle_for_one_stranded_rider() {
    let scenario = Scenario::load(Path::new("scenarios/departure-guarantee.json")).unwrap();
    let guarantee = scenario.environment.networks[0].lines[0]
        .departure_guarantee
        .expect("the scenario is the one that declares a guarantee");

    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(!summary.stopped_by_clock, "the run should end on its own");
    assert_eq!(metrics.passengers_served, 1);

    let taxis = of_kind(&sim, AgentKind::TaxiDriver);
    assert_eq!(taxis.len(), 1, "one trigger, one vehicle");
    let taxi = taxis[0];
    assert_eq!(taxi.pickups, 1);
    assert_eq!(taxi.reason, TransitionReason::ArrivedAtDestination);
    assert_eq!(
        summary.agents_spawned, 2,
        "the cohort's rider plus the vehicle the operator called"
    );

    let rider = of_kind(&sim, AgentKind::CarpoolRider).remove(0);
    assert_eq!(rider.reason, TransitionReason::ArrivedAtDestination);
    // Dispatched on the trigger, not before it, and the rider waited at least that long.
    let waited_s = rider.wait_s.expect("the guarantee vehicle picked it up");
    assert!(
        waited_s >= guarantee.trigger_after_wait_s,
        "expected a wait past the {} s trigger, got {waited_s} s",
        guarantee.trigger_after_wait_s
    );
    assert!(taxi.spawn_s >= guarantee.trigger_after_wait_s);
}

/// A guarantee vehicle whose rider gives up while it is still on its way has nothing to carry and
/// no trip of its own. It stands down at the station rather than driving the line empty, which
/// would charge the service thirty-odd kilometres for a ride nobody took.
#[test]
fn a_guarantee_vehicle_that_finds_nobody_stands_down_instead_of_driving_the_line() {
    let mut scenario = Scenario::load(Path::new("scenarios/departure-guarantee.json")).unwrap();
    let guarantee = scenario.environment.networks[0].lines[0]
        .departure_guarantee
        .expect("the scenario is the one that declares a guarantee");
    // Patience that runs out after the trigger fires but before the vehicle can reach the station.
    scenario.cohorts[0].max_wait_s = Value::Fixed(guarantee.trigger_after_wait_s + 50.0);

    let (sim, metrics, _) = run(&scenario, 42);

    assert_eq!(metrics.passengers_served, 0);
    let taxi = of_kind(&sim, AgentKind::TaxiDriver).remove(0);
    assert_eq!(taxi.pickups, 0);
    assert_eq!(taxi.state, State::EndJourney);
    assert_eq!(taxi.reason, TransitionReason::WaitTimedOut);
    // The approach from the depot, and not a metre of the twenty-kilometre line behind it. The
    // depot sits two kilometres up the road from the stop, so a fifth of the line is a generous
    // ceiling on the approach and still nowhere near driving the line.
    let line_km = sim
        .world()
        .line(taxi.line.expect("dispatched onto a line"))
        .route
        .distance_km;
    assert!(
        taxi.distance_km < line_km / 5.0,
        "expected the depot approach only, got {} km against a {line_km} km line",
        taxi.distance_km
    );
    assert_eq!(
        sim.operator().registered_drivers(),
        0,
        "and it left the registry"
    );
}

/// Every kind in the mixed scenario now has a behaviour, so it runs to completion with nobody
/// stranded - and each pre-formed group's passenger rides with its own driver.
#[test]
fn the_mixed_scenario_runs_to_completion_and_groups_ride_together() {
    let scenario = Scenario::load(Path::new("scenarios/mixed-behaviours.json")).unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(!summary.stopped_by_clock, "nothing is left doing nothing");
    assert_eq!(metrics.stranded, 0);
    for passenger in of_kind(&sim, AgentKind::PolynomialRider) {
        if let Some(driver) = passenger.carried_by {
            let driver = &sim.agents()[driver.0 as usize];
            assert_eq!(driver.kind, AgentKind::PolynomialDriver);
            assert_eq!(driver.group, passenger.group, "it rode with its own driver");
        }
    }
    for driver in of_kind(&sim, AgentKind::PolynomialDriver) {
        for rider in sim
            .agents()
            .iter()
            .filter(|r| r.carried_by == Some(driver.id))
        {
            assert_eq!(
                rider.group, driver.group,
                "a group's driver carries nobody else"
            );
        }
    }
}

/// The base driver on its own is the private car: no line, no station, no registration, nobody
/// picked up - only its own party carried door to door. A world with no lines is what a scenario of
/// nothing but private cars means, so it loads.
#[test]
fn a_private_driver_drives_its_own_trip_with_its_own_party() {
    let scenario = Scenario::from_json(
        r#"{
          "name": "private cars", "tick_s": 1, "max_time_s": 14400,
          "environment": {
            "stations": [{ "name": "Northgate Park and Ride", "latitude": 45.00, "longitude": 5.00 }]
          },
          "cohorts": [{
            "kind": "PrivateDriver", "name": "commuters", "count": 40,
            "spawn_window": { "start_s": 0, "end_s": 600 },
            "origin": { "latitude": 45.00, "longitude": 5.00, "jitter_radius_km": 2 },
            "destination": { "latitude": 45.27, "longitude": 5.00, "jitter_radius_km": 2 },
            "additional_passengers": { "bernoulli": { "p": 0.5 } },
            "vehicle": { "capacity": 5 }
          }]
        }"#,
    )
    .unwrap();

    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(!summary.stopped_by_clock, "the run should end on its own");
    let cars = of_kind(&sim, AgentKind::PrivateDriver);
    assert_eq!(cars.len(), 40);
    for car in &cars {
        assert!(car.arrived(), "{:?} ended {:?}", car.id, car.state);
        assert_eq!(car.line, None, "a private car takes no line");
        assert_eq!(car.pickups, 0);
        let km = car.distance_km;
        assert!((20.0..45.0).contains(&km), "{km} km for a 30 km trip");
    }
    assert!(cars.iter().any(|car| car.seats == 2) && cars.iter().any(|car| car.seats == 1));
    assert_eq!(sim.operator().registered_drivers(), 0);
    assert_eq!(metrics.drivers_total, 40);
    assert_eq!(metrics.passengers_served, 0);
    assert_eq!(
        metrics.empty_distance_share, 1.0,
        "a companion is the driver's own party, not a rider the service carried"
    );
}

/// A private car is offered no line, so a detour bound on one would be a knob that does nothing.
#[test]
fn a_private_driver_with_a_detour_bound_is_a_load_error() {
    let error = Scenario::from_json(&format!(
        r#"{{
          "name": "broken", {TWO_STATIONS},
          "cohorts": [{{
            "kind": "PrivateDriver", "name": "commuters", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "max_detour_pct": 110, "vehicle": {{ "capacity": 5 }}
          }}]
        }}"#
    ))
    .expect_err("a detour bound on a private car is rejected");
    assert!(format!("{error:#}").contains("takes no line"), "{error:#}");
}

/// One registered driver heading north through Northgate from `driver_origin_latitude`, with the
/// given station approach, and `riders` waiting riders at Northgate.
fn approaching(approach: &str, driver_origin_latitude: f64, riders: u32) -> (Sim, Metrics) {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "approach", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
          "cohorts": [
            {{
              "kind": "CarpoolDriver", "name": "commuter", "count": 1,
              "spawn_window": {{ "start_s": 600, "end_s": 600 }},
              "origin": {{ "latitude": {driver_origin_latitude}, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.30, "longitude": 5.00 }},
              "max_wait_s": 900, "station_approach": "{approach}",
              "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "waiting", "count": {riders},
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
              "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(!summary.stopped_by_clock, "the run should end on its own");
    (sim, metrics)
}

/// With nobody to collect, an on-demand driver never goes near the station: it drives its own
/// trip, still registered, and is at least as short as the driver that visits the station and
/// waits out its patience there.
#[test]
fn an_on_demand_driver_with_nobody_waiting_drives_straight_through() {
    let (sim, _) = approaching("on_demand", 44.95, 0);
    let driver = &of_kind(&sim, AgentKind::CarpoolDriver)[0];
    assert!(driver.arrived());
    assert_eq!(driver.station, None, "it never headed for a station");

    let (always, _) = approaching("always", 44.95, 0);
    let always_driver = &of_kind(&always, AgentKind::CarpoolDriver)[0];
    assert!(driver.distance_km <= always_driver.distance_km + 1e-9);
    assert!(
        driver.state_since_s + 800.0 < always_driver.state_since_s,
        "it arrives without waiting out 900 s at an empty station: {} against {}",
        driver.state_since_s,
        always_driver.state_since_s
    );
}

/// A rider waiting at a station ahead of it turns an on-demand driver in; it takes everyone who is
/// there and leaves at once rather than waiting for more.
#[test]
fn an_on_demand_driver_stops_for_riders_ahead_and_leaves_at_once() {
    let (sim, metrics) = approaching("on_demand", 44.95, 2);
    assert_eq!(metrics.passengers_served, 2);
    let driver = &of_kind(&sim, AgentKind::CarpoolDriver)[0];
    assert_eq!(driver.pickups, 2);
    let riders = of_kind(&sim, AgentKind::CarpoolRider);
    let boarded: Vec<f64> = riders
        .iter()
        .map(|rider| rider.wait_s.expect("picked up"))
        .collect();
    assert!(
        (boarded[0] - boarded[1]).abs() <= 1.0,
        "both boarded on consecutive ticks, not one after a wait: {boarded:?}"
    );
}

/// A station behind the driver is no use to anybody, even with a rider standing at it: the driver
/// does not turn back, and once its line is behind it the operator forgets it.
#[test]
fn an_on_demand_driver_never_turns_back_for_a_station_behind_it() {
    let (sim, metrics) = approaching("on_demand", 45.05, 1);
    assert_eq!(metrics.passengers_served, 0);
    let driver = &of_kind(&sim, AgentKind::CarpoolDriver)[0];
    assert!(driver.arrived());
    assert_eq!(driver.pickups, 0);
    assert_eq!(sim.operator().registered_drivers(), 0);
}

/// Only a driver the operator knows about has a station to approach.
#[test]
fn station_approach_on_an_unregistered_driver_is_a_load_error() {
    let error = Scenario::from_json(&format!(
        r#"{{
          "name": "broken", {TWO_STATIONS},
          "cohorts": [{{
            "kind": "GhostDriver", "name": "ghost", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "station_approach": "on_demand", "vehicle": {{ "capacity": 4 }}
          }}]
        }}"#
    ))
    .expect_err("an unregistered driver has no line's station to approach");
    assert!(format!("{error:#}").contains("not one"), "{error:#}");
}

/// Three stops up a meridian - Ashby, then Burton near the far end, then Carlow - with a line from
/// each to the next, a rider at Ashby for Burton and a rider at Burton for Carlow, and one driver
/// going from Ashby to Carlow.
fn chain(rechain: bool) -> (Sim, Metrics) {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "chain", "tick_s": 1, "max_time_s": 14400,
          "environment": {{
            "stations": [
              {{ "name": "Ashby", "latitude": 45.00, "longitude": 5.00 }},
              {{ "name": "Burton", "latitude": 45.15, "longitude": 5.00 }},
              {{ "name": "Carlow", "latitude": 45.20, "longitude": 5.00 }}
            ],
            "networks": [{{
              "name": "Meridian", "operator": "Meridian Mobility",
              "lines": [
                {{ "origin": "Ashby", "destination": "Burton" }},
                {{ "origin": "Burton", "destination": "Carlow" }}
              ]
            }}]
          }},
          "cohorts": [
            {{
              "kind": "CarpoolDriver", "name": "through driver", "count": 1,
              "spawn_window": {{ "start_s": 60, "end_s": 60 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.20, "longitude": 5.00 }},
              "max_wait_s": 60, "rechain": {rechain}, "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "to Burton", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.15, "longitude": 5.00 }},
              "max_wait_s": 7200
            }},
            {{
              "kind": "CarpoolRider", "name": "to Carlow", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.15, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.20, "longitude": 5.00 }},
              "max_wait_s": 7200
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(!summary.stopped_by_clock, "a chain has to end");
    (sim, metrics)
}

/// Having set its rider down at Burton, a chaining driver takes the next line from where it stands
/// and carries the rider waiting there; one that does not chain drives home past it. The line back
/// towards Ashby is never offered to it, because it is behind.
#[test]
fn a_chaining_driver_carries_again_from_where_it_set_down() {
    let (sim, metrics) = chain(false);
    assert_eq!(metrics.passengers_served, 1);

    let (sim_chained, metrics_chained) = chain(true);
    assert_eq!(metrics_chained.passengers_served, 2);
    let driver = &of_kind(&sim_chained, AgentKind::CarpoolDriver)[0];
    assert_eq!(
        driver.chains, 2,
        "one link to Carlow, then nothing left ahead"
    );
    assert_eq!(driver.pickups, 2);
    assert!(driver.arrived());
    assert_eq!(of_kind(&sim, AgentKind::CarpoolDriver)[0].chains, 0);
}

/// Chaining is a registered driver choosing a line again, so it means nothing to anything else.
#[test]
fn rechain_on_a_ghost_is_a_load_error() {
    let error = Scenario::from_json(&format!(
        r#"{{
          "name": "broken", {TWO_STATIONS},
          "cohorts": [{{
            "kind": "GhostDriver", "name": "ghost", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "rechain": true, "vehicle": {{ "capacity": 4 }}
          }}]
        }}"#
    ))
    .expect_err("a ghost does not chain");
    assert!(format!("{error:#}").contains("rechain"), "{error:#}");
}

/// Two lines out of Northgate - north to Central, and west to Westfield - one rider waiting at
/// Northgate bound for `rider_to`, and one road driver of `kind` passing straight through
/// Northgate on its way north, with `extra` knobs.
fn passing(kind: &str, rider_to: (f64, f64), extra: &str) -> (Sim, Metrics) {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "passing", "tick_s": 1, "max_time_s": 14400,
          "environment": {{
            "stations": [
              {{ "name": "Northgate", "latitude": 45.00, "longitude": 5.00 }},
              {{ "name": "Central", "latitude": 45.27, "longitude": 5.00 }},
              {{ "name": "Westfield", "latitude": 45.10, "longitude": 4.70 }}
            ],
            "networks": [{{
              "name": "Valley", "operator": "Valley Mobility",
              "lines": [
                {{ "origin": "Northgate", "destination": "Central" }},
                {{ "origin": "Northgate", "destination": "Westfield" }}
              ]
            }}]
          }},
          "cohorts": [
            {{
              "kind": "{kind}", "name": "passing driver", "count": 1,
              "spawn_window": {{ "start_s": 300, "end_s": 300 }},
              "origin": {{ "latitude": 44.95, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.30, "longitude": 5.00 }},
              "perception_radius_km": 0.1, {extra} "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "waiting", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": {}, "longitude": {} }},
              "max_wait_s": 3600
            }}
          ]
        }}"#,
        rider_to.0, rider_to.1
    ))
    .unwrap_or_else(|error| panic!("loading the passing scenario: {error:#}"));
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(!summary.stopped_by_clock, "the run should end on its own");
    (sim, metrics)
}

/// A ghost that looks out of the window takes no line and goes to no station: it drives home, and
/// the rider standing at the stop it passes gets in and is set down at its own line's end.
#[test]
fn a_ghost_picks_up_a_rider_it_passes_and_sets_it_down_on_its_way() {
    let (sim, metrics) = passing("GhostDriver", (45.27, 5.00), "");
    assert_eq!(metrics.passengers_served, 1);
    let ghost = &of_kind(&sim, AgentKind::GhostDriver)[0];
    assert_eq!(ghost.line, None, "it never took a line");
    assert_eq!(ghost.pickups, 1);
    assert!(ghost.arrived());
    let rider = &of_kind(&sim, AgentKind::CarpoolRider)[0];
    assert!(
        rider.loaded_km > 25.0,
        "carried the length of the valley: {} km",
        rider.loaded_km
    );
}

/// The driver's refusal, rider by rider: a rider bound west would stretch the drive north far past
/// the bound, so the ghost leaves it standing.
#[test]
fn a_road_pickup_the_detour_forbids_is_left_standing() {
    let (_, metrics) = passing("GhostDriver", (45.10, 4.70), r#""max_detour_pct": 110,"#);
    assert_eq!(metrics.passengers_served, 0);
    let (_, unbounded) = passing("GhostDriver", (45.10, 4.70), "");
    assert_eq!(
        unbounded.passengers_served, 1,
        "with no bound it takes anyone"
    );
}

/// An opportunistic driver that registers only after its first pickup drives home unregistered,
/// and tells the operator the moment it has somebody aboard.
#[test]
fn an_opportunistic_driver_registers_after_its_first_road_pickup() {
    let (sim, metrics) = passing(
        "OpportunisticDriver",
        (45.27, 5.00),
        r#""station_approach": "on_demand", "registers": "after_first_pickup","#,
    );
    assert_eq!(metrics.passengers_served, 1);
    let driver = &of_kind(&sim, AgentKind::OpportunisticDriver)[0];
    assert_eq!(driver.pickups, 1);
    let declared_s = driver.declared_s.expect("it registered in the end");
    assert!(
        declared_s > driver.departed_s.unwrap(),
        "registered at {declared_s} s, after setting off at {:?}",
        driver.departed_s
    );
    assert!(driver.line.is_some());
}

/// Every misuse of the two knobs is a load error rather than a knob quietly ignored.
#[test]
fn road_pickup_knobs_where_they_mean_nothing_are_load_errors() {
    let cases = [
        ("CarpoolDriver", r#""perception_radius_km": 0.1"#, "neither"),
        (
            "OpportunisticDriver",
            r#""perception_radius_km": 0.1"#,
            "on_demand",
        ),
        ("GhostDriver", r#""perception_radius_km": 0"#, "positive"),
        (
            "OpportunisticDriver",
            r#""station_approach": "on_demand", "registers": "after_first_pickup""#,
            "perception_radius_km",
        ),
    ];
    for (kind, knobs, fragment) in cases {
        let error = Scenario::from_json(&format!(
            r#"{{
              "name": "broken", {TWO_STATIONS},
              "cohorts": [{{
                "kind": "{kind}", "name": "driver", "count": 1,
                "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                {knobs}, "vehicle": {{ "capacity": 4 }}
              }}]
            }}"#
        ))
        .expect_err("a misused road-pickup knob is rejected");
        assert!(
            format!("{error:#}").contains(fragment),
            "{kind} {knobs}: {error:#}"
        );
    }
}

/// A pre-formed group of three and a carpool rider at the same station for the same place, and one
/// group driver: it takes its own three and leaves the stranger standing, and it leaves as soon as
/// its three are aboard rather than waiting out its patience for a fourth seat.
#[test]
fn a_group_driver_carries_its_own_group_and_nobody_else() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "group", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
          "cohorts": [
            {{
              "kind": "PolynomialDriver", "name": "the family", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
              "group_size": 3, "max_wait_s": 1800, "vehicle": {{ "capacity": 5 }}
            }},
            {{
              "kind": "CarpoolRider", "name": "stranger", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
              "max_wait_s": 600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(!summary.stopped_by_clock);
    let driver = &of_kind(&sim, AgentKind::PolynomialDriver)[0];
    assert_eq!(driver.pickups, 3);
    assert_eq!(of_kind(&sim, AgentKind::PolynomialRider).len(), 3);
    assert_eq!(
        metrics.passengers_served, 3,
        "the family, and not the stranger"
    );
    assert_eq!(
        of_kind(&sim, AgentKind::CarpoolRider)[0].state,
        State::Canceled
    );
    assert_eq!(driver.reason, TransitionReason::ArrivedAtDestination);
}

/// A group passenger is spawned by its driver, and a group size means nothing to anybody else.
#[test]
fn group_knobs_where_they_mean_nothing_are_load_errors() {
    let cases = [
        ("PolynomialRider", r#""max_wait_s": 60"#, "never a cohort"),
        (
            "PolynomialDriver",
            r#""vehicle": { "capacity": 5 }"#,
            "needs a group_size",
        ),
        (
            "CarpoolDriver",
            r#""group_size": 2, "vehicle": { "capacity": 5 }"#,
            "does not travel",
        ),
    ];
    for (kind, knobs, fragment) in cases {
        let error = Scenario::from_json(&format!(
            r#"{{
              "name": "broken", {TWO_STATIONS},
              "cohorts": [{{
                "kind": "{kind}", "name": "cohort", "count": 1,
                "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                {knobs}
              }}]
            }}"#
        ))
        .expect_err("a misused group knob is rejected");
        assert!(format!("{error:#}").contains(fragment), "{kind}: {error:#}");
    }
}

/// A walk-up standing at the stop: a carpool driver that takes unregistered riders picks it up; one
/// that does not leaves it for a ghost that never comes.
#[test]
fn a_carpool_driver_that_takes_walk_ups_picks_up_a_ghost_rider() {
    let served = |takes: bool| {
        let scenario = Scenario::from_json(&format!(
            r#"{{
              "name": "walk-ups", "tick_s": 1, "max_time_s": 14400, {TWO_STATIONS},
              "cohorts": [
                {{
                  "kind": "CarpoolDriver", "name": "driver", "count": 1,
                  "spawn_window": {{ "start_s": 600, "end_s": 600 }},
                  "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                  "max_wait_s": 300, "takes_unregistered_riders": {takes},
                  "vehicle": {{ "capacity": 4 }}
                }},
                {{
                  "kind": "GhostRider", "name": "walk-up", "count": 1,
                  "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                  "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                  "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
                  "max_wait_s": 3600
                }}
              ]
            }}"#
        ))
        .unwrap();
        run(&scenario, 42).1.passengers_served
    };
    assert_eq!(served(false), 0);
    assert_eq!(served(true), 1);
}
