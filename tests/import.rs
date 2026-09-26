//! Translating the agent-list format: every fixture translates in both variants and loads, each
//! field lands where it should, the two variants differ exactly where they are meant to, and
//! anything the format does not have is an error.
//!
//! The fixtures are small cuts of what `scripts/build-lyon-scenarios.py` writes: the lane in both
//! directions, one cell of the advance-declaration grid, and a few pairs of each of the three
//! network services.

use std::path::Path;

use mas::events::EventLog;
use mas::import::{translate, Variant};
use mas::routing::Router;
use mas::scenario::{AgentKind, Scenario, Service, SpawnWindow, StationApproach, Value};
use mas::sim::Sim;
use serde_json::Value as Json;

const FIXTURES: [&str; 6] = [
    "lane-bourgoin",
    "lane-mermoz",
    "lane-grid-cell",
    "private-cars",
    "fleet-door-to-door",
    "fleet-network",
];

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent-list")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn import(name: &str, variant: Variant) -> Scenario {
    let (text, _notes) = translate(&fixture(name), name, variant, 1.3)
        .unwrap_or_else(|error| panic!("translating {name}: {error:#}"));
    Scenario::from_json(&text).unwrap()
}

fn cohort<'a>(scenario: &'a Scenario, label: &str) -> &'a mas::scenario::Cohort {
    scenario
        .cohorts
        .iter()
        .find(|cohort| cohort.name.starts_with(label))
        .unwrap_or_else(|| panic!("no cohort {label:?}"))
}

#[test]
fn every_fixture_translates_in_both_variants_and_runs_to_the_end() {
    for name in FIXTURES {
        for variant in [Variant::AsRan, Variant::Corrected] {
            let scenario = import(name, variant);
            let mut sim = Sim::new(&scenario, 1, Router::new(&scenario));
            let summary = sim.run(&mut EventLog::discarding().unwrap()).unwrap();
            // The as-ran lane's riders have no patience, so the unmatched ones wait for the cap;
            // everything else ends on its own.
            let patient = !(variant == Variant::AsRan && name.starts_with("lane"));
            if patient {
                assert!(!summary.stopped_by_clock, "{name} {variant:?} hit the cap");
            }
            assert!(
                summary.agents_spawned > 0,
                "{name} {variant:?} spawned nobody"
            );
        }
    }
}

#[test]
fn the_two_variants_differ_exactly_where_they_are_meant_to() {
    let as_ran = import("lane-grid-cell", Variant::AsRan);
    let corrected = import("lane-grid-cell", Variant::Corrected);
    assert_eq!((as_ran.tick_s, corrected.tick_s), (30.0, 1.0));
    assert_eq!(as_ran.walk_speed_mps, Value::Fixed(4.0));
    assert_eq!(corrected.walk_speed_mps, Value::Fixed(1.4));
    assert_eq!(as_ran.drive_speed_mps, Value::Fixed(20.0));

    // The declaring rider never timed itself to its driver; corrected, it does. The driver always.
    for (scenario, rider_times) in [(&as_ran, false), (&corrected, true)] {
        assert_eq!(
            cohort(scenario, "carpool rider atp").times_to_counterpart,
            rider_times
        );
        assert!(cohort(scenario, "carpool driver atc").times_to_counterpart);
    }

    // The guarantee never dispatched as ran, and is there corrected, counting its driver's seat.
    let guarantee =
        |scenario: &Scenario| scenario.environment.networks[0].lines[0].departure_guarantee;
    assert!(guarantee(&as_ran).is_none());
    let kept = guarantee(&corrected).expect("the corrected run keeps the guarantee");
    assert_eq!(
        (kept.trigger_after_wait_s, kept.cooldown_s, kept.capacity),
        (1200.0, 1200.0, 9)
    );

    // The lane's carpool rider had no patience and its ghost rider froze at 1500 s.
    assert_eq!(
        cohort(&as_ran, "carpool rider").max_wait_s,
        Value::Fixed(172_800.0)
    );
    assert_eq!(cohort(&as_ran, "rider").max_wait_s, Value::Fixed(1500.0));
    assert_eq!(
        cohort(&corrected, "carpool rider").max_wait_s,
        Value::Fixed(1800.0)
    );

    // A fleet vehicle carried one fewer than its capacity as ran.
    let seats = |variant| {
        let scenario = import("fleet-door-to-door", variant);
        cohort(&scenario, "FleetTaxi")
            .vehicle
            .as_ref()
            .unwrap()
            .capacity
    };
    assert_eq!((seats(Variant::AsRan), seats(Variant::Corrected)), (4, 5));
}

#[test]
fn the_lane_maps_windows_seats_and_companions() {
    let scenario = import("lane-grid-cell", Variant::AsRan);
    let driver = cohort(&scenario, "carpool driver atc");
    assert_eq!(driver.kind, AgentKind::CarpoolDriver);
    assert_eq!(driver.advanced_declaration_lead_s, 600.0);
    assert_eq!(
        driver.departure_offset_s,
        Value::Fixed(600.0),
        "declared departure is lead after spawn"
    );
    assert_eq!(
        (driver.earliness_margin_s, driver.lateness_margin_s),
        (60.0, 60.0)
    );
    assert!(matches!(
        driver.departure_noise_s,
        Value::Drawn(mas::scenario::Distribution::Normal { mean, sd }) if mean == -60.0 && sd == 240.0
    ));
    assert_eq!(
        driver.vehicle.as_ref().unwrap().capacity,
        6,
        "five riders plus the driver's seat"
    );
    assert_eq!(driver.station_approach, StationApproach::OnDemand);
    assert!(driver.rechain);
    assert!(matches!(
        driver.additional_passengers,
        Value::Drawn(mas::scenario::Distribution::Bernoulli { .. })
    ));
    assert!(matches!(&driver.spawn_window, SpawnWindow::Range(range) if range.end_s == 3600.0));

    let ghost = cohort(&scenario, "ghost");
    assert_eq!(ghost.perception_radius_km, Some(0.1));
    let opportunistic = cohort(&scenario, "opportunistic");
    assert_eq!(
        opportunistic.registers,
        mas::scenario::Registers::AfterFirstPickup
    );
    assert_eq!(
        cohort(&scenario, "rider").max_walk_s,
        Value::Fixed(172_800.0)
    );
}

#[test]
fn the_network_services_map_spawns_fleets_and_legs() {
    let private = import("private-cars", Variant::AsRan);
    assert!(private
        .cohorts
        .iter()
        .all(|c| c.kind == AgentKind::PrivateDriver));
    assert!(
        private.environment.stations.is_empty(),
        "a world of private cars needs no stations"
    );
    let outbound = private
        .cohorts
        .iter()
        .find(|c| c.name.contains("outbound"))
        .unwrap();
    assert!(
        matches!(&outbound.spawn_window, SpawnWindow::Drawn(Value::Drawn(
        mas::scenario::Distribution::Normal { mean, sd })) if *mean == 27000.0 && *sd == 1800.0)
    );

    let network = import("fleet-network", Variant::AsRan);
    assert_eq!(network.policy, mas::policy::PolicyName::UrgencyAsRan);
    assert_eq!(
        import("fleet-network", Variant::Corrected).policy,
        mas::policy::PolicyName::Urgency
    );
    let taxis = cohort(&network, "FleetTaxi");
    assert!(matches!(&taxis.spawn_window, SpawnWindow::Drawn(Value::Fixed(s)) if *s == 18000.0));
    assert!(taxis.destination.is_none());
    let rider = network
        .cohorts
        .iter()
        .find(|c| c.kind == AgentKind::AutonomousTaxiRider)
        .unwrap();
    assert_eq!(rider.service, Service::Network);
    assert_eq!(rider.transfer_max_wait_s, Some(Value::Fixed(900.0)));
    assert_eq!(rider.assigned_wait_factor, 1.5);
    assert_eq!(network.environment.networks[0].lines.len(), 36);

    let door = import("fleet-door-to-door", Variant::AsRan);
    let rider = door
        .cohorts
        .iter()
        .find(|c| c.kind == AgentKind::AutonomousTaxiRider)
        .unwrap();
    assert_eq!(rider.service, Service::DoorToDoor);
}

/// A way to break a source document.
type Breakage = fn(&mut Json);

#[test]
fn anything_the_format_does_not_have_is_an_error() {
    let lane = fixture("lane-bourgoin");
    let mut document: Json = serde_json::from_str(&lane).unwrap();
    let cases: [(Breakage, &str); 3] = [
        (
            |d: &mut Json| d["simulation"]["start"]["agents"][1]["class"] = "Hovercraft".into(),
            "unknown class",
        ),
        (
            |d: &mut Json| d["simulation"]["start"]["agents"][1]["colour"] = "red".into(),
            "unknown field",
        ),
        (
            |d: &mut Json| d["simulation"]["start"]["agents"][1]["class"] = "TaxiDriver".into(),
            "dispatched",
        ),
    ];
    for (break_it, fragment) in cases {
        let mut broken = document.clone();
        break_it(&mut broken);
        let error = translate(&broken.to_string(), "broken", Variant::AsRan, 1.3)
            .expect_err("a broken source is rejected");
        assert!(format!("{error:#}").contains(fragment), "{error:#}");
    }
    document["environment"]["stations"][0]["radius"] = 1000.into();
    let (_, notes) = translate(&document.to_string(), "noted", Variant::AsRan, 1.3).unwrap();
    assert!(notes.iter().any(|note| note.contains("display-only")));
}
