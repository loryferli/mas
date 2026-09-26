//! A model in the loop: the seam, the leakage audit, the cost columns, and the four ways this
//! could quietly become a result about nothing.
//!
//! **Nothing here reaches a model.** Every test points the engine at
//! `tests/fixtures/stub-sidecar.py`, which speaks the same protocol and answers from a rule, so
//! the suite stays offline and needs no API key. What is being tested is the engine's side of the
//! pipe, which is where every one of these failures would live: an invalid action retried away, a
//! context carrying something the operator could not know, a dead sidecar silently becoming the
//! heuristic, and a fixed rule that has started making calls.

use mas::agents::AgentId;
use mas::events::EventLog;
use mas::incentives::NoIncentive;
use mas::metrics::Metrics;
use mas::policy::{
    self, AssignContext, DecisionContext, GuaranteeContext, IdleTaxi, LineOption, PolicyName,
    RepositionContext, WaitingPoint, WaitingRider, LEAKAGE_AUDIT,
};
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use mas::world::{Coord, LineId, StationId};
use serde_json::Value as Json;
use std::path::Path;

/// The stub, in one of its three moods. `sh -c` runs it, exactly as a real sidecar command would
/// be run, so the engine's spawn path is the one under test.
fn stub(mode: &str) -> String {
    format!("python3 tests/fixtures/stub-sidecar.py {mode}")
}

fn load(name: &str, policy: PolicyName, sidecar: Option<&str>) -> Scenario {
    let mut scenario = Scenario::load(Path::new("scenarios").join(name).as_path())
        .unwrap_or_else(|error| panic!("loading {name}: {error:#}"));
    scenario.policy = policy;
    scenario.sidecar = sidecar.map(str::to_string);
    scenario
}

fn cached(scenario: &Scenario) -> Router {
    let mut router = Router::new(scenario);
    router
        .load_cache(Path::new("data/routes.json"))
        .expect("the committed route cache");
    router
}

fn run(scenario: &Scenario, seed: u64) -> Metrics {
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(scenario, seed, cached(scenario));
    let summary = sim
        .run(&mut log)
        .unwrap_or_else(|error| panic!("the run failed: {error:#}"));
    Metrics::collect(sim.agents(), &summary, &NoIncentive)
}

/// The rule every committed figure and every row of `analysis/results.csv` was produced under.
/// **It has to make no calls and reproduce those numbers exactly**, or widening the seam has
/// quietly rewritten the baseline every model result is measured against.
#[test]
fn the_heuristic_makes_no_calls_and_reproduces_the_committed_figures() {
    // A sidecar command that could not possibly work, precisely so the test would fail if the
    // fixed rule ever spawned one.
    let scenario = load(
        "baseline.json",
        PolicyName::Heuristic,
        Some("exit 1 # a fixed rule starts no process"),
    );
    let metrics = run(&scenario, 42);

    assert_eq!(metrics.model_decisions, 0);
    assert_eq!(metrics.model_calls, 0);
    assert_eq!(metrics.usd_per_decision, 0.0);
    assert_eq!(metrics.tokens_per_decision, 0.0);
    assert_eq!(metrics.decision_latency_ms, 0.0);
    assert_eq!(metrics.invalid_action_rate, 0.0);

    assert_eq!(metrics.passengers_served, 25);
    assert_eq!(metrics.drivers_active, 9);
    assert!(
        (metrics.vehicle_km_total - 187.8378).abs() < 1e-3,
        "vehicle km total was {}",
        metrics.vehicle_km_total
    );
}

/// The model's answer has to actually reach the world, or every figure a model policy produces is
/// the fallback rule wearing its name.
///
/// The stub answers with the *second*-ranked line rather than the first, which is a line neither
/// fixed rule would take on this corridor. The run therefore has to differ from the heuristic's,
/// and the cost columns have to carry the stub's reported usage through.
#[test]
fn the_models_choice_reaches_the_world_and_its_cost_reaches_the_metrics() {
    let fixed = run(&load("baseline.json", PolicyName::Heuristic, None), 42);
    let model = run(
        &load("baseline.json", PolicyName::Llm, Some(&stub("second-line"))),
        42,
    );

    assert!(model.model_decisions > 0, "the model was asked something");
    assert!(model.model_calls > 0, "and a process answered");
    assert_eq!(
        model.invalid_action_rate, 0.0,
        "every answer was a real line"
    );
    assert_ne!(
        model.vehicle_km_total, fixed.vehicle_km_total,
        "taking the second-ranked line has to move the kilometres driven"
    );

    // 400 in and 30 out per call, at 0.0025 the call, spread over the decisions the memo did not
    // absorb. The point is not the arithmetic: it is that a zero here means the pipeline drops
    // what the model cost, and cost is not a footnote in this phase.
    assert!(model.usd_per_decision > 0.0);
    assert!(model.tokens_per_decision > 0.0);
    assert!(model.decision_latency_ms > 0.0);
}

/// **Nothing per tick and nothing per agent per tick.** A driver at home is asked which line to
/// take on every tick it waits, and there are thousands of ticks: without the memo that is a
/// network call per driver per second, which is a bill rather than a policy.
#[test]
fn a_repeated_question_costs_one_call() {
    let metrics = run(
        &load("baseline.json", PolicyName::Llm, Some(&stub("second-line"))),
        42,
    );
    assert!(
        metrics.model_calls * 20 < metrics.model_decisions,
        "{} calls answered {} decisions, which is not a memo doing any work",
        metrics.model_calls,
        metrics.model_decisions
    );
}

/// `hybrid` is the exception architecture: the fixed rules decide everything they can, and the
/// model is asked only where they disagree with each other. On a corridor where they never do,
/// it must cost nothing at all - no calls, and no process started.
#[test]
fn hybrid_asks_nothing_where_the_two_fixed_rules_agree() {
    let scenario = load(
        "minimal.json",
        PolicyName::Hybrid,
        Some("exit 1 # nothing here should spawn a sidecar"),
    );
    let heuristic = run(&load("minimal.json", PolicyName::Heuristic, None), 42);
    let hybrid = run(&scenario, 42);

    assert_eq!(
        hybrid.model_decisions, 0,
        "one line means one answer, so there is nothing to disagree about"
    );
    assert_eq!(hybrid.passengers_served, heuristic.passengers_served);
    assert_eq!(hybrid.vehicle_km_total, heuristic.vehicle_km_total);
}

/// And on a corridor where they do disagree, it asks - but about the exceptions rather than about
/// everything, so it costs strictly less than `llm` on the same run.
#[test]
fn hybrid_asks_about_the_exceptions_and_llm_about_everything() {
    let hybrid = run(
        &load(
            "baseline.json",
            PolicyName::Hybrid,
            Some(&stub("second-line")),
        ),
        42,
    );
    let llm = run(
        &load("baseline.json", PolicyName::Llm, Some(&stub("second-line"))),
        42,
    );
    assert!(hybrid.model_decisions > 0, "the corridor has exceptions");
    assert!(
        hybrid.model_decisions < llm.model_decisions,
        "hybrid asked about {} decisions and llm about {}",
        hybrid.model_decisions,
        llm.model_decisions
    );
}

/// **An invalid action is counted and refused, never retried away.** The stub names a line that
/// does not exist on every single call.
///
/// Three things have to hold. The rate is reported rather than swallowed; the run falls through to
/// the rule that would have decided, so its mobility figures are the heuristic's; and `calls`
/// never exceeds `decisions`, which is exactly what a retry would break.
#[test]
fn an_invalid_action_is_counted_and_refused_rather_than_retried() {
    let fixed = run(&load("baseline.json", PolicyName::Heuristic, None), 42);
    let broken = run(
        &load("baseline.json", PolicyName::Llm, Some(&stub("invalid"))),
        42,
    );

    assert!(broken.model_calls > 0, "the sidecar was asked");
    assert_eq!(
        broken.invalid_action_rate, 1.0,
        "every answer was something the world cannot do, and every one was counted"
    );
    assert!(
        broken.model_calls <= broken.model_decisions,
        "{} calls for {} decisions is a retry loop",
        broken.model_calls,
        broken.model_decisions
    );
    assert_eq!(
        broken.passengers_served, fixed.passengers_served,
        "a refused action falls through to the rule that would have decided"
    );
    assert!((broken.vehicle_km_total - fixed.vehicle_km_total).abs() < 1e-9);
}

/// **A dead sidecar fails the run, loudly.** Falling back to the heuristic and finishing would
/// publish the heuristic's numbers under the model's name, which is the single worst thing this
/// code could do - and the one failure a reader of the results could never detect.
#[test]
fn a_sidecar_that_dies_mid_run_fails_the_run() {
    let scenario = load("baseline.json", PolicyName::Llm, Some(&stub("die")));
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, 42, cached(&scenario));
    let failure = sim
        .run(&mut log)
        .expect_err("a run whose policy cannot answer is not a run");
    let message = format!("{failure:#}");
    assert!(
        message.contains("sidecar"),
        "the failure has to name the sidecar, not read as a modelling error: {message}"
    );
}

/// A sidecar that will not start at all is the same failure at the other end of the run.
#[test]
fn a_sidecar_that_cannot_be_reached_fails_the_run() {
    let scenario = load(
        "baseline.json",
        PolicyName::Llm,
        Some("exec /nonexistent/sidecar"),
    );
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, 42, cached(&scenario));
    assert!(
        sim.run(&mut log).is_err(),
        "a sidecar that never answers must not read as a policy that decided something"
    );
}

/// The guarantee is the one decision whose fixed answer was always "yes". A policy that can hold
/// the operator's vehicle back has to actually hold it back - and the withheld vehicle has to cost
/// the rider its ride, which is the whole of what the decision is worth deciding.
///
/// `departure-guarantee.json` is the corridor's longest, thinnest line: one rider waits out the
/// 1200 s trigger and is carried by the vehicle the trigger calls. Decline, and nobody comes.
#[test]
fn a_policy_can_hold_the_operators_guarantee_vehicle_back() {
    let sent = run(
        &load(
            "departure-guarantee.json",
            PolicyName::Llm,
            Some(&stub("second-line")),
        ),
        42,
    );
    let held = run(
        &load(
            "departure-guarantee.json",
            PolicyName::Llm,
            Some(&stub("decline")),
        ),
        42,
    );

    assert_eq!(sent.passengers_served, 1, "the trigger calls a vehicle");
    assert!(sent.vehicle_km_total > 25.0, "and it drives the 23 km line");
    assert_eq!(
        held.passengers_served, 0,
        "declining is a real decision: nobody comes, and the rider gives up"
    );
    assert_eq!(
        held.vehicle_km_total, 0.0,
        "and the service spends nothing, which is what it bought"
    );
}

/// Repositioning is `14b`'s decision and the one with **no fixed rule to fall back on**: the
/// engine's answer has always been "stand where you were released". So the test is that a policy
/// which repositions moves the fleet's empty kilometres at all, and that a fixed rule still does
/// not move them.
#[test]
fn repositioning_moves_the_fleets_empty_kilometres_and_a_fixed_rule_does_not() {
    let fixed = run(&load("fleet.json", PolicyName::Heuristic, None), 42);
    let greedy = run(&load("fleet.json", PolicyName::Greedy, None), 42);
    assert_eq!(
        fixed.taxi_km_empty_to_pickup, greedy.taxi_km_empty_to_pickup,
        "neither fixed rule repositions, so the fleet's deadhead is the same under both"
    );

    let model = run(
        &load("fleet.json", PolicyName::Llm, Some(&stub("second-line"))),
        42,
    );
    assert!(
        model.taxi_km_empty_to_pickup > fixed.taxi_km_empty_to_pickup,
        "sending idle vehicles somewhere costs empty kilometres: {} against {}",
        model.taxi_km_empty_to_pickup,
        fixed.taxi_km_empty_to_pickup
    );
}

/// **The serialised context, asserted field by field against the committed audit.**
///
/// This is the difference between a result and a fabrication. A policy that can see a future
/// arrival, an undeclared trip or a metric value beats every real dispatcher, and no figure it
/// produces means anything. So the audit is a committed list rather than a paragraph, and a field
/// added to any context fails this test until somebody has written it down there and looked at it.
#[test]
fn the_serialised_context_is_exactly_the_leakage_audit() {
    let wired = [
        ("line", policy::wire_line(&a_line_context())),
        ("guarantee", policy::wire_guarantee(&a_guarantee_context())),
        ("assign", policy::wire_assign(&an_assign_context())),
        (
            "reposition",
            policy::wire_reposition(&a_reposition_context()),
        ),
    ];
    assert_eq!(
        wired.len(),
        LEAKAGE_AUDIT.len(),
        "every decision the model is asked about has an audited context"
    );

    for (decision, context) in &wired {
        let (_, audited) = LEAKAGE_AUDIT
            .iter()
            .find(|(name, _)| name == decision)
            .unwrap_or_else(|| panic!("{decision} has no entry in the leakage audit"));
        let mut sent = Vec::new();
        leaves(context, "", &mut sent);
        sent.sort();
        sent.dedup();
        assert_eq!(
            sent,
            audited.to_vec(),
            "what the {decision} decision sends is not what the audit says it sends"
        );
    }

    // And the audit itself has to be readable, not a wildcard: a path naming something the model
    // could not know would pass the comparison above and fail here.
    for (decision, audited) in LEAKAGE_AUDIT {
        for path in audited {
            assert!(
                !path.contains("spawn")
                    && !path.contains("future")
                    && !path.contains("service_rate")
                    && !path.contains("objective"),
                "{decision} sends {path}, which is not knowable at decision time"
            );
        }
    }
}

/// Every leaf path in a JSON object, with `[]` for an array of objects - so a nested field is a
/// field the audit has to name rather than one hidden inside an object it already named.
fn leaves(value: &Json, prefix: &str, into: &mut Vec<String>) {
    match value {
        Json::Object(fields) => {
            for (name, field) in fields {
                let path = match prefix.is_empty() {
                    true => name.clone(),
                    false => format!("{prefix}.{name}"),
                };
                leaves(field, &path, into);
            }
        }
        Json::Array(items) => {
            for item in items {
                leaves(item, &format!("{prefix}[]"), into);
            }
        }
        _ => into.push(prefix.to_string()),
    }
}

fn a_line_context() -> DecisionContext {
    DecisionContext {
        time_s: 600.0,
        options: vec![
            LineOption {
                line: LineId(0),
                detour_pct: 104.5,
                riders_waiting: 0,
                best_walk_share: None,
            },
            LineOption {
                line: LineId(1),
                detour_pct: 131.2,
                riders_waiting: 3,
                best_walk_share: Some(0.42),
            },
        ],
        max_detour_pct: Some(150.0),
    }
}

fn a_guarantee_context() -> GuaranteeContext {
    GuaranteeContext {
        line: "Laura House to Aberystwyth".to_string(),
        riders_triggered: 1,
        longest_wait_s: 1260.0,
        patience_spent: 0.7,
        drivers_inbound: 0,
        trigger_after_wait_s: 1200.0,
        cooldown_s: 600.0,
        depot_to_origin_km: 2.4,
        vehicles_sent: 0,
    }
}

fn an_assign_context() -> AssignContext {
    AssignContext {
        taxis: vec![IdleTaxi {
            id: AgentId(0),
            position: Coord::new(52.4161, -4.0802),
            free_seats: 3,
        }],
        riders: vec![WaitingRider {
            id: AgentId(1),
            position: Coord::new(52.4588, -4.0159),
            destination: Coord::new(52.4161, -4.0802),
            seats: 1,
            patience_spent: 0.25,
            neighbours: 0,
        }],
    }
}

fn a_reposition_context() -> RepositionContext {
    RepositionContext {
        taxis: vec![IdleTaxi {
            id: AgentId(0),
            position: Coord::new(52.4161, -4.0802),
            free_seats: 4,
        }],
        stations: vec![WaitingPoint {
            station: StationId(0),
            name: "Bow Street".to_string(),
            coord: Coord::new(52.4399, -4.0304),
        }],
        pickups_by_station: vec![7],
    }
}

/// The one thing the line context holds and never sends. The clock is knowable, so it belongs in
/// the audited context - but a question carrying it is a different question every tick, and a
/// question that cannot be memoised is a call per driver per tick.
#[test]
fn the_wall_clock_is_never_sent() {
    let mut early = a_line_context();
    early.time_s = 0.0;
    let mut late = a_line_context();
    late.time_s = 13_999.0;
    assert_eq!(
        policy::wire_line(&early),
        policy::wire_line(&late),
        "two ticks with the same demand have to be the same question, or the memo never hits"
    );
}
