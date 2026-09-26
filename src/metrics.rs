//! End-of-run aggregation.
//!
//! Every number here is derived from the agents themselves at the end of the run, never
//! accumulated by the tick loop, so the metrics cannot drift from what the event log says
//! happened.
//!
//! A run that hit the clock cap carries `stopped_by_clock` and a nonzero `stranded`. Those two
//! fields are part of the metrics rather than a footnote because a cut-off run's averages are not
//! a finding, and anything reading this file has to be able to tell.
//!
//! **What a model in the loop cost is a column here and never a footnote either.** `usd_per_
//! decision`, `tokens_per_decision`, `decision_latency_ms` and `invalid_action_rate` sit in the
//! same row as `service_rate`, so a sweep comparing a model against a fixed rule produces the
//! mobility delta and its price in one table. Scoring belongs to the engine: these numbers come
//! from the run, and there is no model judging its own work anywhere in this repository, because
//! there is a deterministic ground truth and it gets used.

use serde::Serialize;

use crate::agents::Agent;
use crate::incentives::{CompletedTrip, IncentiveScheme, Money};
use crate::scenario::AgentKind;
use crate::sim::RunSummary;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    pub agents_spawned: u32,
    pub riders_total: u32,
    pub drivers_total: u32,

    /// People set down at their destination, counting each rider's whole party.
    pub passengers_served: u32,
    /// People who gave up waiting, counting whole parties.
    pub passengers_unserved: u32,
    /// `passengers_served` over everyone who asked.
    pub service_rate: f64,

    /// Drivers that carried at least one rider.
    pub drivers_active: u32,

    pub vehicle_km_total: f64,
    /// Vehicle kilometres with at least one picked-up rider aboard.
    pub vehicle_km_loaded: f64,
    pub vehicle_km_empty: f64,
    pub empty_distance_share: f64,

    /// Kilometres travelled by carried people, each rider's party counted in full.
    pub passenger_km: f64,
    /// `passenger_km` over `vehicle_km_total`: people carried per kilometre driven, which is
    /// zero for a service nobody used and rises as vehicles fill.
    pub mean_occupancy: f64,

    /// Mean wait at the departure station, over riders that were picked up. Riders that gave up
    /// are excluded: their wait is `max_wait_s` by construction and would drown the figure.
    pub mean_wait_s: f64,
    /// Mean time from spawning to arriving, over riders that completed their trip.
    pub mean_journey_time_s: f64,

    /// Kilometres a fleet taxi drove with nobody aboard: out to a rider, out to one that had
    /// already given up, and - under a policy that repositions - out to wherever the operator sent
    /// it to wait. **Pure overhead** - unlike a carpool driver's empty kilometres, which were a
    /// trip somebody was making anyway.
    ///
    /// Deliberately not a `taxi_km_empty_repositioning` of its own: the deadhead to a pickup is the
    /// number that exists whether or not anything repositions, and repositioning
    /// lands in the same column rather than in one of its own - it is the same empty kilometre
    /// however it was spent, and splitting it would let a policy look cheap on one column while
    /// spending on the other. The column a repositioning policy has to earn its distance back on
    /// is `mean_time_to_pickup_s`.
    pub taxi_km_empty_to_pickup: f64,
    /// Mean time from hailing to being collected, over riders a fleet taxi carried. The fleet's
    /// counterpart of `mean_wait_s`, separated out so a mixed scenario does not average the two
    /// services together.
    pub mean_time_to_pickup_s: f64,
    /// The share of the fleet that carried anybody at all. A fleet of twenty that served five
    /// trips between four vehicles reads 0.20, which is what says how much of the fleet the
    /// demand actually needed.
    pub fleet_utilisation: f64,

    /// What the incentive scheme paid over every trip that carried somebody. Nothing in the run
    /// responds to it, so it moves on its own and leaves every other column where it was.
    pub incentive_paid: Money,
    /// `incentive_paid` over `passengers_served`: what the service paid per person it carried.
    pub cost_per_passenger_served: f64,

    /// Decisions handed to a model, memo hits included. Zero under a fixed rule, which is how
    /// "`heuristic` makes no calls" is a column rather than a claim.
    pub model_decisions: u64,
    /// Round trips to the sidecar that answered them. The gap to `model_decisions` is what the
    /// memo saved, and it is reported rather than folded away so nobody has to guess the ratio.
    pub model_calls: u64,
    /// What one decision cost the service. Over `model_decisions`, not over `model_calls`: this is
    /// the operator's number, and an operator pays for decisions rather than for round trips.
    pub usd_per_decision: f64,
    /// Input plus output tokens over `model_decisions`.
    pub tokens_per_decision: f64,
    /// Mean wall-clock time per round trip, measured on the engine's side of the pipe so it
    /// includes the round trip rather than only the model's own time.
    pub decision_latency_ms: f64,
    /// The share of decisions the model failed to answer usably, over `model_decisions`.
    ///
    /// **Counted and refused, never retried.** Per decision rather than per call, deliberately: an
    /// action the world cannot do is memoised like any other answer - re-asking the same question
    /// *is* the retry this refuses to do - so every later decision that reads it really was made
    /// by the fallback rule, and the operator's number is how many of its decisions that was. A
    /// high rate means much of a run was the fallback wearing the model's name, which is exactly
    /// why it sits beside the mobility columns instead of inside a retry loop.
    pub invalid_action_rate: f64,

    /// Agents still going when the clock cap hit. Nonzero means the run was cut off.
    pub stranded: u32,
    pub stopped_by_clock: bool,
}

impl Metrics {
    /// `scheme` says what the service pays for the trips the run finished with.
    pub fn collect(
        agents: &[Agent],
        summary: &RunSummary,
        scheme: &dyn IncentiveScheme,
    ) -> Metrics {
        let cost = summary.decisions;
        let riders = || agents.iter().filter(|agent| agent.kind.is_rider());
        let drivers = || agents.iter().filter(|agent| agent.kind.is_driver());

        let passengers_served: u32 = riders().filter(|r| r.arrived()).map(|r| r.seats).sum();
        let passengers_asking: u32 = riders().map(|r| r.seats).sum();

        let vehicle_km_total = total(drivers().map(|d| d.distance_km));
        let vehicle_km_loaded = total(drivers().map(|d| d.loaded_km));
        let vehicle_km_empty = vehicle_km_total - vehicle_km_loaded;

        let passenger_km = total(riders().map(|r| r.loaded_km * r.seats as f64));

        let incentive_paid = CompletedTrip::collect(agents)
            .iter()
            .fold(0.0, |sum, trip| sum + scheme.reward(trip));

        let picked_up: Vec<&Agent> = riders().filter(|r| r.wait_s.is_some()).collect();
        let arrived: Vec<&Agent> = riders().filter(|r| r.arrived()).collect();

        let fleet = || {
            agents
                .iter()
                .filter(|agent| agent.kind == AgentKind::AutonomousTaxi)
        };
        // Riders a fleet taxi actually carried, found through the vehicle that carried them
        // rather than through their own kind, so a mixed scenario splits the two services by who
        // did the driving.
        let by_taxi: Vec<&Agent> = picked_up
            .iter()
            .copied()
            .filter(|rider| {
                rider.carried_by.is_some_and(|driver| {
                    agents[driver.0 as usize].kind == AgentKind::AutonomousTaxi
                })
            })
            .collect();

        Metrics {
            agents_spawned: summary.agents_spawned,
            riders_total: riders().count() as u32,
            drivers_total: drivers().count() as u32,

            passengers_served,
            passengers_unserved: passengers_asking - passengers_served,
            service_rate: ratio(passengers_served as f64, passengers_asking as f64),

            drivers_active: drivers().filter(|d| d.pickups > 0).count() as u32,

            vehicle_km_total,
            vehicle_km_loaded,
            vehicle_km_empty,
            empty_distance_share: ratio(vehicle_km_empty, vehicle_km_total),

            passenger_km,
            mean_occupancy: ratio(passenger_km, vehicle_km_total),

            mean_wait_s: mean(picked_up.iter().map(|r| r.wait_s.unwrap_or(0.0))),
            mean_journey_time_s: mean(arrived.iter().map(|r| r.state_since_s - r.spawn_s)),

            taxi_km_empty_to_pickup: total(fleet().map(|taxi| taxi.distance_km - taxi.loaded_km)),
            mean_time_to_pickup_s: mean(by_taxi.iter().map(|r| r.wait_s.unwrap_or(0.0))),
            fleet_utilisation: ratio(
                fleet().filter(|taxi| taxi.pickups > 0).count() as f64,
                fleet().count() as f64,
            ),

            incentive_paid,
            cost_per_passenger_served: ratio(incentive_paid, passengers_served as f64),

            model_decisions: cost.decisions,
            model_calls: cost.calls,
            usd_per_decision: ratio(cost.usd, cost.decisions as f64),
            tokens_per_decision: ratio(
                (cost.input_tokens + cost.output_tokens) as f64,
                cost.decisions as f64,
            ),
            decision_latency_ms: ratio(cost.latency_ms, cost.calls as f64),
            invalid_action_rate: ratio(cost.invalid as f64, cost.decisions as f64),

            stranded: summary.stranded,
            stopped_by_clock: summary.stopped_by_clock,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Zero rather than a division by zero: a run nobody took part in has a service rate of nothing,
/// not a NaN that poisons every average downstream of it.
fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

/// Folded from zero rather than summed: `f64`'s `Sum` starts at negative zero, and a `-0.0` in
/// the metrics file is a distinction nothing downstream wants to explain.
fn total(values: impl Iterator<Item = f64>) -> f64 {
    values.fold(0.0, |sum, value| sum + value)
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let mut count = 0u32;
    let mut sum = 0.0;
    for value in values {
        sum += value;
        count += 1;
    }
    ratio(sum, count as f64)
}
