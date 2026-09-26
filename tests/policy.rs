//! The policy seam: which line a driver takes, and what changes when the rule changes.

use mas::agents::{State, TransitionReason};
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::policy::PolicyName;
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use std::path::Path;

fn load(name: &str, policy: PolicyName) -> Scenario {
    let mut scenario = Scenario::load(Path::new("scenarios").join(name).as_path())
        .unwrap_or_else(|error| panic!("loading {name}: {error:#}"));
    scenario.policy = policy;
    scenario
}

fn run(scenario: &Scenario, seed: u64) -> (Sim, Metrics) {
    run_with(scenario, seed, Router::new(scenario))
}

/// The committed cache, for the one test whose figures are the documented command's own.
fn cached(scenario: &Scenario, seed: u64) -> (Sim, Metrics) {
    let mut router = Router::new(scenario);
    router
        .load_cache(Path::new("data/routes.json"))
        .expect("the committed route cache");
    run_with(scenario, seed, router)
}

fn run_with(scenario: &Scenario, seed: u64, router: Router) -> (Sim, Metrics) {
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(scenario, seed, router);
    let summary = sim.run(&mut log).unwrap();
    let metrics = Metrics::collect(sim.agents(), &summary, &NoIncentive);
    (sim, metrics)
}

/// Two stations feeding one destination, and a driver standing at the one the ranking prefers
/// while the only rider stands at the other. Both lines are inside the driver's detour bound, so
/// nothing but the policy decides which it drives.
///
/// None of these coordinates are in the committed cache, so every leg is the straight-line
/// fallback: the driver's own trip is 30.0 km, driving the near line costs it 100.0% of that and
/// the far one 100.4%, and the rider will not walk the 5.6 km between the two stations.
const TWO_STATIONS: &str = r#"{
  "name": "policy-choice",
  "tick_s": 1,
  "max_time_s": 14400,
  "policy": "POLICY",
  "environment": {
    "stations": [
      { "name": "Ashford Green",    "latitude": 46.000, "longitude": 6.00 },
      { "name": "Bellwood Cross",   "latitude": 46.050, "longitude": 6.01 },
      { "name": "Cathedral Square", "latitude": 46.270, "longitude": 6.00 }
    ],
    "networks": [{
      "name": "Cathedral Corridor",
      "operator": "Cathedral Mobility",
      "lines": [
        { "origin": "Ashford Green",  "destination": "Cathedral Square" },
        { "origin": "Bellwood Cross", "destination": "Cathedral Square" }
      ]
    }]
  },
  "cohorts": [
    {
      "kind": "CarpoolDriver", "name": "the driver", "count": 1,
      "spawn_window": { "start_s": 0, "end_s": 0 },
      "origin": { "latitude": 46.000, "longitude": 6.00 },
      "destination": { "latitude": 46.270, "longitude": 6.00 },
      "departure_offset_s": 300,
      "max_detour_pct": 150,
      "vehicle": { "capacity": 5 }
    },
    {
      "kind": "CarpoolRider", "name": "the rider", "count": 1,
      "spawn_window": { "start_s": 0, "end_s": 0 },
      "origin": { "latitude": 46.050, "longitude": 6.01 },
      "destination": { "latitude": 46.270, "longitude": 6.00 },
      "departure_offset_s": 0
    }
  ]
}"#;

fn two_stations(policy: &str) -> Scenario {
    Scenario::from_json(&TWO_STATIONS.replace("POLICY", policy)).expect("valid scenario")
}

/// The whole of the seam in one scenario: the same world, the same seed, two answers.
///
/// `heuristic` takes the line the operator ranked first and drives it alone, because the ranking
/// prices a walk and knows nothing about who is standing where. `greedy` sees the rider on the
/// second-ranked line, pays 0.4% more detour for it and carries somebody. It is also the
/// property the demand-following rule is for: no driver sets off for an empty line while a line
/// it would accept has a rider on it.
#[test]
fn the_two_policies_take_different_lines_on_the_same_seed() {
    let heuristic = two_stations("heuristic");
    let greedy = two_stations("greedy");
    assert_eq!(heuristic.policy, PolicyName::Heuristic);
    assert_eq!(greedy.policy, PolicyName::Greedy);

    let (ranking, ranked_metrics) = run(&heuristic, 42);
    let (demand, demand_metrics) = run(&greedy, 42);

    let agent_of = |sim: &Sim, driver: bool| {
        sim.agents()
            .iter()
            .position(|agent| agent.kind.is_driver() == driver)
            .expect("declared")
    };
    let line_of = |sim: &Sim, driver: bool| sim.agents()[agent_of(sim, driver)].line;
    let rider_line = line_of(&ranking, false);
    assert_eq!(
        rider_line,
        line_of(&demand, false),
        "the rider ranks on its own walk and neither policy moves it"
    );

    assert_ne!(
        line_of(&ranking, true),
        rider_line,
        "the ranking sends the driver to the station nearest itself, which is the empty one"
    );
    assert_eq!(
        line_of(&demand, true),
        rider_line,
        "following demand sends it to the station with somebody on it"
    );

    assert_eq!(ranked_metrics.passengers_served, 0);
    assert_eq!(demand_metrics.passengers_served, 1);
    assert_eq!(
        ranking.agents()[agent_of(&ranking, false)].reason,
        TransitionReason::WaitTimedOut,
        "under the ranking the rider stands there until its patience runs out"
    );
}

/// `heuristic` is the rule every run and every committed sweep row was produced under, so it has
/// to reproduce them exactly. The figures are `baseline.json` at seed 42, which is one row of
/// `analysis/results.csv` and the number quoted in `CLAUDE.md`.
#[test]
fn the_heuristic_reproduces_the_committed_baseline_figures() {
    let scenario = load("baseline.json", PolicyName::Heuristic);
    assert_eq!(
        scenario.policy,
        PolicyName::Heuristic,
        "and it is the default, so a scenario that names no policy is unchanged"
    );
    let (_, metrics) = cached(&scenario, 42);
    assert_eq!(metrics.passengers_served, 25);
    assert_eq!(
        metrics.drivers_active, 9,
        "twenty-five people carried in nine of the thirteen vehicles"
    );
    assert!(
        (metrics.vehicle_km_total - 187.8378).abs() < 1e-3,
        "vehicle km total was {}",
        metrics.vehicle_km_total
    );
}

/// A claim is a promise: it tells a rider which station to walk to. So a driver that has claimed
/// somebody stops reconsidering its line, however the demand moves afterwards - otherwise the
/// rider walks to a station the vehicle no longer visits, which is a debug invariant and not a
/// matter of taste.
///
/// `advanced-declaration.json` under `greedy` is where that bites: a declaring rider is in the
/// pool of every line it would accept and stands at none of them, so the demand a driver follows
/// is the pool, and it commits the moment it claims out of one.
#[test]
fn a_driver_that_has_claimed_a_rider_keeps_its_line() {
    let (sim, metrics) = run(&load("advanced-declaration.json", PolicyName::Greedy), 42);
    assert_eq!(metrics.passengers_served, 25, "the pool is visible demand");

    let claimed: Vec<_> = sim
        .agents()
        .iter()
        .filter(|agent| agent.claimed_by.is_some())
        .collect();
    assert!(
        !claimed.is_empty(),
        "the scenario is about riders claimed ahead"
    );
    for agent in &claimed {
        let driver = agent.claimed_by.expect("filtered on it");
        assert_eq!(
            agent.line,
            sim.agents()[driver.0 as usize].line,
            "agent {:?} walked to a line its driver left",
            agent.id
        );
    }
    // A claim reserves no seat, so on a corridor with two-kilometre walks some of them are still
    // coming when their driver runs out of patience. What must not happen is a claimed rider
    // walking to a line the vehicle no longer visits, which is the assertion above.
    assert!(
        claimed.iter().any(|agent| agent.state == State::EndJourney),
        "and the claim carried at least one of them the whole way"
    );
}

/// **Pair scoring's one mechanism, and the thing neither fixed rule can do**: a rider that
/// registered on one line is taken by a driver on another and walks to that driver's station
/// instead.
///
/// `baseline.json` declares no leads at all, so under `heuristic` or `greedy` nothing is ever
/// claimed on it - every match happens where the rider is already standing, on the line it named.
/// Under `pairs` the line is geometry: a driver takes whichever registered rider is still walking
/// and would accept its line against that rider's *own* `max_walk_s`, and `Influence::Claim`
/// re-registers it. So a claim on this scenario is the mechanism firing, and a claimed rider on
/// its driver's line is the invariant that says the diversion arrived somewhere coherent.
#[test]
fn pair_scoring_takes_riders_off_the_line_they_registered_on() {
    for fixed in [PolicyName::Heuristic, PolicyName::Greedy] {
        let (sim, _) = run(&load("baseline.json", fixed), 42);
        assert!(
            sim.agents().iter().all(|agent| agent.claimed_by.is_none()),
            "{fixed:?} keys every match on the line, and nothing on this scenario declares ahead"
        );
    }

    let (sim, metrics) = run(&load("baseline.json", PolicyName::Pairs), 42);
    let diverted: Vec<_> = sim
        .agents()
        .iter()
        .filter(|agent| agent.claimed_by.is_some())
        .collect();
    assert!(
        !diverted.is_empty(),
        "pair scoring diverts riders on a scenario where no cohort declares ahead"
    );
    for agent in &diverted {
        let driver = agent.claimed_by.expect("filtered on it");
        assert_eq!(
            agent.line,
            sim.agents()[driver.0 as usize].line,
            "rider {:?} was diverted onto a line its driver is not driving",
            agent.id
        );
    }
    assert!(
        diverted
            .iter()
            .any(|agent| agent.state == State::EndJourney),
        "and at least one of the diversions carried its rider the whole way"
    );
    assert!(
        metrics.passengers_served > 0,
        "a policy that diverts everybody and serves nobody is a broken one"
    );
}

/// **The measurement behind not assigning pair scoring across the whole fleet.**
///
/// `pairs` is greedy per driver: every driver that could claim somebody reaches for the rider with
/// the shortest remaining walk, so two drivers on one tick can reach for the same one and the apply
/// pass gives it to the first. A fleet-wide assignment would spread them, and this test is what
/// says how much that is worth: the loser re-decides on the very next tick, which is one second of
/// its waiting, so a collision costs a second and never a ride. `heuristic` never claims anybody at
/// all, so it can never collide.
#[test]
fn a_lost_claim_costs_a_tick_and_not_a_ride() {
    let scenario = load("baseline.json", PolicyName::Pairs);
    let (sim, metrics) = cached(&scenario, 42);
    let collisions = sim.claim_collisions();
    // Collisions do happen - otherwise this test would be pinning nothing - and every one of them
    // is a driver that claimed somebody else on a later tick rather than a rider left behind.
    assert!(
        collisions > 0,
        "no claim ever lost a race, so nothing is measured here"
    );
    let served = metrics.passengers_served;

    let (heuristic, _) = cached(&load("baseline.json", PolicyName::Heuristic), 42);
    assert_eq!(
        heuristic.claim_collisions(),
        0,
        "the heuristic claims nobody, so it cannot collide"
    );
    println!("pairs, 13 drivers, seed 42: {collisions} lost claims, {served} served");

    // And it does not run away with the fleet. Ten times the drivers against the same thirty-two
    // riders is the worst case the sweep's own axis reaches - every driver reaching for one of a
    // shrinking pool of divertible riders - and the count stays in the tens.
    let mut crowded = load("baseline.json", PolicyName::Pairs);
    for cohort in crowded.cohorts.iter_mut().take(4) {
        cohort.count *= 10;
    }
    let (crowded_sim, crowded_metrics) = cached(&crowded, 42);
    println!(
        "pairs, 130 drivers, seed 42: {} lost claims, {} served",
        crowded_sim.claim_collisions(),
        crowded_metrics.passengers_served
    );
    assert!(
        crowded_sim.claim_collisions() < 1000,
        "a collision per tick per driver would be a dispatcher problem rather than a second of \
         waiting: {}",
        crowded_sim.claim_collisions()
    );
}
