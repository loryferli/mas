//! The operational decisions, behind a trait so they can be swapped - including for a model.
//!
//! Four decisions go through here, and every one of them is the dispatcher's rather than the
//! agent's: **which line a driver takes**, **whether to spend a guarantee vehicle on a rider that
//! has waited too long**, **which idle taxi collects which waiting rider**, and **where an idle
//! taxi waits**. They are what a study of a low-flow service is actually comparing, which is why
//! they sit behind [`Policy`] rather than inside a state machine.
//!
//! Everything else stays a behaviour and stays in `agents/`: a rider's walk refusal, a driver's
//! patience, the detour it will accept. A policy chooses between options an agent would accept,
//! it does not overrule the agent.
//!
//! Three of the four have a default implementation on the trait, and each default is exactly what
//! the engine did before the seam widened - dispatch the guarantee, assign greedily, never
//! reposition. That is what keeps [`Heuristic`] and [`Greedy`] reproducing every committed figure
//! byte for byte while [`Model`] overrides all four.
//!
//! **Every context here is audited for leakage, and the audit is committed** as
//! [`LEAKAGE_AUDIT`] rather than left in prose. Nothing a context carries is unknowable to the
//! deciding party at decision time: no future spawn, no undeclared request, no realised demand,
//! no metric value, no objective function. A policy that could see any of those would beat any
//! real dispatcher and the comparison would be worth nothing, which is the classic way a
//! simulation result turns out to be no result at all.
//!
//! `decide` takes `&mut self` because a policy is allowed to be stateful - [`Model`] holds a pipe
//! to a sidecar and a memo of what it has already asked. Neither fixed rule keeps state, and
//! neither draws randomness: under those two a run stays a pure function of its scenario, its
//! seed and the cache. Under a model it is not, which is why a comparison runs k seeds and reports
//! the band.

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{json, Value as Json};

use crate::agents::AgentId;
use crate::llm::{DecisionCost, Sidecar};
use crate::world::{Coord, LineId, StationId};

/// What a rider at the end of its patience is worth in extra driving, in kilometres.
///
/// This is the whole of how waiting is traded against distance in the fleet's greedy assignment:
/// a rider that has waited none of its patience is priced at the kilometres to reach it, and one
/// that has waited all of it is priced at that minus this. A calibration knob - it is a rate of
/// exchange between two units and no measurement fixes it - and the corridor's longest line is
/// 23 km, so ten is "worth going most of the way across the study area for". `urgency` normalises
/// a weight matrix and mixes it 0.8 to 0.2, which is the same arbitrary choice spelled longer.
///
/// ponytail: a constant rather than a scenario knob. Make it one the day a sweep wants to move it.
const URGENCY_KM: f64 = 10.0;

// ---------------------------------------------------------------------------------------------
// the line choice
// ---------------------------------------------------------------------------------------------

/// One line a driver could take, and what taking it would cost.
///
/// Built by [`crate::operator::Operator::line_options`], in the operator's own ranking order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineOption {
    pub line: LineId,
    /// The trip this line turns the driver's own into, as a percentage of driving straight there.
    /// 100 is a line that costs nothing to serve.
    pub detour_pct: f64,
    /// Riders the operator can offer this driver on this line **right now**: registered on it and
    /// either standing at its origin station or still in its pool having declared ahead. Never a
    /// trip nobody has declared yet.
    pub riders_waiting: usize,
    /// The rider half of a pair score: of the riders that could still be sent to this line, the
    /// shortest remaining walk, as a share of that rider's **own** `max_walk_s`. `None` under any
    /// policy that keys a match on the line, because then there is no pair to score - a rider
    /// belongs to the line it registered on and nothing may move it.
    ///
    /// A share rather than seconds so it is comparable across riders that disagree about how far
    /// they will walk, and so it sits in the same unit as the driver's own half of the score:
    /// each party's cost measured against the threshold that party declared.
    pub best_walk_share: Option<f64>,
}

/// What a policy is allowed to know when a driver chooses a line.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionContext {
    /// Deliberately **not** sent to a model. The clock is knowable, so it belongs in the audited
    /// context, but a question that carries it is a different question every tick and could never
    /// be memoised - and a decision that cannot be memoised is a network call per driver per
    /// tick, which is a bill rather than a policy. Nothing in the choice needs it: the waits and
    /// the demand are already relative.
    pub time_s: f64,
    /// Every line, in the operator's ranking order: quickest trip first.
    pub options: Vec<LineOption>,
    /// The driver's own refusal - the most detour it will accept, as a percentage of driving
    /// straight there. `None` accepts any. A policy may not overrule it, which is what
    /// [`DecisionContext::accepts`] is for.
    pub max_detour_pct: Option<f64>,
}

impl DecisionContext {
    /// Whether the driver would accept this line at all. The operator ranks and never refuses, so
    /// the refusal is the driver's own and every policy is subject to it.
    pub fn accepts(&self, option: &LineOption) -> bool {
        self.max_detour_pct
            .is_none_or(|limit_pct| option.detour_pct <= limit_pct)
    }

    /// The acceptable options, in ranking order.
    fn acceptable(&self) -> impl Iterator<Item = &LineOption> {
        self.options.iter().filter(|option| self.accepts(option))
    }
}

/// What a policy decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Take this line: register on it, drive to its origin station and pick up there.
    TakeLine(LineId),
    /// No line is worth it. Drive the trip the driver was making anyway, carrying nobody - which
    /// is what a driver that never signed up does, rather than vanishing off the road.
    DriveOwnTrip,
}

// ---------------------------------------------------------------------------------------------
// the guarantee
// ---------------------------------------------------------------------------------------------

/// What the operator knows when it decides whether to send a guarantee vehicle now.
///
/// The decision the engine used to make unconditionally: a rider on this line has waited past the
/// trigger, no vehicle is already coming and the cooldown has elapsed, so one goes. A vehicle
/// costs the service every kilometre it covers, and a regular driver may be minutes away, so
/// *whether* to spend it is a judgement a fixed threshold makes badly by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct GuaranteeContext {
    /// The line, as the two station names a reader would use.
    pub line: String,
    /// Riders on this line that have waited past the trigger.
    pub riders_triggered: usize,
    /// The longest of those waits, in seconds.
    pub longest_wait_s: f64,
    /// How much of its patience the longest-waiting rider has spent, from zero to one. A rider at
    /// one is about to give up whatever anybody does.
    pub patience_spent: f64,
    /// Drivers registered on this line and still on their way to its origin station: the demand's
    /// own answer, which a vehicle sent now may duplicate.
    pub drivers_inbound: usize,
    pub trigger_after_wait_s: f64,
    pub cooldown_s: f64,
    /// What the vehicle would cover empty just to reach the line's origin station.
    pub depot_to_origin_km: f64,
    /// Guarantee vehicles already sent on this line during this run.
    pub vehicles_sent: u32,
}

// ---------------------------------------------------------------------------------------------
// the fleet
// ---------------------------------------------------------------------------------------------

/// An idle fleet taxi, as the operator sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdleTaxi {
    pub id: AgentId,
    pub position: Coord,
    pub free_seats: u32,
}

/// A fleet rider standing where it is, hailed and not yet assigned to anybody.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaitingRider {
    pub id: AgentId,
    pub position: Coord,
    pub destination: Coord,
    pub seats: u32,
    /// How much of its patience it has spent, from zero to one.
    pub patience_spent: f64,
    /// Other fleet riders waiting within a hundred metres of it whose leg ends where its does: the
    /// riders one taxi would collect along with it. Knowable at decision time - they have all
    /// hailed - and read only by `urgency`.
    pub neighbours: u32,
}

/// What the operator knows when it pairs idle taxis with waiting riders.
///
/// Built only on the ticks where there is a pairing to make at all - two filters over the arena
/// and an early return if either side is empty - which is what keeps a model call off every tick.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignContext {
    pub taxis: Vec<IdleTaxi>,
    pub riders: Vec<WaitingRider>,
}

/// One taxi told to collect one rider. Indices into [`AssignContext`], resolved to arena ids by
/// the caller, so the action space is the pairing and never an arena slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pairing {
    pub taxi: usize,
    pub rider: usize,
    /// Whether the taxi will actually come. False only under `urgency-as-ran`, for a rider whose
    /// assignment a later one in the same decision overwrote: it is told a taxi is coming - its
    /// patience stretches and nobody else is sent - and none ever does.
    pub collected: bool,
}

impl Pairing {
    pub fn new(taxi: usize, rider: usize) -> Pairing {
        Pairing {
            taxi,
            rider,
            collected: true,
        }
    }
}

/// A boarding point, as somewhere an idle taxi could wait.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitingPoint {
    pub station: StationId,
    pub name: String,
    pub coord: Coord,
}

/// What the operator knows when it decides where its idle vehicles should wait.
///
/// There is no heuristic here at all - the engine's answer has always been "stand where you were
/// released" - so a model competing on this is competing against doing nothing, and the write-up
/// has to say so before a reader works it out. `pickups_by_station` is the run's **realised
/// past**, which an operator plainly knows; there is deliberately no forecast, no spawn queue and
/// no clock, both because those would be leakage and because a context without them is a context
/// that changes only when something happens.
#[derive(Debug, Clone, PartialEq)]
pub struct RepositionContext {
    /// Taxis parked with nothing to do.
    pub taxis: Vec<IdleTaxi>,
    pub stations: Vec<WaitingPoint>,
    /// Riders collected so far nearest each station, in `stations` order.
    pub pickups_by_station: Vec<u32>,
}

/// One idle taxi sent to wait somewhere. Indices into [`RepositionContext`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub taxi: usize,
    pub station: usize,
}

// ---------------------------------------------------------------------------------------------
// the trait
// ---------------------------------------------------------------------------------------------

pub trait Policy {
    /// Which line this driver takes, of the ones it would accept.
    fn decide(&mut self, context: &DecisionContext) -> Result<Action>;

    /// Whether to send a guarantee vehicle now. The default is `true`: what the operator did
    /// before this was a decision, so every committed guarantee run is untouched.
    fn dispatch_guarantee(&mut self, context: &GuaranteeContext) -> Result<bool> {
        let _ = context;
        Ok(true)
    }

    /// Which idle taxi collects which waiting rider. The default is the greedy pricing that has
    /// always been the fleet operator's rule.
    fn assign_fleet(&mut self, context: &AssignContext) -> Result<Vec<Pairing>> {
        Ok(greedy_pairs(context))
    }

    /// Whether this policy matches `(driver, rider)` pairs across lines rather than keying a
    /// match on the line both parties registered on. `false` leaves the operator's queries and
    /// every agent's machine exactly as they were, which is what keeps the two fixed rules
    /// reproducing every committed figure.
    fn matches_pairs(&self) -> bool {
        false
    }

    /// Whether this policy repositions at all. `false` lets the tick loop skip building a context
    /// it would only throw away, which is what keeps a fleet run under a fixed rule as cheap as
    /// it was.
    fn repositions(&self) -> bool {
        false
    }

    /// Where the idle vehicles should wait. The default moves nothing.
    fn reposition(&mut self, context: &RepositionContext) -> Result<Vec<Move>> {
        let _ = context;
        Ok(Vec::new())
    }

    /// What the model cost. Zero for a policy that never calls one.
    fn cost(&self) -> DecisionCost {
        DecisionCost::default()
    }
}

/// Which policy a run uses. A scenario field rather than a bare command-line flag, so it is a
/// JSON pointer the sweep can patch and the rules can be compared in one table; `--policy`
/// overrides it for a single run.
///
/// ponytail: `ValueEnum` derived here rather than a second copy of the same variants in
/// `main.rs`. `clap` is already a dependency of the crate, and the mapping was the whole of what
/// the copy would have added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum PolicyName {
    /// The line the operator ranked quickest, subject to the driver's detour bound.
    #[default]
    Heuristic,
    /// The acceptable line with riders on it, nearest first.
    Greedy,
    /// The line as geometry rather than as the match key: a driver takes riders that would accept
    /// its line whatever line they registered on, and each side is weighed against its own
    /// threshold.
    Pairs,
    /// `heuristic` everywhere but the fleet, where riders are weighed by how many share their kerb
    /// and how close they are to giving up before the taxis are, and a rider's neighbours ride with
    /// it.
    Urgency,
    /// `urgency` with its original loop, which could hand one taxi several riders and collect only
    /// the last: the rule the published fleet runs were made under.
    #[serde(rename = "urgency-as-ran")]
    UrgencyAsRan,
    /// The model on every decision there is.
    Llm,
    /// The model only where the two fixed rules disagree, and on the three decisions no fixed
    /// rule covers.
    Hybrid,
}

impl PolicyName {
    /// Whether a run under this policy reaches a model, and therefore needs a sidecar.
    pub fn needs_model(self) -> bool {
        matches!(self, PolicyName::Llm | PolicyName::Hybrid)
    }
}

pub fn build(name: PolicyName, sidecar: &str) -> Box<dyn Policy> {
    match name {
        PolicyName::Heuristic => Box::new(Heuristic),
        PolicyName::Greedy => Box::new(Greedy),
        PolicyName::Pairs => Box::new(Pairs),
        PolicyName::Urgency => Box::new(Urgency),
        PolicyName::UrgencyAsRan => Box::new(UrgencyAsRan),
        PolicyName::Llm => Box::new(Model::new(sidecar, false)),
        PolicyName::Hybrid => Box::new(Model::new(sidecar, true)),
    }
}

// ---------------------------------------------------------------------------------------------
// the two fixed rules
// ---------------------------------------------------------------------------------------------

/// The option with the least detour. `min_by` keeps the first of equals and the options arrive in
/// the operator's ranking order, so a tie resolves the same way on every run.
fn least_detour<'a>(options: impl Iterator<Item = &'a LineOption>) -> Option<&'a LineOption> {
    options.min_by(|left, right| left.detour_pct.total_cmp(&right.detour_pct))
}

/// Take the first line the operator ranked that the detour bound allows.
///
/// What every driver did before the seam existed, unchanged, which is why every run and every
/// committed sweep row still reproduces. It keeps the **shared yardstick**: the ranking prices
/// both flanking legs on foot for both parties, so driver and rider name the same station because
/// they read the same number, and this policy is the one that depends on that.
///
/// It reads nothing that moves while a driver waits at home, so re-asking it every tick returns
/// the same line every tick: the driver registers once and commits at spawn.
#[derive(Debug, Clone, Copy)]
pub struct Heuristic;

impl Policy for Heuristic {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        Ok(heuristic_line(context))
    }
}

fn heuristic_line(context: &DecisionContext) -> Action {
    match context.acceptable().next() {
        Some(option) => Action::TakeLine(option.line),
        None => Action::DriveOwnTrip,
    }
}

/// Follow the demand: of the lines this driver would accept, the nearest one that has riders
/// waiting on it, and the nearest one of all when nobody is waiting anywhere.
///
/// This is what removes the shared yardstick rather than working around it. The driver never
/// reads the ranking's `cost_s`, which prices a rider's walk it is never going to take; it ranks
/// on its own detour, in kilometres, and on who is actually standing at a station. Riders go on
/// ranking by their own walking time, and the two sides no longer have to agree on a number,
/// because only one side chooses the meeting point.
///
/// Because demand moves while a driver waits at home, this answer moves with it: the driver is
/// asked again every tick until it sets off, and switches lines rather than being committed to a
/// choice made at spawn.
///
/// ponytail: greedy per driver, with no view of what the rest of the fleet is doing, so two
/// drivers can pile onto the same waiting rider. That is what makes it a baseline rather than a
/// dispatcher. Score `(driver, rider)` pairs across the fleet when a real corridor shows service
/// left on the table.
#[derive(Debug, Clone, Copy)]
pub struct Greedy;

impl Policy for Greedy {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        Ok(greedy_line(context))
    }
}

fn greedy_line(context: &DecisionContext) -> Action {
    let with_riders = least_detour(
        context
            .acceptable()
            .filter(|option| option.riders_waiting > 0),
    );
    let choice = with_riders.or_else(|| least_detour(context.acceptable()));
    match choice {
        Some(option) => Action::TakeLine(option.line),
        None => Action::DriveOwnTrip,
    }
}

/// Drop the line as the key and score `(driver, rider)` pairs directly.
///
/// The third fixed rule, and the one the other two are both short of. Under `heuristic` and
/// `greedy` a rider registers on one line and can only ever be found by a driver on that same
/// line: a rider whose best line has no driver, and whose second has one, is a ride the service
/// never makes. Here the line is demoted to geometry. A driver takes any registered rider that is
/// still walking and would accept the line the driver is on - its own `max_walk_s` measured from
/// where it has got to - and the rider is diverted to that station instead.
///
/// **Each side is scored in its own unit against its own threshold, which is the whole point of
/// dropping the shared yardstick.** The driver picks its line on detour, in kilometres of its own
/// driving, exactly as [`Greedy`] does; the rider is picked on the walk it would still have to
/// make, in seconds, against the `max_walk_s` it declared. Neither number has to mean anything to
/// the other party, because neither party is computing the other's.
///
/// ponytail: still greedy per driver, with no view of what the rest of the fleet is doing - two
/// drivers can reach for the same rider and the apply pass gives it to the first. A fleet-wide
/// assignment is the same computation as [`greedy_pairs`] over a different pool; write it when a
/// sweep shows drivers colliding often enough to matter.
///
/// ponytail: a rider is diverted at most once - a claim sets `claimed_by` and nothing clears it -
/// so it never ping-pongs between stations, and a driver that leaves without it does not free it
/// for somebody else. Clear the claim on a driver's departure if a sweep shows riders stranded by
/// a driver that gave up on them.
#[derive(Debug, Clone, Copy)]
pub struct Pairs;

impl Policy for Pairs {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        Ok(pair_line(context))
    }

    fn matches_pairs(&self) -> bool {
        true
    }
}

/// The acceptable line whose best `(driver, rider)` pair is cheapest, and [`greedy_line`] when no
/// line has a pair to score.
///
/// Each half of the score is that party's own cost against its own threshold, and the two are
/// added: the driver's extra driving as a fraction of going straight there, the rider's remaining
/// walk as a fraction of the walk it said it would accept. Adding them is a rate of exchange and
/// no measurement fixes it - the same kind of choice as [`URGENCY_KM`], and the same answer: one
/// to one until a sweep wants to move it.
///
/// ponytail: the two halves are weighed equally. Weight them the day a sweep asks what a service
/// that cared more about the walk than the detour would look like.
fn pair_line(context: &DecisionContext) -> Action {
    let scored = context
        .acceptable()
        .filter_map(|option| {
            option
                .best_walk_share
                .map(|share| (option, (option.detour_pct - 100.0) / 100.0 + share))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1));
    match scored {
        Some((option, _cost)) => Action::TakeLine(option.line),
        // Nobody to pair with anywhere. Follow the demand the operator can see, exactly as
        // `greedy` would: there is no pair scoring to do and the line is a line again.
        None => greedy_line(context),
    }
}

/// The fleet operator's own rule: idle taxis to waiting riders, greedily, lowest cost first.
///
/// **The heuristic to beat, written down as a baseline rather than as a truth.** Each candidate
/// pair costs the kilometres between them, discounted by how much of its patience the rider has
/// already spent; pairs are taken lowest cost first and neither party is used twice.
pub fn greedy_pairs(context: &AssignContext) -> Vec<Pairing> {
    // Each rider's own candidates, cheapest first by cost then taxi. Only its `riders` cheapest can
    // ever be the one it gets - at most one fewer than that are taken by the other riders first -
    // so the rest are dropped before the global sort. The pairing is exactly the one the full
    // sort makes, at a fraction of the cost on a fleet of thousands.
    let keep = context.riders.len();
    let by_cost = |left: &(f64, usize, usize), right: &(f64, usize, usize)| {
        left.0.total_cmp(&right.0).then(left.1.cmp(&right.1))
    };
    let mut priced: Vec<(f64, usize, usize)> = Vec::new();
    for (rider_index, rider) in context.riders.iter().enumerate() {
        let mut candidates: Vec<(f64, usize, usize)> = context
            .taxis
            .iter()
            .enumerate()
            .filter(|(_, taxi)| taxi.free_seats >= rider.seats)
            .map(|(taxi_index, taxi)| {
                let approach_km = crate::world::haversine_km(taxi.position, rider.position);
                (
                    approach_km - URGENCY_KM * rider.patience_spent,
                    taxi_index,
                    rider_index,
                )
            })
            .collect();
        if candidates.len() > keep {
            candidates.select_nth_unstable_by(keep - 1, by_cost);
            candidates.truncate(keep);
        }
        priced.extend(candidates);
    }
    // Cheapest first, and the two indices break a tie, so the pairing never depends on the sort's
    // own partitioning.
    priced.sort_by(|left, right| {
        left.0
            .total_cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });

    // Flags rather than a scan of the pairs made so far: on a fleet of thousands that scan is the
    // whole cost of a decision.
    let mut taxi_used = vec![false; context.taxis.len()];
    let mut rider_used = vec![false; context.riders.len()];
    let mut pairs: Vec<Pairing> = Vec::new();
    for (_cost, taxi, rider) in priced {
        if !taxi_used[taxi] && !rider_used[rider] {
            taxi_used[taxi] = true;
            rider_used[rider] = true;
            pairs.push(Pairing::new(taxi, rider));
            if pairs.len() == context.riders.len() {
                break;
            }
        }
    }
    pairs
}

/// The sixth fixed rule: `heuristic`'s line, and a fleet assignment that weighs riders rather than
/// distances.
///
/// A rider's weight is how many neighbours share its kerb and its next stop, plus an urgency that
/// rises from zero to three as the rider nears the end of its patience - flat until two thirds of it
/// is spent, then exponentially. Weights are scaled by the largest, distances by the longest, and a
/// pair costs `0.2 x distance + 0.8 x (1 - weight)`; the cheapest pair is made, the rider's
/// neighbours are sent the same taxi, and the rest are priced again. Riders therefore go first by
/// crowd and urgency and only then by how far away the taxi is, which is the opposite emphasis to
/// [`greedy_pairs`].
///
/// Each taxi is used once per decision. The rule's original loop could hand a taxi a second rider
/// before it had moved - it was still idle - and only the last one it was handed was ever
/// collected; that is `urgency-as-ran`, not this.
#[derive(Debug, Clone, Copy)]
pub struct Urgency;

impl Policy for Urgency {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        Ok(heuristic_line(context))
    }

    fn assign_fleet(&mut self, context: &AssignContext) -> Result<Vec<Pairing>> {
        Ok(urgency_pairs(context, false))
    }
}

/// `urgency` as the published runs had it: a taxi handed a rider is still idle for the rest of the
/// decision, so it can be handed another, and it collects only the last. Every rider it was handed
/// before is left believing a taxi is coming - offered to nobody else, its patience stretched - and
/// gives up in the end. Kept so those runs can be reproduced; never the rule to read a result off.
#[derive(Debug, Clone, Copy)]
pub struct UrgencyAsRan;

impl Policy for UrgencyAsRan {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        Ok(heuristic_line(context))
    }

    fn assign_fleet(&mut self, context: &AssignContext) -> Result<Vec<Pairing>> {
        Ok(urgency_pairs(context, true))
    }
}

/// How near a neighbour stands: the same kerb.
pub const NEIGHBOUR_KM: f64 = 0.1;

/// `reuse_taxis` is the original rule's own loop, kept as `urgency-as-ran`: see [`UrgencyAsRan`].
pub fn urgency_pairs(context: &AssignContext, reuse_taxis: bool) -> Vec<Pairing> {
    let riders = &context.riders;
    let neighbours_of = |rider: usize| -> Vec<usize> {
        (0..riders.len())
            .filter(|&other| other != rider)
            .filter(|&other| {
                crate::world::haversine_km(riders[other].position, riders[rider].position)
                    <= NEIGHBOUR_KM
                    && crate::world::haversine_km(
                        riders[other].destination,
                        riders[rider].destination,
                    ) <= crate::agents::SAME_PLACE_KM
            })
            .collect()
    };
    let mut assigned = vec![false; riders.len()];
    let mut used = vec![false; context.taxis.len()];
    let mut pairs: Vec<Pairing> = Vec::new();
    loop {
        let open: Vec<usize> = (0..riders.len()).filter(|&r| !assigned[r]).collect();
        let free: Vec<usize> = (0..context.taxis.len()).filter(|&t| !used[t]).collect();
        if open.is_empty() || free.is_empty() {
            break;
        }
        let weight = |rider: &WaitingRider| {
            let urgency = 3.0 * (((rider.patience_spent - 2.0 / 3.0).min(0.0)) * 2.0).exp();
            rider.neighbours as f64 + urgency
        };
        let weights: Vec<f64> = open.iter().map(|&r| weight(&riders[r])).collect();
        let heaviest = weights.iter().copied().fold(0.0, f64::max);
        let farthest = open
            .iter()
            .flat_map(|&r| {
                free.iter().map(move |&t| {
                    crate::world::haversine_km(riders[r].position, context.taxis[t].position)
                })
            })
            .fold(0.0, f64::max);
        let mut best: Option<(f64, usize, usize)> = None;
        for (row, &rider) in open.iter().enumerate() {
            let weight_share = match heaviest > 0.0 {
                true => weights[row] / heaviest,
                false => 0.0,
            };
            for &taxi in &free {
                if context.taxis[taxi].free_seats < riders[rider].seats {
                    continue;
                }
                let km = crate::world::haversine_km(
                    riders[rider].position,
                    context.taxis[taxi].position,
                );
                let distance_share = match farthest > 0.0 {
                    true => km / farthest,
                    false => 0.0,
                };
                let cost = 0.2 * distance_share + 0.8 * (1.0 - weight_share);
                // Strictly less, scanning riders then taxis: the first of equal costs wins.
                if best.is_none_or(|(lowest, _, _)| cost < lowest) {
                    best = Some((cost, rider, taxi));
                }
            }
        }
        let Some((_, rider, taxi)) = best else {
            break;
        };
        match reuse_taxis {
            // The original's loop: the taxi stays idle until it moves, so the same decision can
            // hand it another rider, and only the last one it was handed is collected.
            true => {
                for earlier in pairs.iter_mut().filter(|pair| pair.taxi == taxi) {
                    earlier.collected = false;
                }
            }
            false => used[taxi] = true,
        }
        assigned[rider] = true;
        pairs.push(Pairing::new(taxi, rider));
        for neighbour in neighbours_of(rider) {
            if !assigned[neighbour] {
                assigned[neighbour] = true;
                pairs.push(Pairing::new(taxi, neighbour));
            }
        }
    }
    pairs
}

// ---------------------------------------------------------------------------------------------
// the leakage audit
// ---------------------------------------------------------------------------------------------

/// **Every field of every context the model is sent, listed field by field.**
///
/// This is the audit, committed rather than described. A test walks the JSON each `wire_*`
/// function produces and asserts its leaf paths are exactly these - so a field added to a context
/// fails the suite until somebody has written it down here and looked at it. `[]` marks an array
/// of objects.
///
/// What is *not* here is the point: no future spawn, no rider that has not registered, no trip
/// nobody has declared, no metric value, no objective function, and no wall clock. The clock is
/// left out for a second reason as well - a context carrying it is a different question every
/// tick, and a question that cannot be memoised is a network call per agent per tick.
pub const LEAKAGE_AUDIT: [(&str, &[&str]); 4] = [
    (
        "line",
        &[
            "max_detour_pct",
            "options[].best_walk_share",
            "options[].detour_pct",
            "options[].line",
            "options[].riders_waiting",
        ],
    ),
    (
        "guarantee",
        &[
            "cooldown_s",
            "depot_to_origin_km",
            "drivers_inbound",
            "line",
            "longest_wait_s",
            "patience_spent",
            "riders_triggered",
            "trigger_after_wait_s",
            "vehicles_sent",
        ],
    ),
    (
        "assign",
        &[
            "riders[].destination_latitude",
            "riders[].destination_longitude",
            "riders[].latitude",
            "riders[].longitude",
            "riders[].neighbours",
            "riders[].patience_spent",
            "riders[].seats",
            "taxis[].free_seats",
            "taxis[].latitude",
            "taxis[].longitude",
        ],
    ),
    (
        "reposition",
        &[
            "stations[].latitude",
            "stations[].longitude",
            "stations[].name",
            "stations[].pickups_so_far",
            "taxis[].free_seats",
            "taxis[].latitude",
            "taxis[].longitude",
        ],
    ),
];

/// The serialised line context. `line` is the option's position in the list, not a world id: the
/// action space is "one of these options", so that is all the model is given to name.
pub fn wire_line(context: &DecisionContext) -> Json {
    json!({
        "max_detour_pct": context.max_detour_pct,
        "options": context.options.iter().enumerate().map(|(index, option)| json!({
            "line": index,
            "detour_pct": round(option.detour_pct),
            "riders_waiting": option.riders_waiting,
            "best_walk_share": option.best_walk_share.map(round),
        })).collect::<Vec<Json>>(),
    })
}

pub fn wire_guarantee(context: &GuaranteeContext) -> Json {
    json!({
        "line": context.line,
        "riders_triggered": context.riders_triggered,
        "longest_wait_s": round(context.longest_wait_s),
        "patience_spent": round(context.patience_spent),
        "drivers_inbound": context.drivers_inbound,
        "trigger_after_wait_s": round(context.trigger_after_wait_s),
        "cooldown_s": round(context.cooldown_s),
        "depot_to_origin_km": round(context.depot_to_origin_km),
        "vehicles_sent": context.vehicles_sent,
    })
}

pub fn wire_assign(context: &AssignContext) -> Json {
    json!({
        "taxis": context.taxis.iter().map(|taxi| json!({
            "latitude": round(taxi.position.latitude),
            "longitude": round(taxi.position.longitude),
            "free_seats": taxi.free_seats,
        })).collect::<Vec<Json>>(),
        "riders": context.riders.iter().map(|rider| json!({
            "latitude": round(rider.position.latitude),
            "longitude": round(rider.position.longitude),
            "destination_latitude": round(rider.destination.latitude),
            "destination_longitude": round(rider.destination.longitude),
            "seats": rider.seats,
            "patience_spent": round(rider.patience_spent),
            "neighbours": rider.neighbours,
        })).collect::<Vec<Json>>(),
    })
}

pub fn wire_reposition(context: &RepositionContext) -> Json {
    json!({
        "taxis": context.taxis.iter().map(|taxi| json!({
            "latitude": round(taxi.position.latitude),
            "longitude": round(taxi.position.longitude),
            "free_seats": taxi.free_seats,
        })).collect::<Vec<Json>>(),
        "stations": context.stations.iter().enumerate().map(|(index, station)| json!({
            "name": station.name,
            "latitude": round(station.coord.latitude),
            "longitude": round(station.coord.longitude),
            "pickups_so_far": context.pickups_by_station.get(index).copied().unwrap_or(0),
        })).collect::<Vec<Json>>(),
    })
}

/// Four decimals on every float in a context, which is about a metre of latitude.
///
/// Not cosmetic: the memo is keyed on the exact bytes of the request, so a position that differs
/// in the fifteenth decimal would be a fresh question and a fresh call. Rounding is what makes
/// "the same situation" mean the same situation.
fn round(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

// ---------------------------------------------------------------------------------------------
// the model
// ---------------------------------------------------------------------------------------------

/// The line the model may take, or the trip the driver was making anyway.
#[derive(Debug, Deserialize)]
struct LineAnswer {
    action: String,
    option: usize,
}

#[derive(Debug, Deserialize)]
struct GuaranteeAnswer {
    dispatch: bool,
}

#[derive(Debug, Deserialize)]
struct PairsAnswer {
    pairs: Vec<Pair>,
}

#[derive(Debug, Deserialize)]
struct Pair {
    taxi: usize,
    rider: usize,
}

#[derive(Debug, Deserialize)]
struct MovesAnswer {
    moves: Vec<MoveAnswer>,
}

#[derive(Debug, Deserialize)]
struct MoveAnswer {
    taxi: usize,
    station: usize,
}

/// A model in the loop, on the same seam the two fixed rules plug into.
///
/// `exceptions_only` is the whole difference between the two model policies:
///
/// - `llm` asks about every line choice there is.
/// - `hybrid` asks only where **the two fixed rules disagree** - where `heuristic` would take the
///   quickest line and `greedy` a different one that has somebody standing on it. That is the
///   exception a fixed threshold handles badly by construction, and it is not invented: the two
///   rules cross over on the real corridor, greedy ahead below thirteen vehicles and behind above
///   thirty-two, which is the signature of two rules each missing what the other sees. Everywhere
///   they agree there is nothing for a model to add and it is not asked.
///
/// Both ask about the other three decisions every time those fire, because there is no second
/// fixed rule to disagree with: the guarantee's answer was always "yes", the assignment's was
/// always greedy, and repositioning had no rule at all.
///
/// **An invalid action is counted and refused, never retried.** Refused means the decision falls
/// through to the rule that would have made it - the heuristic's line, the guarantee's yes, the
/// greedy pairing, no movement - and `invalid_action_rate` is reported beside the mobility
/// columns so a reader can see how much of a run was really the fallback. Retrying would hide
/// exactly the number worth having.
#[derive(Debug)]
pub struct Model {
    sidecar: Sidecar,
    exceptions_only: bool,
    /// Whether a refusal has already been reported. One line per run rather than one per refusal:
    /// a refused answer is memoised, so a single bad answer would otherwise print thousands of
    /// times, and the number that matters is `invalid_action_rate` rather than the log.
    warned: bool,
}

impl Model {
    pub fn new(sidecar: &str, exceptions_only: bool) -> Model {
        Model {
            sidecar: Sidecar::new(sidecar),
            exceptions_only,
            warned: false,
        }
    }

    /// Ask, parse, and count a shape the world cannot do without retrying it.
    fn answer<T: for<'de> Deserialize<'de>>(
        &mut self,
        decision: &str,
        context: &Json,
        schema: &Json,
    ) -> Result<Option<T>> {
        let raw = self.sidecar.ask(decision, context, schema)?;
        match serde_json::from_value::<T>(raw.clone()) {
            Ok(answer) => Ok(Some(answer)),
            Err(_) => {
                self.refuse(decision, &format!("not an action at all: {raw}"), ());
                Ok(None)
            }
        }
    }

    /// Count it, say so once, and hand back what the fixed rule would have decided.
    fn refuse<T>(&mut self, decision: &str, why: &str, fallback: T) -> T {
        self.sidecar.count_invalid();
        if !self.warned {
            self.warned = true;
            eprintln!(
                "warning: refused a {decision} action ({why}); the fixed rule decided instead. \
                 Further refusals are counted in invalid_action_rate rather than printed."
            );
        }
        fallback
    }
}

impl Policy for Model {
    fn decide(&mut self, context: &DecisionContext) -> Result<Action> {
        let fallback = heuristic_line(context);
        if self.exceptions_only && greedy_line(context) == fallback {
            return Ok(fallback);
        }
        let schema = json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["take_line", "drive_own_trip"]},
                "option": {"type": "integer", "minimum": 0},
            },
            "required": ["action", "option"],
            "additionalProperties": false,
        });
        let Some(answer): Option<LineAnswer> = self
            .answer("line", &wire_line(context), &schema)
            .context("the line decision")?
        else {
            return Ok(fallback);
        };
        if answer.action == "drive_own_trip" {
            return Ok(Action::DriveOwnTrip);
        }
        let Some(option) = context.options.get(answer.option) else {
            return Ok(self.refuse("line", "no such line option", fallback));
        };
        // The driver's own refusal, which no policy may overrule.
        if !context.accepts(option) {
            return Ok(self.refuse("line", "a detour the driver would not accept", fallback));
        }
        Ok(Action::TakeLine(option.line))
    }

    fn dispatch_guarantee(&mut self, context: &GuaranteeContext) -> Result<bool> {
        let schema = json!({
            "type": "object",
            "properties": {"dispatch": {"type": "boolean"}},
            "required": ["dispatch"],
            "additionalProperties": false,
        });
        let answer: Option<GuaranteeAnswer> = self
            .answer("guarantee", &wire_guarantee(context), &schema)
            .context("the guarantee decision")?;
        Ok(answer.is_none_or(|answer| answer.dispatch))
    }

    fn assign_fleet(&mut self, context: &AssignContext) -> Result<Vec<Pairing>> {
        let schema = json!({
            "type": "object",
            "properties": {
                "pairs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "taxi": {"type": "integer", "minimum": 0},
                            "rider": {"type": "integer", "minimum": 0},
                        },
                        "required": ["taxi", "rider"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["pairs"],
            "additionalProperties": false,
        });
        let fallback = || greedy_pairs(context);
        let Some(answer): Option<PairsAnswer> = self
            .answer("assign", &wire_assign(context), &schema)
            .context("the fleet assignment")?
        else {
            return Ok(fallback());
        };

        let mut pairs: Vec<Pairing> = Vec::new();
        for pair in &answer.pairs {
            let (Some(taxi), Some(rider)) =
                (context.taxis.get(pair.taxi), context.riders.get(pair.rider))
            else {
                return Ok(self.refuse("assign", "no such taxi or rider", fallback()));
            };
            if taxi.free_seats < rider.seats {
                return Ok(self.refuse("assign", "a party larger than the vehicle", fallback()));
            }
            if pairs
                .iter()
                .any(|made| made.taxi == pair.taxi || made.rider == pair.rider)
            {
                return Ok(self.refuse("assign", "a taxi or a rider used twice", fallback()));
            }
            pairs.push(Pairing::new(pair.taxi, pair.rider));
        }
        Ok(pairs)
    }

    fn repositions(&self) -> bool {
        true
    }

    fn reposition(&mut self, context: &RepositionContext) -> Result<Vec<Move>> {
        let schema = json!({
            "type": "object",
            "properties": {
                "moves": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "taxi": {"type": "integer", "minimum": 0},
                            "station": {"type": "integer", "minimum": 0},
                        },
                        "required": ["taxi", "station"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["moves"],
            "additionalProperties": false,
        });
        let Some(answer): Option<MovesAnswer> = self
            .answer("reposition", &wire_reposition(context), &schema)
            .context("the repositioning decision")?
        else {
            return Ok(Vec::new());
        };

        let mut moves: Vec<Move> = Vec::new();
        for proposed in &answer.moves {
            if proposed.taxi >= context.taxis.len() || proposed.station >= context.stations.len() {
                return Ok(self.refuse("reposition", "no such taxi or station", Vec::new()));
            }
            if moves.iter().any(|made| made.taxi == proposed.taxi) {
                return Ok(self.refuse("reposition", "a taxi sent two places", Vec::new()));
            }
            moves.push(Move {
                taxi: proposed.taxi,
                station: proposed.station,
            });
        }
        Ok(moves)
    }

    fn cost(&self) -> DecisionCost {
        self.sidecar.cost()
    }
}
