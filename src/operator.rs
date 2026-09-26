//! The operator: the matching authority for the lines it runs.
//!
//! It is a registry, not an agent - it has no state machine and no position. Riders and drivers
//! register on a line when they commit to a trip, and the operator answers "who is on my line and
//! going my way". Everything it returns is a suggestion; the pairing itself is resolved in the
//! simulation's apply pass, so two drivers reaching for the same rider on the same tick cannot
//! both get one.
//!
//! ponytail: one registry across every network rather than one per operator. Matching only ever
//! happens between two agents registered on the same line, so per-operator registries would
//! partition the same set the line key already partitions. Split it if a scenario ever needs two
//! operators to compete for the same line.

use crate::agents::{may_carry, Agent, AgentId, State};
use crate::policy::LineOption;
use crate::routing::{Profile, Router};
use crate::scenario::AgentKind;
use crate::world::{angle_between_degrees, bearing_degrees, haversine_km, Coord, LineId, World};

/// How far off a party's own heading a candidate may lie and still count as on the way.
const MAX_DETOUR_ANGLE_DEGREES: f64 = 45.0;

/// Two points within a metre of each other are the same place, and the bearing between them is
/// meaningless. This is the normal case at a station, where driver and rider stand together.
pub const COINCIDENT_KM: f64 = 0.001;

/// A line an agent could travel on, and what it costs in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMatch {
    pub line: LineId,
    /// Approach to the origin station, plus the line itself, plus the leg from the destination
    /// station onwards. The quantity [`Operator::ranked_lines`] sorts on.
    pub cost_s: f64,
    /// Of that, the part covered on foot. What a rider weighs against its `max_walk_s`.
    pub walk_s: f64,
}

#[derive(Debug, Clone, Copy)]
struct Registration {
    line: LineId,
    agent: AgentId,
}

/// Who has told the operator they are travelling, and on which line.
///
/// Registrations are kept in the order they arrived, so matching is first-come-first-served and a
/// run does not depend on hash iteration order.
#[derive(Debug, Default)]
pub struct Operator {
    riders: Vec<Registration>,
    drivers: Vec<Registration>,
}

impl Operator {
    pub fn new() -> Operator {
        Operator::default()
    }

    /// Every line that could serve a trip from `origin` to `destination`, quickest first: the
    /// walk to a line's origin station, the line itself at the duration the routing server gave
    /// it, and the leg from its destination station onwards.
    ///
    /// The operator ranks; it does not refuse. A rider walks down the list until it finds one it
    /// is willing to walk to, and a driver takes the first. Both flanking legs are priced on
    /// foot for both parties, so the two sides rank the lines identically and meet on the one the
    /// operator would have offered either of them.
    ///
    /// Empty only when the world has no lines, which validation already rules out.
    pub fn ranked_lines(
        &self,
        world: &World,
        router: &Router,
        origin: Coord,
        destination: Coord,
    ) -> Vec<LineMatch> {
        let mut matches: Vec<LineMatch> = world
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let walk_s = router
                    .route(origin, world.station(line.origin).coord, Profile::Foot)
                    .duration_s
                    + router
                        .route(
                            world.station(line.destination).coord,
                            destination,
                            Profile::Foot,
                        )
                        .duration_s;
                LineMatch {
                    line: LineId(index as u32),
                    cost_s: walk_s + line.route.duration_s,
                    walk_s,
                }
            })
            .collect();
        // A stable sort on a total order: two lines costing the same stay in declaration order,
        // so the answer does not depend on how the sort happened to partition them.
        matches.sort_by(|left, right| left.cost_s.total_cmp(&right.cost_s));
        matches
    }

    /// Every line this driver could take, in the ranking's own order: what serving each would
    /// cost it in detour on its own trip, and how many riders are waiting on each right now.
    ///
    /// The one query a driver's line choice needs, and the reason a driver has no business
    /// computing a rider's walk. It hands a [`crate::policy::Policy`] the two quantities the
    /// choice actually turns on, and each is measured in the unit the party paying it feels: the
    /// detour in **kilometres of driving**, the demand in people standing at a station.
    ///
    /// The detour is deliberately *not* [`LineMatch::cost_s`]. That number prices both flanking
    /// legs on foot for both parties on purpose, so it is not a travel time, and reading it as one
    /// makes a driver refuse a line because of a walk it is never going to take. A kilometre is a
    /// kilometre whoever covers it, which is what keeps the driver's refusal out of the shared
    /// yardstick the ranking depends on.
    ///
    /// ponytail: the uncached legs come back as straight lines, so their distance is understated
    /// while the line itself is real road geometry - the ratio therefore overstates the detour and
    /// a driver refuses a little more readily than it should until the approach legs are cached
    /// too. Measured on `minimal.json`: about 150% with the committed cache and about 122% without
    /// it, so a fixed 110% would refuse the toy corridor outright - which is why the figure is a
    /// knob with no default rather than a constant.
    ///
    /// ponytail: rebuilt from scratch every tick a driver spends at home, which is a handful of
    /// route lookups and one registry scan per line. Fleets and lines are both in the tens; cache
    /// the geometry on the agent if a sweep ever makes this show up in a profile.
    pub fn line_options(
        &self,
        world: &World,
        router: &Router,
        driver: &Agent,
        agents: &[Agent],
        pairs: bool,
    ) -> Vec<LineOption> {
        let direct_km = router
            .route(driver.origin, driver.destination, Profile::Road)
            .distance_km;
        self.ranked_lines(world, router, driver.origin, driver.destination)
            .into_iter()
            .map(|matched| {
                let line = world.line(matched.line);
                let detoured_km = router
                    .route(
                        driver.origin,
                        world.station(line.origin).coord,
                        Profile::Road,
                    )
                    .distance_km
                    + line.route.distance_km
                    + router
                        .route(
                            world.station(line.destination).coord,
                            driver.destination,
                            Profile::Road,
                        )
                        .distance_km;
                let divertible = match pairs {
                    true => self.divertible_to(world, router, matched.line, agents),
                    false => Vec::new(),
                };
                LineOption {
                    line: matched.line,
                    best_walk_share: divertible.first().map(|(_id, share)| *share),
                    // A driver going nowhere has no detour to measure, so nothing refuses it.
                    detour_pct: match direct_km > 0.0 {
                        true => detoured_km * 100.0 / direct_km,
                        false => 0.0,
                    },
                    riders_waiting: self.demand_on(matched.line, driver, agents) + divertible.len(),
                }
            })
            .collect()
    }

    /// How many riders this driver could pick up on `line`: those already standing at its origin
    /// station, and those that declared ahead and are still in its pool, which this driver can
    /// only take if it declares ahead too and their departure windows overlap.
    ///
    /// The demand half of [`Operator::line_options`]. Both halves are needed or the demand a
    /// declaring cohort represents is invisible: such a rider never stands at a station until a
    /// driver has claimed it, so counting only the station would make advance declaration look
    /// like no demand at all and a demand-following policy would drive away from it.
    ///
    /// Deliberately with **no direction filter**, unlike [`Operator::riders_for`], and for the
    /// same reason [`Operator::declared_riders_for`] has none. That filter asks whether a rider
    /// lies ahead of where the driver is *now*: the right question for a driver already on the
    /// road, and the wrong one for a driver at home choosing where to drive, where it would hide a
    /// full station that happens to lie off to one side of a trip the driver has not started.
    /// Being registered on the line is already the statement that the two are going the same way,
    /// and the driver's own side of the bargain is `max_detour_pct`, weighed on the same option in
    /// the same breath.
    ///
    /// A rider that declared on several lines counts on each of them, so this is demand on a line
    /// and not a headcount of distinct people.
    fn demand_on(&self, line: LineId, driver: &Agent, agents: &[Agent]) -> usize {
        self.riders
            .iter()
            .filter(|entry| entry.line == line)
            .filter(|entry| may_carry(driver, &agents[entry.agent.0 as usize]))
            .filter(|entry| {
                let rider = &agents[entry.agent.0 as usize];
                rider.state == State::WaitingDriver
                    || (driver.declares_ahead()
                        && rider.awaits_advance_match()
                        && driver.window.overlaps(rider.window))
            })
            .count()
    }

    /// Every registered rider a pair-scoring policy could send to `line` instead of wherever it
    /// is currently walking, cheapest walk first.
    ///
    /// **This is the query that drops the line as the match key.** Everything else the operator
    /// answers is partitioned by the line both parties registered on, which is exactly what makes
    /// a rider invisible to a driver on the line it ranked second. Here the registration says only
    /// that the operator knows about the trip; whether the two can travel together is decided by
    /// the rider's own `max_walk_s`, measured against the walk it would still have to make from
    /// where it has got to - both flanking legs, the same sum it accepted its own line on.
    ///
    /// The arena is scanned rather than the registry, because the line a rider happens to be
    /// registered on is the one thing this query is not interested in. `is_registered` is still
    /// the gate: a ghost told nobody it was travelling, so nothing the operator answers may offer
    /// it.
    ///
    /// Sorted by the walk, ties broken by arena order, so the pairing never depends on how the
    /// sort partitioned equals.
    pub fn divertible_to(
        &self,
        world: &World,
        router: &Router,
        line: LineId,
        agents: &[Agent],
    ) -> Vec<(AgentId, f64)> {
        let origin = world.station(world.line(line).origin).coord;
        let destination = world.station(world.line(line).destination).coord;
        let mut found: Vec<(AgentId, f64)> = agents
            .iter()
            .filter(|rider| rider.kind.is_rider() && rider.kind.is_registered())
            .filter(|rider| rider.kind != AgentKind::PolynomialRider)
            .filter(|rider| rider.awaits_pair_match())
            .filter_map(|rider| {
                let walk_s = router
                    .route(rider.position(), origin, Profile::Foot)
                    .duration_s
                    + router
                        .route(destination, rider.destination, Profile::Foot)
                        .duration_s;
                // As a **share of this rider's own threshold**, not in seconds. That is what
                // makes the number comparable across riders that disagree about how far they will
                // walk, and it is the rider's half of a pair score: the driver's half is its
                // detour as a share of driving straight there, and neither party has to read the
                // other's unit.
                (walk_s <= rider.max_walk_s).then_some((rider.id, walk_s / rider.max_walk_s))
            })
            .collect();
        found.sort_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)));
        found
    }

    pub fn register_rider(&mut self, agent: AgentId, line: LineId) {
        self.riders.push(Registration { line, agent });
    }

    pub fn register_driver(&mut self, agent: AgentId, line: LineId) {
        self.drivers.push(Registration { line, agent });
    }

    /// Forget an agent, whichever side it registered on. One method rather than one per side:
    /// an agent that has finished or given up leaves the registry the same way either way.
    pub fn unregister(&mut self, agent: AgentId) {
        self.riders.retain(|entry| entry.agent != agent);
        self.drivers.retain(|entry| entry.agent != agent);
    }

    pub fn registered_riders(&self) -> usize {
        self.riders.len()
    }

    pub fn registered_drivers(&self) -> usize {
        self.drivers.len()
    }

    /// Riders on `driver`'s line who are still waiting and lie on its way, oldest registration
    /// first.
    pub fn riders_for(&self, driver: &Agent, agents: &[Agent]) -> Vec<AgentId> {
        candidates(&self.riders, driver, agents)
    }

    /// Riders on `driver`'s line that declared their trip ahead, have not set off yet, nobody has
    /// taken, and whose departure window overlaps the driver's own.
    ///
    /// No direction filter, unlike [`Operator::riders_for`]: such a rider is still at home, and
    /// where it is standing says nothing about the trip. Being registered on the line is already
    /// the statement that the two are going the same way, and the rider's own `max_walk_s` is
    /// what decided the line's origin station was close enough to walk to.
    pub fn declared_riders_for(&self, driver: &Agent, agents: &[Agent]) -> Vec<AgentId> {
        let Some(line) = driver.line else {
            return Vec::new();
        };
        self.riders
            .iter()
            .filter(|entry| entry.line == line)
            .map(|entry| entry.agent)
            .filter(|id| {
                let rider = &agents[id.0 as usize];
                rider.awaits_advance_match()
                    && may_carry(driver, rider)
                    && driver.window.overlaps(rider.window)
            })
            .collect()
    }
}

fn candidates(registry: &[Registration], party: &Agent, agents: &[Agent]) -> Vec<AgentId> {
    let Some(line) = party.line else {
        return Vec::new();
    };
    registry
        .iter()
        .filter(|entry| entry.line == line && entry.agent != party.id)
        .map(|entry| entry.agent)
        .filter(|id| {
            let candidate = &agents[id.0 as usize];
            candidate.state == State::WaitingDriver
                && may_carry(party, candidate)
                && on_the_way(party, candidate.position())
        })
        .collect()
}

/// Whether `candidate` lies ahead of `party`: within [`MAX_DETOUR_ANGLE_DEGREES`] of the way it
/// is already heading, and nearer to it than its own destination.
///
/// At a station the two are standing on the same spot, so this is trivially true - the line
/// registration is what has already established they are going the same way. The angle matters
/// for a driver considering someone it has not reached yet.
pub fn on_the_way(party: &Agent, candidate: Coord) -> bool {
    let here = party.position();
    let to_candidate_km = haversine_km(here, candidate);
    if to_candidate_km <= COINCIDENT_KM {
        return true;
    }
    let to_destination_km = haversine_km(here, party.destination);
    to_candidate_km < to_destination_km
        && angle_between_degrees(
            bearing_degrees(here, party.destination),
            bearing_degrees(here, candidate),
        ) <= MAX_DETOUR_ANGLE_DEGREES
}
