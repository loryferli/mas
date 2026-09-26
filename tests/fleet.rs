//! The on-demand fleet: door-to-door service, and the operator that assigns a vehicle to a rider.
//!
//! What separates a fleet taxi from every other driver is that it has no trip of its own, so
//! every kilometre it covers empty is a cost the service pays rather than a trip somebody was
//! making anyway. These tests pin the four things that has to mean: the rider is collected where
//! it is standing, two riders going the same way share one vehicle, a taxi nobody assigned covers
//! no distance at all, and the assignment is event-driven rather than run on every tick.

use mas::agents::{Agent, State, TransitionReason};
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::routing::Router;
use mas::scenario::{AgentKind, Scenario};
use mas::sim::{RunSummary, Sim};
use mas::world::haversine_km;

/// A fleet scenario declares no lines: service is door to door, so there is nothing to be matched
/// onto. The stations are still here because the geography is, and because a mixed scenario would
/// need them.
const NO_LINES: &str = r#"
    "environment": {
      "stations": [
        { "name": "Northgate Park and Ride", "latitude": 45.00, "longitude": 5.00 }
      ],
      "networks": []
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

/// One taxi, one rider, no station in between: the rider must be collected at its own origin and
/// set down at its own destination, which is the whole of what door-to-door means.
#[test]
fn a_fleet_rider_is_collected_at_its_own_origin() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "one taxi one rider", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "the hailer", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.10, "longitude": 4.90 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.10 }},
              "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(!summary.stopped_by_clock, "a fleet run ends on its own");
    assert_eq!(summary.stranded, 0, "and strands nobody");
    assert_eq!(metrics.passengers_served, 1);

    let rider = of_kind(&sim, AgentKind::AutonomousTaxiRider)[0];
    let taxi = of_kind(&sim, AgentKind::AutonomousTaxi)[0];
    assert_eq!(rider.state, State::EndJourney);
    assert_eq!(rider.reason, TransitionReason::ArrivedAtDestination);
    assert_eq!(rider.carried_by, Some(taxi.id));
    assert_eq!(
        rider.station, None,
        "a fleet rider never walks to a station, so it never holds one"
    );
    assert_eq!(taxi.station, None, "and neither does the vehicle");
    assert_eq!(taxi.pickups, 1);

    // The pickup happened at the rider's own door, which is what separates this from a carpool
    // rendezvous: there the rider walks and the position at pickup is a station's.
    assert!(rider.loaded_km > 0.0, "the rider was carried some distance");
    let door_to_door_km = haversine_km(rider.origin, rider.destination);
    assert!(
        (rider.loaded_km - door_to_door_km).abs() < door_to_door_km * 0.001,
        "the ride covers origin to destination, not a station pair: {} against {door_to_door_km}",
        rider.loaded_km
    );

    // A carpool driver's empty kilometres were a trip it was making anyway. These are not.
    assert!(
        metrics.taxi_km_empty_to_pickup > 0.0,
        "the taxi drove out to collect somebody, and that is pure overhead"
    );
    assert_eq!(metrics.fleet_utilisation, 1.0, "the one taxi worked");
    assert!(metrics.mean_time_to_pickup_s > 0.0);
}

/// Two riders standing together and bound for the same place must travel as one trip. This is the
/// whole of the pooling the fleet does: one drop-off, and whoever is not going exactly there walks
/// the remainder.
#[test]
fn two_riders_sharing_a_destination_travel_as_one_trip() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "pooled", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "vehicle": {{ "capacity": 2 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "the pair", "count": 2,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.10, "longitude": 4.90 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.10 }},
              "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, _) = run(&scenario, 42);

    assert_eq!(metrics.passengers_served, 2);
    let taxi = of_kind(&sim, AgentKind::AutonomousTaxi)[0];
    assert_eq!(taxi.pickups, 2, "both in the one vehicle");

    // One trip, not two: the loaded distance is one ride's worth carrying two people, so the
    // passenger-kilometres are twice the vehicle's loaded kilometres.
    assert!(
        (metrics.passenger_km - 2.0 * metrics.vehicle_km_loaded).abs() < 1e-9,
        "two people over one loaded leg: {} passenger-km on {} loaded",
        metrics.passenger_km,
        metrics.vehicle_km_loaded
    );
    assert!(
        metrics.mean_occupancy > 1.0,
        "pooling is the only way a fleet occupancy exceeds one, got {}",
        metrics.mean_occupancy
    );
}

/// A taxi nobody assigns must cover no distance at all. There is deliberately no repositioning
/// rule - that is the decision a model is handed, and it competes against standing
/// still - so an idle vehicle standing still is the behaviour, not an oversight.
#[test]
fn an_unassigned_taxi_covers_no_distance() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "idle fleet", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 3,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00, "jitter_radius_km": 2 }},
              "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "the hailer", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.10, "longitude": 4.90 }},
              "destination": {{ "latitude": 45.27, "longitude": 5.10 }},
              "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 7);

    assert!(!summary.stopped_by_clock, "the idle fleet stands down");
    let idle: Vec<&Agent> = of_kind(&sim, AgentKind::AutonomousTaxi)
        .into_iter()
        .filter(|taxi| taxi.pickups == 0)
        .collect();
    assert_eq!(idle.len(), 2, "one taxi took the one rider");
    for taxi in &idle {
        assert_eq!(
            taxi.distance_km, 0.0,
            "taxi {:?} moved without being assigned anybody",
            taxi.id
        );
        assert_eq!(taxi.reason, TransitionReason::StoodDown);
    }
    assert!(
        (metrics.fleet_utilisation - 1.0 / 3.0).abs() < 1e-9,
        "one of three vehicles was needed, got {}",
        metrics.fleet_utilisation
    );
}

/// Assignment must run only where demand or idleness changed. A per-tick global matrix over the
/// whole fleet is the most expensive thing this engine could do, and it would put a model call on
/// every tick, so the trigger is the thing worth pinning rather than the pairing.
#[test]
fn assignment_runs_only_when_demand_or_idleness_changed() {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "event driven", "tick_s": 1, "max_time_s": 28800, {NO_LINES},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "the queue", "count": 3,
              "spawn_window": {{ "start_s": 0, "end_s": 600 }},
              "origin": {{ "latitude": 45.02, "longitude": 4.98, "jitter_radius_km": 0.5 }},
              "destination": {{ "latitude": 45.06, "longitude": 5.02, "jitter_radius_km": 0.5 }},
              "max_wait_s": 28800
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);

    assert!(
        !summary.stopped_by_clock,
        "the run has to finish for the count to mean anything"
    );
    assert_eq!(metrics.passengers_served, 3, "one vehicle, three trips");
    // Three riders and one vehicle: the operator can only ever pair on the tick a rider hails
    // with the vehicle free, or the tick the vehicle comes free with a rider waiting. Three
    // assignments, out of thousands of ticks.
    assert_eq!(sim.fleet_assignments(), 3);
    assert!(
        summary.ticks > 100,
        "and the run was long enough for a per-tick matrix to be obvious, {} ticks",
        summary.ticks
    );
}

/// A scenario that declares a fleet taxi with somewhere to be, or a party of its own, means
/// something other than what it says: a taxi has no trip, and nobody rides with a robot.
#[test]
fn a_fleet_taxi_declaring_a_trip_of_its_own_is_a_load_error() {
    let with_destination = format!(
        r#"{{
          "name": "bad", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
          "cohorts": [{{
            "kind": "AutonomousTaxi", "name": "the fleet", "count": 1,
            "spawn_window": {{ "start_s": 0, "end_s": 0 }},
            "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
            "destination": {{ "latitude": 45.27, "longitude": 5.00 }},
            "vehicle": {{ "capacity": 4 }}
          }}]
        }}"#
    );
    let error = format!("{:#}", Scenario::from_json(&with_destination).unwrap_err());
    assert!(
        error.contains("no trip of its own"),
        "unexpected error: {error}"
    );

    let with_party = with_destination
        .replace(
            r#""destination": { "latitude": 45.27, "longitude": 5.00 },"#,
            "",
        )
        .replace(r#""vehicle""#, r#""additional_passengers": 1, "vehicle""#);
    let error = format!("{:#}", Scenario::from_json(&with_party).unwrap_err());
    assert!(
        error.contains("carries nobody of its own"),
        "unexpected error: {error}"
    );
}

/// One taxi and two riders at the same kerb whose doors are 700 m apart, with the taxi chaining
/// its drop-offs or not.
fn two_doors(chained: bool) -> (Sim, Metrics) {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "two doors", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "chained_drop_offs": {chained}, "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "first door", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.20, "longitude": 5.00 }},
              "max_wait_s": 3600
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "second door", "count": 1,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.2063, "longitude": 5.00 }},
              "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap();
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(!summary.stopped_by_clock);
    (sim, metrics)
}

/// Chaining its drop-offs, the taxi sets the second rider down at its own door instead of leaving
/// it to walk 700 m from the first one's, and drives that 700 m itself. Both share the vehicle
/// either way: the pooling radius, not the drop-off, decides who rides together.
#[test]
fn a_chaining_taxi_sets_each_rider_down_at_its_own_door() {
    for chained in [false, true] {
        let (sim, metrics) = two_doors(chained);
        assert_eq!(metrics.passengers_served, 2, "chained {chained}");
        let taxi = &of_kind(&sim, AgentKind::AutonomousTaxi)[0];
        assert_eq!(taxi.pickups, 2, "both rode together, chained {chained}");
        let second = of_kind(&sim, AgentKind::AutonomousTaxiRider)[1];
        // What it covered on foot: everything it travelled that it was not carried.
        let walked_km = second.distance_km - second.loaded_km;
        match chained {
            false => assert!(walked_km > 0.6, "walked {walked_km} km from the first door"),
            true => assert!(
                walked_km < 0.01,
                "set down at its own door, walked {walked_km} km"
            ),
        }
    }
    let (single, _) = two_doors(false);
    let (chained, _) = two_doors(true);
    let km = |sim: &Sim| of_kind(sim, AgentKind::AutonomousTaxi)[0].distance_km;
    assert!(km(&chained) > km(&single) + 0.6);
}

/// A carpool vehicle always sets each rider down at its own line's destination, so the knob only
/// means something to a fleet taxi.
#[test]
fn chained_drop_offs_on_a_carpool_driver_is_a_load_error() {
    let error = Scenario::from_json(
        r#"{
          "name": "broken",
          "environment": {
            "stations": [
              { "name": "A", "latitude": 45.0, "longitude": 5.0 },
              { "name": "B", "latitude": 45.2, "longitude": 5.0 }
            ],
            "networks": [{ "name": "n", "operator": "o", "lines": [{ "origin": "A", "destination": "B" }] }]
          },
          "cohorts": [{
            "kind": "CarpoolDriver", "name": "driver", "count": 1,
            "spawn_window": { "start_s": 0, "end_s": 0 },
            "origin": { "latitude": 45.0, "longitude": 5.0 },
            "destination": { "latitude": 45.2, "longitude": 5.0 },
            "chained_drop_offs": true, "vehicle": { "capacity": 4 }
          }]
        }"#,
    )
    .expect_err("a carpool vehicle chains by its riders' lines already");
    assert!(
        format!("{error:#}").contains("chained_drop_offs"),
        "{error:#}"
    );
}

/// Three stations up a meridian joined by two lines, two taxis at the first, and `riders` riders
/// travelling from beside the first station to beside the last, over the network.
fn over_the_network(riders: u32, rider_knobs: &str) -> (Sim, Metrics) {
    let scenario = Scenario::from_json(&format!(
        r#"{{
          "name": "network fleet", "tick_s": 1, "max_time_s": 14400,
          "environment": {{
            "stations": [
              {{ "name": "Ashby", "latitude": 45.00, "longitude": 5.00 }},
              {{ "name": "Burton", "latitude": 45.10, "longitude": 5.00 }},
              {{ "name": "Carlow", "latitude": 45.20, "longitude": 5.00 }}
            ],
            "networks": [{{
              "name": "Meridian", "operator": "Meridian Fleet",
              "lines": [
                {{ "origin": "Ashby", "destination": "Burton" }},
                {{ "origin": "Burton", "destination": "Carlow" }}
              ]
            }}]
          }},
          "cohorts": [
            {{
              "kind": "AutonomousTaxi", "name": "the fleet", "count": 2,
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
              "vehicle": {{ "capacity": 4 }}
            }},
            {{
              "kind": "AutonomousTaxiRider", "name": "through riders", "count": {riders},
              "spawn_window": {{ "start_s": 0, "end_s": 0 }},
              "origin": {{ "latitude": 45.001, "longitude": 5.00 }},
              "destination": {{ "latitude": 45.199, "longitude": 5.00 }},
              "service": "network", {rider_knobs} "max_wait_s": 3600
            }}
          ]
        }}"#
    ))
    .unwrap_or_else(|error| panic!("loading the network fleet: {error:#}"));
    let (sim, metrics, summary) = run(&scenario, 42);
    assert!(
        !summary.stopped_by_clock,
        "a network fleet run ends on its own"
    );
    (sim, metrics)
}

/// Over the network a trip is four hails: door to the nearest station, one per line, and the last
/// station to the door - each a separate pickup, and every change a wait at a station.
#[test]
fn a_network_rider_travels_its_path_one_leg_at_a_time() {
    let (sim, metrics) = over_the_network(1, "");
    assert_eq!(metrics.passengers_served, 1);
    let rider = &of_kind(&sim, AgentKind::AutonomousTaxiRider)[0];
    assert!(rider.arrived());
    assert_eq!(rider.legs.len(), 4, "Ashby, Burton, Carlow, then the door");
    assert_eq!(rider.leg, 3);
    let pickups: u32 = of_kind(&sim, AgentKind::AutonomousTaxi)
        .iter()
        .map(|taxi| taxi.pickups)
        .sum();
    assert_eq!(pickups, 4, "one pickup per leg");
    assert!(
        rider.loaded_km > 21.0,
        "carried the length of the meridian: {} km",
        rider.loaded_km
    );
}

/// Riders at the same station taking the same next hop share the vehicle for that hop, which is
/// how a network pools a fleet that door to door would not.
#[test]
fn network_riders_share_each_hop() {
    let (sim, metrics) = over_the_network(3, "");
    assert_eq!(metrics.passengers_served, 3);
    let taxis = of_kind(&sim, AgentKind::AutonomousTaxi);
    let pickups: u32 = taxis.iter().map(|taxi| taxi.pickups).sum();
    assert_eq!(pickups, 12, "four legs for each of three riders");
    // Two idle taxis split the first two riders between them, and the third shares a vehicle: two
    // vehicles' worth of loaded kilometres, not three.
    let km: f64 = taxis.iter().map(|taxi| taxi.loaded_km).sum();
    assert!(
        km < 50.0,
        "three riders rode 22 km in two vehicles, not three: {km} km loaded"
    );
}

/// When the same station is nearest both ends there is no network trip to make, and the rider is
/// served door to door.
#[test]
fn a_network_rider_with_one_station_nearest_both_ends_goes_door_to_door() {
    let scenario = Scenario::from_json(
        r#"{
          "name": "short hop", "tick_s": 1, "max_time_s": 7200,
          "environment": {
            "stations": [
              { "name": "Ashby", "latitude": 45.00, "longitude": 5.00 },
              { "name": "Carlow", "latitude": 45.20, "longitude": 5.00 }
            ],
            "networks": [{ "name": "m", "operator": "m", "lines": [{ "origin": "Ashby", "destination": "Carlow" }] }]
          },
          "cohorts": [
            { "kind": "AutonomousTaxi", "name": "fleet", "count": 1,
              "spawn_window": { "start_s": 0, "end_s": 0 },
              "origin": { "latitude": 45.00, "longitude": 5.00 }, "vehicle": { "capacity": 4 } },
            { "kind": "AutonomousTaxiRider", "name": "local", "count": 1,
              "spawn_window": { "start_s": 0, "end_s": 0 },
              "origin": { "latitude": 45.01, "longitude": 5.00 },
              "destination": { "latitude": 45.03, "longitude": 5.00 },
              "service": "network", "max_wait_s": 3600 }
          ]
        }"#,
    )
    .unwrap();
    let (sim, metrics, _) = run(&scenario, 42);
    assert_eq!(metrics.passengers_served, 1);
    assert!(of_kind(&sim, AgentKind::AutonomousTaxiRider)[0]
        .legs
        .is_empty());
}

/// The network knobs mean nothing off a network fleet rider, and a network needs lines.
#[test]
fn network_service_knobs_where_they_mean_nothing_are_load_errors() {
    let with_line = r#""networks": [{ "name": "m", "operator": "m", "lines": [{ "origin": "A", "destination": "B" }] }]"#;
    let cases = [
        (
            "CarpoolRider",
            r#""service": "network","#,
            with_line,
            "not one",
        ),
        (
            "AutonomousTaxiRider",
            r#""transfer_max_wait_s": 900,"#,
            with_line,
            "only a network",
        ),
        (
            "AutonomousTaxiRider",
            r#""assigned_wait_factor": 0.5,"#,
            with_line,
            "at least 1",
        ),
        (
            "CarpoolRider",
            r#""assigned_wait_factor": 1.5,"#,
            with_line,
            "not one",
        ),
        (
            "AutonomousTaxiRider",
            r#""service": "network","#,
            r#""networks": []"#,
            "travels over them",
        ),
    ];
    for (kind, knobs, networks, fragment) in cases {
        let error = Scenario::from_json(&format!(
            r#"{{
              "name": "broken",
              "environment": {{
                "stations": [
                  {{ "name": "A", "latitude": 45.0, "longitude": 5.0 }},
                  {{ "name": "B", "latitude": 45.2, "longitude": 5.0 }}
                ],
                {networks}
              }},
              "cohorts": [{{
                "kind": "{kind}", "name": "rider", "count": 1,
                "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                "origin": {{ "latitude": 45.0, "longitude": 5.0 }},
                "destination": {{ "latitude": 45.2, "longitude": 5.0 }},
                {knobs} "max_wait_s": 600
              }}]
            }}"#
        ))
        .expect_err("a misused network knob is rejected");
        assert!(
            format!("{error:#}").contains(fragment),
            "{kind} {knobs}: {error:#}"
        );
    }
}

/// A rider 3 km from the only taxi, with a minute and a half of patience: without a stretch it has
/// given up before the taxi arrives, and with its patience stretched once the taxi is coming it
/// is still there.
#[test]
fn an_assigned_rider_waits_longer_for_the_taxi_coming_for_it() {
    let served = |factor: f64| {
        let scenario = Scenario::from_json(&format!(
            r#"{{
              "name": "stretch", "tick_s": 1, "max_time_s": 7200, {NO_LINES},
              "cohorts": [
                {{ "kind": "AutonomousTaxi", "name": "fleet", "count": 1,
                   "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                   "origin": {{ "latitude": 45.00, "longitude": 5.00 }},
                   "vehicle": {{ "capacity": 4 }} }},
                {{ "kind": "AutonomousTaxiRider", "name": "impatient", "count": 1,
                   "spawn_window": {{ "start_s": 0, "end_s": 0 }},
                   "origin": {{ "latitude": 45.027, "longitude": 5.00 }},
                   "destination": {{ "latitude": 45.10, "longitude": 5.00 }},
                   "max_wait_s": 90, "assigned_wait_factor": {factor} }}
              ]
            }}"#
        ))
        .unwrap();
        run(&scenario, 42).1.passengers_served
    };
    assert_eq!(served(1.0), 0);
    assert_eq!(served(5.0), 1);
}

/// A rider near the end of its patience, far from the only idle taxi, and a fresh rider right next
/// to it. `greedy` sends the taxi to the near one; `urgency` weighs the one about to give up above
/// the distance, and sends it there.
#[test]
fn the_urgency_rule_reaches_for_the_rider_about_to_give_up() {
    use mas::agents::AgentId;
    use mas::policy::{greedy_pairs, urgency_pairs, AssignContext, IdleTaxi, WaitingRider};
    use mas::world::Coord;
    let rider = |id: u32, latitude: f64, patience_spent: f64| WaitingRider {
        id: AgentId(id),
        position: Coord::new(latitude, 5.0),
        destination: Coord::new(45.5, 5.0),
        seats: 1,
        patience_spent,
        neighbours: 0,
    };
    let context = AssignContext {
        taxis: vec![IdleTaxi {
            id: AgentId(0),
            position: Coord::new(45.0, 5.0),
            free_seats: 4,
        }],
        riders: vec![rider(1, 45.001, 0.0), rider(2, 45.2, 0.95)],
    };
    assert_eq!(
        greedy_pairs(&context)[0].rider,
        0,
        "greedy takes the near rider"
    );
    assert_eq!(
        urgency_pairs(&context, false)[0].rider,
        1,
        "urgency takes the one about to go"
    );

    // Neighbours ride together: two riders at one kerb for one stop, one taxi, both assigned to it.
    let mut crowd = context.clone();
    crowd.riders = vec![
        WaitingRider {
            neighbours: 1,
            ..rider(1, 45.02, 0.0)
        },
        WaitingRider {
            neighbours: 1,
            ..rider(2, 45.02, 0.0)
        },
    ];
    let pairs = urgency_pairs(&crowd, false);
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().all(|pair| pair.taxi == 0));
}

/// The rule as the published runs had it: one taxi, two riders far apart, and the taxi is handed
/// both - the first assignment overwritten by the second, so only the second rider is collected.
#[test]
fn the_urgency_rule_as_ran_hands_one_taxi_two_riders_and_loses_the_first() {
    use mas::agents::AgentId;
    use mas::policy::{urgency_pairs, AssignContext, IdleTaxi, WaitingRider};
    use mas::world::Coord;
    let rider = |id: u32, latitude: f64| WaitingRider {
        id: AgentId(id),
        position: Coord::new(latitude, 5.0),
        destination: Coord::new(45.5, 5.0),
        seats: 1,
        patience_spent: 0.0,
        neighbours: 0,
    };
    let context = AssignContext {
        taxis: vec![IdleTaxi {
            id: AgentId(0),
            position: Coord::new(45.0, 5.0),
            free_seats: 4,
        }],
        riders: vec![rider(1, 45.01), rider(2, 45.2)],
    };
    let fixed = urgency_pairs(&context, false);
    assert_eq!(fixed.len(), 1, "each taxi once");
    let as_ran = urgency_pairs(&context, true);
    assert_eq!(as_ran.len(), 2);
    assert!(as_ran.iter().all(|pair| pair.taxi == 0));
    assert_eq!(
        as_ran.iter().filter(|pair| pair.collected).count(),
        1,
        "only the last is collected"
    );
    assert!(!as_ran[0].collected);
}
