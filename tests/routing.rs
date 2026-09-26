//! The route cache, its straight-line fallback, and the determinism both have to preserve.

use mas::events::EventLog;
use mas::routing::{Profile, Router};
use mas::scenario::Scenario;
use mas::sim::Sim;
use mas::world::{haversine_km, Coord, LineId};

const NORTHGATE: Coord = Coord {
    latitude: 45.00,
    longitude: 5.00,
};
const TERMINUS: Coord = Coord {
    latitude: 45.27,
    longitude: 5.00,
};

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

fn scenario_with(cohorts: &str) -> Scenario {
    Scenario::from_json(&format!(
        r#"{{ "name": "routing", "tick_s": 1, "max_time_s": 7200, {TWO_STATIONS},
             "cohorts": [{cohorts}] }}"#
    ))
    .unwrap()
}

/// A cache entry with a deliberate dogleg to the east, so a hit is unmistakably not the straight
/// line: three points, and a duration nothing else in the test would produce.
const CACHE: &str = r#"{
  "routes": [
    {
      "profile": "Road",
      "from": { "latitude": 45.00, "longitude": 5.00 },
      "to": { "latitude": 45.27, "longitude": 5.00 },
      "duration_s": 2643.5,
      "points": [[45.00, 5.00], [45.14, 5.20], [45.27, 5.00]]
    }
  ]
}"#;

#[test]
fn a_cached_pair_returns_the_cached_route() {
    let scenario = scenario_with("");
    let mut router = Router::new(&scenario);
    assert_eq!(router.load_cache_json(CACHE).unwrap(), 1);

    let route = router.route(NORTHGATE, TERMINUS, Profile::Road);
    assert_eq!(
        route.points.len(),
        3,
        "the cached dogleg, not a straight line"
    );
    assert_eq!(route.duration_s, 2643.5);
    assert!(
        route.distance_km > haversine_km(NORTHGATE, TERMINUS),
        "a road route detours, so it is longer than the crow flies"
    );

    // The key rounds to about eleven metres, so floating-point noise on the same point still hits.
    let nudged = Coord::new(NORTHGATE.latitude + 1e-9, NORTHGATE.longitude - 1e-9);
    assert_eq!(router.route(nudged, TERMINUS, Profile::Road), route);

    // The same pair on foot is a different route, and there is no foot entry.
    let on_foot = router.route(NORTHGATE, TERMINUS, Profile::Foot);
    assert_eq!(on_foot.points.len(), 2, "no foot entry, so the fallback");
}

#[test]
fn a_miss_falls_back_within_a_sane_band_of_the_straight_line_time() {
    let scenario = scenario_with("");
    let router = Router::new(&scenario);
    let direct_km = haversine_km(NORTHGATE, TERMINUS);

    let on_foot = router.route(NORTHGATE, TERMINUS, Profile::Foot);
    let expected_walk_s = direct_km * 1000.0 / scenario.walk_speed_mps.nominal();
    assert!((on_foot.duration_s - expected_walk_s).abs() < 1e-6);
    assert!((on_foot.distance_km - direct_km).abs() < 1e-9);

    let by_road = router.route(NORTHGATE, TERMINUS, Profile::Road);
    let straight_line_s = direct_km * 1000.0 / scenario.drive_speed_mps.nominal();
    let ratio = by_road.duration_s / straight_line_s;
    assert!(
        (ratio - scenario.road_detour_factor).abs() < 1e-9,
        "a road miss should take the detour factor longer than the straight line, got {ratio}"
    );
    assert!(
        (1.0..2.0).contains(&ratio),
        "the detour factor should stay in a sane band, got {ratio}"
    );

    // The journey the fallback produces arrives when its duration says it does.
    let mut journey = by_road.clone().into_journey();
    let ticks = (by_road.duration_s / 1.0).ceil() as u32;
    for _ in 0..ticks {
        journey.advance(1.0);
    }
    assert!(
        journey.finished(),
        "the fallback journey should be over after its own duration"
    );
}

/// A line in the world follows the cached road geometry, not the straight line between its
/// endpoints. This is what makes `vehicle_km` a road distance rather than a crow-flies one.
#[test]
fn a_line_in_the_world_follows_the_cached_road() {
    let scenario = scenario_with("");

    let straight = Sim::new(&scenario, 0, Router::new(&scenario));
    let straight_route = &straight.world().line(LineId(0)).route;
    assert_eq!(
        straight_route.points.len(),
        2,
        "no cache, so the straight line"
    );

    let mut router = Router::new(&scenario);
    router.load_cache_json(CACHE).unwrap();
    let cached = Sim::new(&scenario, 0, router);
    let cached_route = &cached.world().line(LineId(0)).route;

    assert_eq!(cached_route.points.len(), 3, "the cached geometry");
    assert!(
        cached_route.distance_km > straight_route.distance_km,
        "the road is longer than the crow flies: {} km vs {} km",
        cached_route.distance_km,
        straight_route.distance_km
    );
}

/// Two runs of the same scenario, seed and cache write the same bytes.
#[test]
fn two_same_seed_runs_write_identical_event_logs() {
    let scenario = scenario_with(
        r#"{
          "kind": "GhostRider", "name": "walk-up riders", "count": 5,
          "spawn_window": { "start_s": 0, "end_s": 600 },
          "origin": { "latitude": 45.00, "longitude": 5.00, "jitter_radius_km": 1 },
          "destination": { "latitude": 45.27, "longitude": 5.00, "jitter_radius_km": 1 },
          "max_wait_s": 300
        }"#,
    );

    let run_to_file = |name: &str| {
        let path = std::env::temp_dir().join(format!("mas-determinism-{name}.csv"));
        let mut router = Router::new(&scenario);
        router.load_cache_json(CACHE).unwrap();
        let mut log = EventLog::to_file(&path).unwrap();
        Sim::new(&scenario, 42, router).run(&mut log).unwrap();
        drop(log);
        std::fs::read(&path).unwrap()
    };

    let first = run_to_file("first");
    assert!(!first.is_empty(), "the run should have logged something");
    assert_eq!(first, run_to_file("second"), "same seed, same bytes");
}
