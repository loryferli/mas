//! The tick loop: advance the clock, spawn the cohorts whose time has come, then
//! perceive → decide → apply.
//!
//! Every random draw comes from one seeded `ChaCha8Rng` owned by [`Sim`] - never a thread-local
//! generator, and never `StdRng`, whose output is not guaranteed stable across releases.
//! Reproducibility is the point.

use anyhow::Result;
use rand::RngExt;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::agents::taxi;
use crate::agents::{
    self, drop_point, Agent, AgentId, Context, DepartureWindow, Influence, State, TransitionReason,
    Vehicle,
};
use crate::events::EventLog;
use crate::llm::DecisionCost;
use crate::operator::Operator;
use crate::policy::{self, GuaranteeContext, IdleTaxi, Policy, RepositionContext, WaitingPoint};
use crate::routing::{Profile, Router};
use crate::scenario::{
    AgentKind, Cohort, DepartureGuarantee, Registers, Scenario, Service, SpawnArea, SpawnWindow,
    StationApproach,
};
use crate::world::{haversine_km, Coord, LineId, StationId, World, KM_PER_DEGREE_LATITUDE};

/// How a run ended, and how many agents it accounted for. The numbers a reader needs to know
/// whether the metrics are worth looking at; [`crate::metrics::Metrics`] has the rest.
#[derive(Debug, Default, PartialEq)]
pub struct RunSummary {
    pub ticks: u64,
    pub agents_spawned: u32,
    pub canceled: u32,
    /// Agents that reached the end of their journey by themselves.
    pub finished: u32,
    /// Agents still going when the clock cap hit. A nonzero count means the run was cut off, so
    /// its numbers are not a finding.
    pub stranded: u32,
    /// Set when the clock cap ended the run instead of the agents running out.
    pub stopped_by_clock: bool,
    pub events: u64,
    /// What the model in the loop cost, if there was one. All zeroes under a fixed rule, which is
    /// how "`heuristic` makes zero calls" is asserted rather than assumed.
    pub decisions: DecisionCost,
}

pub struct Sim {
    world: World,
    router: Router,
    operator: Operator,
    /// The operator's four decisions: which line a driver takes, whether to spend a guarantee
    /// vehicle, which taxi collects which rider, and where an idle taxi waits. Built from the
    /// scenario, held for the whole run, and allowed to be stateful - a model policy holds a pipe
    /// to a sidecar and a memo of what it has already asked.
    policy: Box<dyn Policy>,
    agents: Vec<Agent>,
    /// Agents not yet in the arena, sorted by spawn time so the front is always next.
    pending: Vec<Agent>,
    /// When each line last had a guarantee vehicle dispatched, indexed by `LineId`. `None` until
    /// the first one.
    guarantee_last_s: Vec<Option<f64>>,
    /// How many ticks the fleet operator actually paired taxis to riders on. Nonzero only on the
    /// ticks where demand or idleness changed, which is what keeps assignment event-driven - and
    /// what a test asserts against, because a per-tick global matrix would put a model call on
    /// every tick.
    fleet_assignments: u64,
    /// Claims that arrived at a rider somebody else had already taken this tick, or whose party no
    /// longer fit the seats left. **The measurement behind the decision not to assign pair scoring
    /// across the fleet**: a policy is greedy per driver, so two drivers can reach for the same
    /// rider, and this counts every time it happened. The loser re-decides on the next tick, so
    /// each one costs a second of a driver's waiting and nothing else.
    claim_collisions: u64,
    /// Riders collected so far nearest each station, which is the only history the repositioning
    /// decision is given: an operator plainly knows where it has picked people up.
    pickups_by_station: Vec<u32>,
    time_s: f64,
    tick_s: f64,
    max_time_s: f64,
}

impl Sim {
    /// Draw every agent's spawn time and jittered endpoints up front, so the arena is a pure
    /// function of the scenario and the seed and does not depend on tick ordering.
    ///
    /// `router` is the third input to a run: it supplies every leg's geometry, from the cache
    /// where there is one and from the straight line where there is not.
    pub fn new(scenario: &Scenario, seed: u64, mut router: Router) -> Sim {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);

        // Drawn before any agent, and once for the whole run.
        //
        // ponytail: one draw per run rather than one per agent. Both parties price a line's
        // flanking legs at these speeds, so a per-agent walking pace makes them rank the lines
        // differently and they stop meeting at the same station. That is the shared yardstick,
        // straightened in 7b and 11b, and per-agent speeds belong after it.
        router.set_speeds(
            scenario.walk_speed_mps.draw_speed(&mut rng),
            scenario.drive_speed_mps.draw_speed(&mut rng),
        );

        let mut pending: Vec<Agent> = Vec::with_capacity(scenario.total_agents() as usize);
        let mut groups = 0u32;

        for cohort in &scenario.cohorts {
            for _ in 0..cohort.count {
                let spawn_s = match &cohort.spawn_window {
                    SpawnWindow::Range(range) => sample_range(&mut rng, range.start_s, range.end_s),
                    SpawnWindow::Drawn(value) => {
                        value.draw(&mut rng).clamp(0.0, scenario.max_time_s)
                    }
                };
                let origin = sample_area(&mut rng, &cohort.origin);
                // A fleet taxi has nowhere to be, so it draws nothing here and its destination is
                // where it starts. Every other cohort draws exactly as it always did.
                let destination = match &cohort.destination {
                    Some(area) => sample_area(&mut rng, area),
                    None => origin,
                };
                let mut agent = new_agent(cohort, spawn_s, origin, destination, &mut rng);
                // A pre-formed group: its size drawn after everything else the driver drew, and its
                // passengers spawned beside it, sharing its trip.
                if let Some(size) = &cohort.group_size {
                    let free_seats = agent.vehicle.as_ref().map_or(0, |vehicle| {
                        vehicle.capacity.saturating_sub(vehicle.seats_used)
                    });
                    let passengers = size.draw_count(&mut rng).min(free_seats);
                    agent.group = Some(groups);
                    for _ in 0..passengers {
                        pending.push(group_passenger(&agent, groups));
                    }
                    groups += 1;
                }
                pending.push(agent);
            }
        }

        // Earliest first, and the queue is popped from the back.
        pending.sort_by(|left, right| right.spawn_s.total_cmp(&left.spawn_s));

        let world = World::from_environment(&scenario.environment, &router);
        // A network fleet rider's route over the lines is fixed by where it starts and where it is
        // going, so it is worked out once, here, and takes no random draw.
        for agent in pending
            .iter_mut()
            .filter(|agent| agent.service == Service::Network)
        {
            agent.legs = taxi::network_legs(&world, agent.origin, agent.destination);
        }
        Sim {
            guarantee_last_s: vec![None; world.lines().len()],
            pickups_by_station: vec![0; world.stations().len()],
            world,
            router,
            operator: Operator::new(),
            policy: policy::build(scenario.policy, scenario.sidecar()),
            agents: Vec::with_capacity(pending.len()),
            pending,
            fleet_assignments: 0,
            claim_collisions: 0,
            time_s: 0.0,
            tick_s: scenario.tick_s,
            max_time_s: scenario.max_time_s,
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn operator(&self) -> &Operator {
        &self.operator
    }

    pub fn time_s(&self) -> f64 {
        self.time_s
    }

    pub fn fleet_assignments(&self) -> u64 {
        self.fleet_assignments
    }

    /// How many claims lost a race to another driver, which is the cost of a policy that scores
    /// pairs one driver at a time.
    pub fn claim_collisions(&self) -> u64 {
        self.claim_collisions
    }

    /// Run until every agent has finished, or the clock passes `max_time_s`.
    pub fn run(&mut self, log: &mut EventLog) -> Result<RunSummary> {
        let mut ticks = 0u64;
        let mut spawned = 0u32;
        let stopped_by_clock = loop {
            spawned += self.spawn_due(log)?;
            spawned += self.dispatch_guarantees(log)?;
            self.assign_fleet(log)?;
            self.reposition_fleet(log)?;
            self.stand_down_fleet(log)?;

            if self.pending.is_empty() && !self.agents.iter().any(|a| a.state.is_active()) {
                break false;
            }
            if self.time_s > self.max_time_s {
                break true;
            }

            self.step(log)?;

            let previous_s = self.time_s;
            self.time_s += self.tick_s;
            ticks += 1;
            debug_assert!(
                self.time_s > previous_s,
                "the clock must strictly increase, {previous_s} -> {}",
                self.time_s
            );
        };

        let stranded = if stopped_by_clock {
            self.strand_remaining(log)?
        } else {
            0
        };
        log.finish()?;

        Ok(RunSummary {
            ticks,
            agents_spawned: spawned,
            canceled: self.count(State::Canceled),
            finished: self.count(State::EndJourney) - stranded,
            stranded,
            stopped_by_clock,
            events: log.rows(),
            decisions: self.policy.cost(),
        })
    }

    fn count(&self, state: State) -> u32 {
        self.agents.iter().filter(|a| a.state == state).count() as u32
    }

    /// Move every agent whose spawn time has arrived into the arena.
    ///
    /// The agent's id is its slot in the arena, assigned here. Nothing is ever removed from
    /// `agents`, so a slot stays valid for the rest of the run and one agent can refer to
    /// another by id without a lookup.
    fn spawn_due(&mut self, log: &mut EventLog) -> Result<u32> {
        let mut spawned = 0;
        while self
            .pending
            .last()
            .is_some_and(|agent| agent.spawn_s <= self.time_s)
        {
            let mut agent = self.pending.pop().expect("just checked");
            agent.id = AgentId(self.agents.len() as u32);
            agent.state_since_s = self.time_s;
            log.record(
                self.time_s,
                agent.id,
                agent.kind,
                agent.state,
                agent.reason,
                agent.position(),
                agent.vehicle.as_ref().map(|vehicle| vehicle.seats_used),
                agent.distance_km,
                &agent.label,
            )?;
            self.agents.push(agent);
            spawned += 1;
        }
        Ok(spawned)
    }

    /// The departure guarantee: a line whose rider has waited past its trigger gets a vehicle.
    ///
    /// A guarantee vehicle is not a cohort - the operator calls it mid-run - so it enters the
    /// arena here rather than through the spawn queue, and takes no random draw at all. A scenario
    /// with no guarantee on any line therefore produces exactly the bytes it did before this
    /// existed.
    fn dispatch_guarantees(&mut self, log: &mut EventLog) -> Result<u32> {
        let mut dispatched = 0;
        for index in 0..self.world.lines().len() {
            let line = LineId(index as u32);
            let Some(guarantee) = self.world.line(line).departure_guarantee else {
                continue;
            };
            // One vehicle at a time per line, and then a cooldown before the next. The first of
            // those is what makes a zero cooldown mean "once the last one has gone" rather than
            // "one every tick": a rider waiting is a standing trigger until somebody reaches it.
            let already_coming = self.agents.iter().any(|agent| {
                agent.kind == AgentKind::TaxiDriver
                    && agent.line == Some(line)
                    && agent.state.is_active()
            });
            let cooling_down = self.guarantee_last_s[index]
                .is_some_and(|last_s| self.time_s - last_s < guarantee.cooldown_s);
            if already_coming || cooling_down {
                continue;
            }
            let triggered: Vec<&Agent> = self
                .agents
                .iter()
                .filter(|agent| {
                    agent.kind.is_rider()
                        && agent.state == State::WaitingDriver
                        && agent.line == Some(line)
                        && self.time_s - agent.state_since_s >= guarantee.trigger_after_wait_s
                })
                .collect();
            if triggered.is_empty() {
                continue;
            }

            // **Whether** to spend the vehicle is the operator's decision, not the trigger's. A
            // fixed rule says yes - which is what every committed guarantee run was produced
            // under, so those are untouched - and a policy may say wait, because a vehicle costs
            // the service every kilometre it covers and a regular driver may be minutes away.
            let context = self.guarantee_context(line, &guarantee, &triggered);
            let dispatch = self.policy.dispatch_guarantee(&context)?;
            if !dispatch {
                // A decline starts the cooldown exactly as a dispatch does. Without that the
                // trigger is a standing question and the operator would be asked again every
                // tick, which for a model policy is a bill rather than a policy; with it, a line
                // is decided on at most once per cooldown either way.
                self.guarantee_last_s[index] = Some(self.time_s);
                let rider = triggered[0].id.0 as usize;
                self.record(
                    rider,
                    TransitionReason::GuaranteeDeclined,
                    "the operator held its vehicle back",
                    log,
                )?;
                continue;
            }

            let destination = self.world.station(self.world.line(line).destination).coord;
            let vehicle = guarantee_vehicle(
                AgentId(self.agents.len() as u32),
                line,
                &guarantee,
                destination,
                self.time_s,
            );
            let id = vehicle.id;
            self.agents.push(vehicle);
            self.operator.register_driver(id, line);
            self.guarantee_last_s[index] = Some(self.time_s);
            let detail = format!(
                "dispatched to {}",
                self.world.station(self.world.line(line).origin).name
            );
            self.log_state(id.0 as usize, &detail, log)?;
            dispatched += 1;
        }
        Ok(dispatched)
    }

    /// The fleet operator: pair idle taxis with waiting riders, and write the pairing down.
    ///
    /// Not an influence, and not a `Claim`: a fleet taxi has no line, and the operator is not an
    /// agent. It writes straight into the arena for the same reason
    /// [`Sim::dispatch_guarantees`] does - the operator *is* the authority here, so there is
    /// nothing for the apply pass to arbitrate.
    ///
    /// The assignment lands on `Agent::claimed_by`, the field a carpool driver's advance claim
    /// already uses: an assignment and a claim are the same statement, that this vehicle is coming
    /// for this rider and nobody else should.
    fn assign_fleet(&mut self, log: &mut EventLog) -> Result<()> {
        let Some(context) = taxi::assign_context(&self.agents, self.time_s) else {
            return Ok(());
        };
        let pairs = self.policy.assign_fleet(&context)?;
        if pairs.is_empty() {
            return Ok(());
        }
        self.fleet_assignments += 1;
        for pair in pairs {
            // The policy names a pair by its position in the two lists it was handed and has
            // already been validated against them, so this resolves rather than checks.
            let taxi = context.taxis[pair.taxi].id;
            let rider = context.riders[pair.rider].id;
            let name = self.agents[taxi.0 as usize].label.clone();
            self.agents[rider.0 as usize].claimed_by = match pair.collected {
                true => Some(taxi),
                false => Some(agents::NO_TAXI),
            };
            self.record(
                rider.0 as usize,
                TransitionReason::AssignedByOperator,
                &format!("assigned to {name}"),
                log,
            )?;
        }
        Ok(())
    }

    /// The one leak-audited view of a triggered guarantee, built where the trigger already scanned.
    fn guarantee_context(
        &self,
        line: LineId,
        guarantee: &DepartureGuarantee,
        triggered: &[&Agent],
    ) -> GuaranteeContext {
        let waits = triggered.iter().map(|rider| {
            (
                self.time_s - rider.state_since_s,
                taxi::patience_spent(rider, self.time_s),
            )
        });
        let (longest_wait_s, patience_spent) = waits
            .fold((0.0f64, 0.0f64), |worst, (wait, spent)| {
                (worst.0.max(wait), worst.1.max(spent))
            });
        let route = &self.world.line(line);
        GuaranteeContext {
            line: format!(
                "{} to {}",
                self.world.station(route.origin).name,
                self.world.station(route.destination).name
            ),
            riders_triggered: triggered.len(),
            longest_wait_s,
            patience_spent,
            // The demand's own answer, which a vehicle sent now may duplicate.
            drivers_inbound: self
                .agents
                .iter()
                .filter(|agent| agent.kind.is_driver() && agent.line == Some(line))
                .filter(|agent| {
                    matches!(
                        agent.state,
                        State::WaitingDeparture
                            | State::RideToDepartureStation
                            | State::RideToOwnDestinationRegistered
                    )
                })
                .count(),
            trigger_after_wait_s: guarantee.trigger_after_wait_s,
            cooldown_s: guarantee.cooldown_s,
            depot_to_origin_km: self
                .router
                .route(
                    Coord::new(guarantee.latitude, guarantee.longitude),
                    self.world.station(route.origin).coord,
                    Profile::Road,
                )
                .distance_km,
            vehicles_sent: self
                .agents
                .iter()
                .filter(|agent| agent.kind == AgentKind::TaxiDriver && agent.line == Some(line))
                .count() as u32,
        }
    }

    /// Where the idle fleet waits. **The one decision with no fixed rule to fall back on**: the
    /// engine's answer has always been "stand where you were released", so a policy here competes
    /// against doing nothing, and the write-up has to say so before a reader works it out.
    ///
    /// Skipped entirely unless the policy says it repositions, so a fleet run under a fixed rule
    /// costs exactly what it did before this existed. Asked only about taxis that are **parked** -
    /// one already on its way somewhere is not re-asked, which is what stops a vehicle being sent
    /// back and forth - and only while there is fleet demand still to come, because a fleet with
    /// none left is about to be stood down.
    fn reposition_fleet(&mut self, log: &mut EventLog) -> Result<()> {
        if !self.policy.repositions() {
            return Ok(());
        }
        let demand_to_come = self
            .pending
            .iter()
            .any(|agent| agent.kind == AgentKind::AutonomousTaxiRider);
        if !demand_to_come {
            return Ok(());
        }
        let parked: Vec<IdleTaxi> = self
            .agents
            .iter()
            .filter(|agent| taxi::is_idle(agent))
            .filter(|agent| {
                agent
                    .journey
                    .as_ref()
                    .is_none_or(|journey| journey.finished())
            })
            .map(|agent| IdleTaxi {
                id: agent.id,
                position: agent.position(),
                free_seats: agent
                    .vehicle
                    .as_ref()
                    .map_or(0, |vehicle| vehicle.free_seats()),
            })
            .collect();
        if parked.is_empty() {
            return Ok(());
        }

        let context = RepositionContext {
            taxis: parked,
            stations: self
                .world
                .stations()
                .iter()
                .enumerate()
                .map(|(index, station)| WaitingPoint {
                    station: StationId(index as u32),
                    name: station.name.clone(),
                    coord: station.coord,
                })
                .collect(),
            pickups_by_station: self.pickups_by_station.clone(),
        };
        for moved in self.policy.reposition(&context)? {
            let taxi = context.taxis[moved.taxi].id;
            let station = context.stations[moved.station].station;
            // Written straight into the arena, like an assignment: the operator is the authority
            // here, so there is nothing for the apply pass to arbitrate. The taxi's own machine
            // does the driving, and stays idle while it drives.
            self.agents[taxi.0 as usize].station = Some(station);
            let name = self.world.station(station).name.clone();
            self.record(
                taxi.0 as usize,
                TransitionReason::Repositioned,
                &format!("sent to wait at {name}"),
                log,
            )?;
        }
        Ok(())
    }

    /// Release every idle taxi once no fleet rider is left to serve, on the road or still to come.
    ///
    /// A taxi has no trip of its own, so nothing it does ends it: without this the fleet is
    /// active for ever and every fleet run hits the clock cap, which would make every fleet
    /// number a cut-off run rather than a result. The condition is exact rather than a timer, so a
    /// fleet run ends the tick the work does and `fleet_utilisation` is not diluted by the length
    /// of a knob. `Sim` is where it lives because only `Sim` can see the spawn queue.
    fn stand_down_fleet(&mut self, log: &mut EventLog) -> Result<()> {
        let demand_to_come = self
            .pending
            .iter()
            .any(|agent| agent.kind == AgentKind::AutonomousTaxiRider);
        let demand_now = self
            .agents
            .iter()
            .any(|agent| agent.kind == AgentKind::AutonomousTaxiRider && agent.state.is_active());
        if demand_to_come || demand_now {
            return Ok(());
        }
        for index in 0..self.agents.len() {
            if !taxi::is_idle(&self.agents[index]) {
                continue;
            }
            let time_s = self.time_s;
            enter(
                &mut self.agents[index],
                State::EndJourney,
                TransitionReason::StoodDown,
                time_s,
            );
            self.log_state(index, "no demand left", log)?;
        }
        Ok(())
    }

    /// One tick: decide for every agent against the same world, then apply the influences.
    fn step(&mut self, log: &mut EventLog) -> Result<()> {
        let influences: Vec<Influence> = {
            let pairs = self.policy.matches_pairs();
            let fleet_assignments = taxi::fleet_assignments(&self.agents);
            let context = Context {
                fleet_assignments: &fleet_assignments,
                time_s: self.time_s,
                operator: &self.operator,
                agents: &self.agents,
                router: &self.router,
                pairs,
            };
            // Disjoint fields: every agent reads the same arena while the policy - the one
            // thing a decision is allowed to mutate - is borrowed on its own.
            let policy = &mut *self.policy;
            self.agents
                .iter()
                .map(|agent| agents::decide(agent, &self.world, &context, policy))
                .collect::<Result<Vec<_>>>()?
        };

        // An influence was decided against the state its agent was in at the start of the tick. If
        // another agent's influence has moved it since - a rider picked up on the tick its patience
        // ran out, say - the decision is stale and is dropped: the other agent got there first.
        let decided_in: Vec<State> = self.agents.iter().map(|agent| agent.state).collect();
        for (index, influence) in influences.into_iter().enumerate() {
            if self.agents[index].state != decided_in[index] {
                continue;
            }
            self.apply(index, influence, log)?;
        }
        self.check_invariants();
        Ok(())
    }

    fn apply(&mut self, index: usize, influence: Influence, log: &mut EventLog) -> Result<()> {
        match influence {
            Influence::Idle => {}

            Influence::Advance => {
                let agent = &mut self.agents[index];
                let moved_km = match &mut agent.journey {
                    Some(journey) => journey.advance(self.tick_s),
                    None => 0.0,
                };
                agent.distance_km += moved_km;
                // Loaded kilometres, from both sides of the same ride: a driver carrying anyone,
                // and a rider being carried.
                let loaded = match &agent.vehicle {
                    Some(vehicle) => !vehicle.riders.is_empty(),
                    None => agent.state == State::RideCarpoolToDestination,
                };
                if loaded {
                    agent.loaded_km += moved_km;
                }
            }

            Influence::Register { line } => {
                let agent = &mut self.agents[index];
                // Switching lines rather than taking a first one: a policy that follows demand is
                // asked again every tick the driver spends at home. Out of the old pool before
                // into the new, so the operator never counts one seat on two lines, and the
                // declaration keeps the time the operator was first told about the trip.
                let switched = agent.line.replace(line);
                agent.declared_s = agent.declared_s.or(Some(self.time_s));
                let (id, kind) = (agent.id, agent.kind);
                // Unconditionally, rather than only when a line is being switched: an agent that
                // is in no pool is removed from nothing, and one that pooled on several lines
                // before naming one must leave all of them. The operator never counts one seat
                // twice either way.
                self.operator.unregister(id);
                // A ghost takes the line and tells nobody, so no query the operator answers can
                // offer it to anyone; only a driver standing at the same station finds it.
                if kind.is_registered() {
                    if kind.is_driver() {
                        self.operator.register_driver(id, line);
                    } else {
                        self.operator.register_rider(id, line);
                    }
                }
                let reason = if kind.is_registered() {
                    TransitionReason::RegisteredWithOperator
                } else {
                    TransitionReason::TookLineUnregistered
                };
                let detail = match switched {
                    Some(_) => format!(
                        "switched to {}",
                        self.world.station(self.world.line(line).origin).name
                    ),
                    None => String::new(),
                };
                self.record(index, reason, &detail, log)?;
            }

            Influence::Declare { lines } => {
                debug_assert!(
                    !lines.is_empty() && self.agents[index].kind.is_rider(),
                    "only a rider declares, and only on lines it would accept"
                );
                let agent = &mut self.agents[index];
                agent.declared_s = Some(self.time_s);
                // No line of its own: it is in every one of these pools and walks to whichever a
                // driver takes it from.
                let id = agent.id;
                for line in &lines {
                    self.operator.register_rider(id, *line);
                }
                let detail = match lines.len() {
                    1 => "declared on 1 line".to_string(),
                    count => format!("declared on {count} lines"),
                };
                self.record(
                    index,
                    TransitionReason::RegisteredWithOperator,
                    &detail,
                    log,
                )?;
            }

            Influence::Claim { rider, driver } => {
                debug_assert_eq!(
                    self.agents[index].id, driver,
                    "a driver emits its own claim"
                );
                let rider_index = rider.0 as usize;
                let seats = self.agents[rider_index].seats;
                // Another driver claimed it this tick, or the seats left no longer fit its party.
                // Neither is an error: the driver looks again next tick.
                if !(self.agents[rider_index].awaits_advance_match()
                    || self.agents[rider_index].awaits_pair_match())
                    || self.agents[index]
                        .vehicle
                        .as_ref()
                        .is_some_and(|vehicle| vehicle.free_seats() < seats)
                {
                    self.claim_collisions += 1;
                    return Ok(());
                }

                let line = self.agents[index]
                    .line
                    .expect("a driver is registered before it claims");
                let name = self.agents[index].label.clone();
                let rider_agent = &mut self.agents[rider_index];
                rider_agent.line = Some(line);
                rider_agent.claimed_by = Some(driver);
                // Out of every pool and back into one: from here the rider is walking to this
                // line's station, and a driver on another line must not reach it there.
                self.operator.unregister(rider);
                self.operator.register_rider(rider, line);
                self.record(
                    rider_index,
                    TransitionReason::MatchedInAdvance,
                    &format!("claimed by {name}"),
                    log,
                )?;
            }

            Influence::Travel {
                to,
                station,
                profile,
                state,
                reason,
            } => {
                let from = self.agents[index].position();
                let journey = self.router.route(from, to, profile).into_journey();
                // Anyone aboard rides the same geometry at the same speed, so their positions and
                // their accumulated kilometres are the vehicle's without any per-tick coupling
                // between the two agents.
                let passengers = self.agents[index]
                    .vehicle
                    .as_ref()
                    .map(|vehicle| vehicle.riders.clone())
                    .unwrap_or_default();
                for rider in &passengers {
                    self.agents[rider.0 as usize].journey = Some(journey.clone());
                }

                let agent = &mut self.agents[index];
                agent.journey = Some(journey);
                agent.station = station;
                // The realised departure, against the `departure_s` the agent declared.
                agent.departed_s = agent.departed_s.or(Some(self.time_s));
                enter(agent, state, reason, self.time_s);
                let detail =
                    station.map_or(String::new(), |id| self.world.station(id).name.clone());
                self.log_state(index, &detail, log)?;
            }

            Influence::Transition { state, reason } => {
                enter(&mut self.agents[index], state, reason, self.time_s);
                // An on-demand driver whose line has fallen behind it has nothing left to offer the
                // operator either, though it is still driving.
                if state == State::RideToOwnDestination {
                    let id = self.agents[index].id;
                    self.operator.unregister(id);
                }
                self.settle(index);
                self.log_state(index, "", log)?;
            }

            Influence::Pickup { rider, driver } => {
                debug_assert_eq!(
                    self.agents[index].id, driver,
                    "a driver emits its own pickup"
                );
                let rider_index = rider.0 as usize;
                let seats = self.agents[rider_index].seats;
                let taken = self.agents[rider_index].state != State::WaitingDriver;
                let no_room = self.agents[index]
                    .vehicle
                    .as_ref()
                    .is_some_and(|vehicle| vehicle.free_seats() < seats);
                // Another driver got there first this tick, or the rider's party outgrew the
                // seats left. Neither is an error: the driver tries again next tick.
                if taken || no_room {
                    return Ok(());
                }

                let driver_agent = &mut self.agents[index];
                let vehicle = driver_agent
                    .vehicle
                    .as_mut()
                    .expect("only a driver emits a pickup");
                vehicle.seats_used += seats;
                vehicle.riders.push(rider);
                driver_agent.pickups += 1;
                let name = driver_agent.label.clone();

                // Picked up on the road rather than at a station: the vehicle is partway along a leg,
                // and the rider rides the rest of it from here.
                let on_the_road = matches!(
                    self.agents[index].state,
                    State::RideToOwnDestination
                        | State::RideToOwnDestinationRegistered
                        | State::RideCarpoolToDestinationStation
                );
                let leg = match on_the_road {
                    true => self.agents[index].journey.clone(),
                    false => None,
                };
                let rider_agent = &mut self.agents[rider_index];
                if leg.is_some() {
                    rider_agent.journey = leg;
                }
                // Every wait it has had so far: one for a trip of one leg, all of them for a network
                // fleet rider that has changed on its way.
                let waited_s = self.time_s - rider_agent.state_since_s;
                rider_agent.wait_s = Some(rider_agent.wait_s.map_or(waited_s, |s| s + waited_s));
                rider_agent.carried_by = Some(driver);
                enter(
                    rider_agent,
                    State::RideCarpoolToDestination,
                    TransitionReason::MatchedToDriver,
                    self.time_s,
                );
                self.operator.unregister(rider);
                // Where the fleet has actually collected people, which is the only history the
                // repositioning decision is given. Carpool pickups happen at a station by
                // construction and say nothing a policy does not already read off the registry.
                if self.agents[index].kind == AgentKind::AutonomousTaxi {
                    if let Some(nearest) = self.nearest_station(self.agents[rider_index].position())
                    {
                        self.pickups_by_station[nearest.0 as usize] += seats;
                    }
                }
                self.log_state(rider_index, &format!("picked up by {name}"), log)?;
            }

            Influence::NextLeg { rider } => {
                debug_assert_eq!(self.agents[index].id, rider, "a rider moves itself on");
                let time_s = self.time_s;
                let agent = &mut self.agents[index];
                agent.leg += 1;
                // A new hail: whichever taxi carried the last leg is not coming for this one.
                agent.claimed_by = None;
                enter(
                    agent,
                    State::WaitingDriver,
                    TransitionReason::Transferred,
                    time_s,
                );
                self.log_state(index, "", log)?;
            }

            Influence::Rechain { driver } => {
                debug_assert_eq!(self.agents[index].id, driver, "a driver chains itself");
                self.operator.unregister(driver);
                // A rider it claimed and never picked up is not coming along: it stays on the old
                // line and waits there for whoever drives it next, as it would if the driver had
                // simply gone home.
                for rider in self.agents.iter_mut() {
                    if rider.claimed_by == Some(driver)
                        && matches!(
                            rider.state,
                            State::WaitingDeparture
                                | State::RideToDepartureStation
                                | State::WaitingDriver
                        )
                    {
                        rider.claimed_by = None;
                    }
                }
                let time_s = self.time_s;
                let agent = &mut self.agents[index];
                // The trip starts again here and now, towards the destination it always had.
                agent.origin = agent.position();
                agent.line = None;
                agent.station = None;
                agent.departure_s = time_s;
                agent.departure_noise_s = 0.0;
                agent.window = DepartureWindow {
                    earliest_s: time_s,
                    latest_s: time_s,
                };
                agent.chains += 1;
                enter(
                    agent,
                    State::WaitingDeparture,
                    TransitionReason::Rechained,
                    time_s,
                );
                self.log_state(index, "", log)?;
            }

            Influence::DropOff { driver } => {
                debug_assert_eq!(
                    self.agents[index].id, driver,
                    "a driver emits its own drop-off"
                );
                let station = self.agents[index]
                    .station
                    .map(|id| self.world.station(id).name.clone())
                    .unwrap_or_default();
                // Whoever gets out here; the rest ride on to their own drop points, and the driver
                // stays on its loaded leg until the last of them is down.
                let aboard = self.agents[index]
                    .vehicle
                    .as_ref()
                    .map(|vehicle| vehicle.riders.clone())
                    .unwrap_or_default();
                let (getting_out, staying): (Vec<AgentId>, Vec<AgentId>) =
                    aboard.into_iter().partition(|rider| {
                        drop_point(
                            &self.agents[index],
                            &self.agents[rider.0 as usize],
                            &self.world,
                            &self.agents,
                        )
                        .reached_by(&self.agents[index])
                    });
                let staying_seats: u32 = staying
                    .iter()
                    .map(|rider| self.agents[rider.0 as usize].seats)
                    .sum();
                let passengers = match &mut self.agents[index].vehicle {
                    Some(vehicle) => {
                        vehicle.seats_used = vehicle.reserved + staying_seats;
                        vehicle.riders = staying.clone();
                        getting_out
                    }
                    None => Vec::new(),
                };

                for rider in passengers {
                    let rider_index = rider.0 as usize;
                    // The journey is left alone: it is finished, and its final point is where the
                    // rider is standing. Clearing it would fall back to the agent's origin.
                    enter(
                        &mut self.agents[rider_index],
                        State::ArrivalDestinationStation,
                        TransitionReason::DroppedOff,
                        self.time_s,
                    );
                    self.log_state(rider_index, &station, log)?;
                }

                if staying.is_empty() {
                    enter(
                        &mut self.agents[index],
                        State::ArrivalDestinationStation,
                        TransitionReason::ArrivedAtStation,
                        self.time_s,
                    );
                    self.log_state(index, &station, log)?;
                }
            }
        }
        Ok(())
    }

    /// The station nearest a point, or `None` on a scenario with no stations at all - which a
    /// door-to-door fleet is allowed to be.
    fn nearest_station(&self, at: Coord) -> Option<StationId> {
        self.world
            .stations()
            .iter()
            .enumerate()
            .min_by(|left, right| {
                haversine_km(at, left.1.coord).total_cmp(&haversine_km(at, right.1.coord))
            })
            .map(|(index, _)| StationId(index as u32))
    }

    fn settle(&mut self, index: usize) {
        let agent = &self.agents[index];
        if !agent.state.is_active() {
            let id = agent.id;
            self.operator.unregister(id);
        }
    }

    fn log_state(&mut self, index: usize, detail: &str, log: &mut EventLog) -> Result<()> {
        let agent = &self.agents[index];
        log.record(
            self.time_s,
            agent.id,
            agent.kind,
            agent.state,
            agent.reason,
            agent.position(),
            agent.vehicle.as_ref().map(|vehicle| vehicle.seats_used),
            agent.distance_km,
            detail,
        )
    }

    /// Log a row without changing state - used where something happened to an agent that its
    /// state does not record, such as registering with the operator.
    fn record(
        &mut self,
        index: usize,
        reason: TransitionReason,
        detail: &str,
        log: &mut EventLog,
    ) -> Result<()> {
        let agent = &self.agents[index];
        log.record(
            self.time_s,
            agent.id,
            agent.kind,
            agent.state,
            reason,
            agent.position(),
            agent.vehicle.as_ref().map(|vehicle| vehicle.seats_used),
            agent.distance_km,
            detail,
        )
    }

    /// The physical invariants, checked every tick in debug builds so a logic regression fails a
    /// plain `cargo run` rather than surfacing as a quietly wrong number three phases later.
    ///
    /// Monotonic distance is asserted where it is produced, in `Journey::advance`.
    fn check_invariants(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        let mut aboard: Vec<AgentId> = Vec::new();
        for agent in &self.agents {
            // A claimed rider walks to the line its driver chose, and nothing moves it off that
            // line afterwards. If those two ever disagree, the rider is standing at one station
            // waiting for a vehicle that is going to another.
            // Only while it is still on its way to be picked up: afterwards the claim is history, and a
            // driver that has chained since holds a different line, or none.
            let awaiting = matches!(
                agent.state,
                State::WaitingDeparture | State::RideToDepartureStation | State::WaitingDriver
            );
            let claimed = agent.claimed_by.filter(|driver| *driver != agents::NO_TAXI);
            if let (Some(driver), true) = (claimed, awaiting) {
                assert_eq!(
                    agent.line, self.agents[driver.0 as usize].line,
                    "agent {:?} was claimed onto a different line than its driver's",
                    agent.id
                );
            }
            // A fleet taxi with nobody aboard is idle and available; one that still holds a
            // rider after being released would be a passenger nothing will ever set down.
            if agent.kind == AgentKind::AutonomousTaxi && !agent.state.is_active() {
                assert!(
                    agent
                        .vehicle
                        .as_ref()
                        .is_some_and(|vehicle| vehicle.riders.is_empty()),
                    "fleet taxi {:?} was released still carrying somebody",
                    agent.id
                );
            }
            let Some(vehicle) = &agent.vehicle else {
                continue;
            };
            assert!(
                vehicle.seats_used <= vehicle.capacity,
                "agent {:?} carries {} in {} seats",
                agent.id,
                vehicle.seats_used,
                vehicle.capacity
            );
            for rider in &vehicle.riders {
                assert!(
                    !aboard.contains(rider),
                    "agent {rider:?} is in two vehicles at once"
                );
                assert_eq!(
                    self.agents[rider.0 as usize].state,
                    State::RideCarpoolToDestination,
                    "agent {rider:?} is aboard {:?} but not in a riding state",
                    agent.id
                );
                assert_eq!(
                    self.agents[rider.0 as usize].carried_by,
                    Some(agent.id),
                    "agent {rider:?} is aboard {:?} and recorded as carried by somebody else",
                    agent.id
                );
                aboard.push(*rider);
            }
        }
    }

    /// Close out the agents still going when the clock cap hit, so the log accounts for all of
    /// them rather than trailing off.
    fn strand_remaining(&mut self, log: &mut EventLog) -> Result<u32> {
        let mut stranded = 0;
        for index in 0..self.agents.len() {
            if !self.agents[index].state.is_active() {
                continue;
            }
            let time_s = self.time_s;
            enter(
                &mut self.agents[index],
                State::EndJourney,
                TransitionReason::ClockRanOut,
                time_s,
            );
            self.log_state(index, "clock cap reached", log)?;
            stranded += 1;
        }
        Ok(stranded)
    }
}

fn enter(agent: &mut Agent, state: State, reason: TransitionReason, time_s: f64) {
    agent.state = state;
    agent.reason = reason;
    agent.state_since_s = time_s;
}

/// One agent's knobs, drawn from the cohort's distributions.
///
/// Every draw happens for every agent and in this order, whether the knob is a distribution or a
/// bare number, so a `Fixed` knob consumes nothing and giving one knob a distribution leaves the
/// other knobs' draws exactly where they were.
fn new_agent(
    cohort: &Cohort,
    spawn_s: f64,
    origin: Coord,
    destination: Coord,
    rng: &mut ChaCha8Rng,
) -> Agent {
    let departure_offset_s = cohort.departure_offset_s.draw_non_negative(rng);
    let max_wait_s = cohort.max_wait_s.draw_non_negative(rng);
    let max_walk_s = cohort.max_walk_s.draw_non_negative(rng);
    let additional_passengers = cohort.additional_passengers.draw_count(rng);
    // Drawn last, after every knob that existed before it, so a scenario that does not set it
    // consumes exactly the randomness it always did.
    let max_detour_pct = cohort
        .max_detour_pct
        .as_ref()
        .map(|value| value.draw(rng).max(100.0));
    // Last again, for the same reason. A lead the noise overruns leaves no earlier than declaring
    // allowed: the agent cannot set off before it has told anyone it is going.
    let lead_s = cohort.advanced_declaration_lead_s;
    let departure_noise_s = (lead_s + cohort.departure_noise_s.draw(rng)).max(0.0) - lead_s;
    // After that, and only when set, so nothing before it moves.
    let transfer_max_wait_s = match &cohort.transfer_max_wait_s {
        Some(value) => value.draw_non_negative(rng),
        None => max_wait_s,
    };

    // A party drawn larger than the vehicle would seat nobody at all, itself included.
    // Validation rejects a fixed figure that cannot fit; a drawn one is clamped to the seats
    // there are, because a distribution with a long tail is not a mistake.
    let seats = match &cohort.vehicle {
        Some(vehicle) => (additional_passengers + 1).min(vehicle.capacity),
        None => additional_passengers + 1,
    };
    let departure_s = spawn_s + departure_offset_s;
    Agent {
        // Replaced with the arena slot when the agent spawns.
        id: AgentId(u32::MAX),
        kind: cohort.kind,
        label: cohort.name.clone(),
        origin,
        destination,
        seats,
        spawn_s,
        departure_s,
        departure_noise_s,
        max_wait_s,
        max_walk_s,
        max_detour_pct,
        station_approach: cohort.station_approach,
        takes_unregistered_riders: cohort.takes_unregistered_riders,
        rechain: cohort.rechain,
        chains: 0,
        chained_drop_offs: cohort.chained_drop_offs,
        group: None,
        service: cohort.service,
        legs: Vec::new(),
        leg: 0,
        transfer_max_wait_s,
        assigned_wait_factor: cohort.assigned_wait_factor,
        walks_unclaimed: cohort.walks_unclaimed,
        times_to_counterpart: cohort.times_to_counterpart,
        perception_radius_km: cohort.perception_radius_km,
        registers_after_first_pickup: cohort.registers == Registers::AfterFirstPickup,
        declaration_lead_s: cohort.advanced_declaration_lead_s,
        // The interval, once, rather than the two margins to be added and subtracted wherever a
        // departure time is weighed. Validation keeps both margins non-negative, so it never
        // comes out inverted.
        window: DepartureWindow {
            earliest_s: departure_s - cohort.earliness_margin_s,
            latest_s: departure_s + cohort.lateness_margin_s,
        },
        declared_s: None,
        departed_s: None,
        claimed_by: None,
        carried_by: None,
        state: State::WaitingDeparture,
        reason: TransitionReason::Spawned,
        state_since_s: spawn_s,
        line: None,
        station: None,
        journey: None,
        vehicle: cohort.vehicle.as_ref().map(|vehicle| Vehicle {
            capacity: vehicle.capacity,
            // Nobody rides with a robot, so a fleet vehicle reserves no seat for a driver and its
            // capacity is the number of passengers the service can actually sell.
            reserved: reserved_seats(cohort.kind, seats),
            seats_used: reserved_seats(cohort.kind, seats),
            riders: Vec::new(),
        }),
        distance_km: 0.0,
        loaded_km: 0.0,
        wait_s: None,
        pickups: 0,
    }
}

/// The vehicle a departure guarantee dispatches: it starts at the depot, drives the line it was
/// called for and leaves the station as soon as there is nobody left to take, which is what a
/// `max_wait_s` of zero says. Its own trip *is* the line, so its destination is the line's
/// destination station and the leg home is nothing.
///
/// It is registered on its line before it exists as far as the operator is concerned, so the
/// machine in [`crate::agents::driver::decide_registered`] skips the line query and drives.
fn guarantee_vehicle(
    id: AgentId,
    line: LineId,
    guarantee: &DepartureGuarantee,
    destination: Coord,
    time_s: f64,
) -> Agent {
    Agent {
        id,
        kind: AgentKind::TaxiDriver,
        label: "departure guarantee".to_string(),
        origin: Coord::new(guarantee.latitude, guarantee.longitude),
        destination,
        seats: 1,
        spawn_s: time_s,
        departure_s: time_s,
        departure_noise_s: 0.0,
        max_wait_s: 0.0,
        max_walk_s: 0.0,
        // It was called for one line and drives that line; there is no alternative to weigh.
        max_detour_pct: None,
        station_approach: StationApproach::Always,
        takes_unregistered_riders: false,
        rechain: false,
        chains: 0,
        chained_drop_offs: false,
        group: None,
        service: Service::DoorToDoor,
        legs: Vec::new(),
        leg: 0,
        transfer_max_wait_s: 0.0,
        assigned_wait_factor: 1.0,
        walks_unclaimed: false,
        times_to_counterpart: false,
        perception_radius_km: None,
        registers_after_first_pickup: false,
        declaration_lead_s: 0.0,
        window: DepartureWindow {
            earliest_s: time_s,
            latest_s: time_s,
        },
        declared_s: Some(time_s),
        departed_s: None,
        claimed_by: None,
        carried_by: None,
        state: State::WaitingDeparture,
        reason: TransitionReason::GuaranteeDispatched,
        state_since_s: time_s,
        line: Some(line),
        station: None,
        journey: None,
        vehicle: Some(Vehicle {
            capacity: guarantee.capacity,
            // Validation keeps the capacity at two or more, so there is always a seat to offer.
            reserved: 1,
            seats_used: 1,
            riders: Vec::new(),
        }),
        distance_km: 0.0,
        loaded_km: 0.0,
        wait_s: None,
        pickups: 0,
    }
}

/// A passenger of `driver`'s pre-formed group: the same origin, destination, spawn and window, the
/// driver's patience and walk, one seat, and no lead of its own - it is not matched in advance, it
/// takes the line its driver took. Built from the driver rather than drawn, so a group costs no
/// randomness beyond its size.
fn group_passenger(driver: &Agent, group: u32) -> Agent {
    Agent {
        id: AgentId(u32::MAX),
        kind: AgentKind::PolynomialRider,
        label: format!("{} (group)", driver.label),
        origin: driver.origin,
        destination: driver.destination,
        seats: 1,
        spawn_s: driver.spawn_s,
        departure_s: driver.departure_s,
        departure_noise_s: driver.departure_noise_s,
        max_wait_s: driver.max_wait_s,
        max_walk_s: driver.max_walk_s,
        max_detour_pct: None,
        station_approach: StationApproach::Always,
        takes_unregistered_riders: false,
        rechain: false,
        chains: 0,
        chained_drop_offs: false,
        perception_radius_km: None,
        registers_after_first_pickup: false,
        group: Some(group),
        service: Service::DoorToDoor,
        legs: Vec::new(),
        leg: 0,
        transfer_max_wait_s: driver.max_wait_s,
        assigned_wait_factor: 1.0,
        walks_unclaimed: false,
        times_to_counterpart: false,
        declaration_lead_s: 0.0,
        window: driver.window,
        declared_s: None,
        departed_s: None,
        claimed_by: None,
        carried_by: None,
        state: State::WaitingDeparture,
        reason: TransitionReason::Spawned,
        state_since_s: driver.spawn_s,
        line: None,
        station: None,
        journey: None,
        vehicle: None,
        distance_km: 0.0,
        loaded_km: 0.0,
        wait_s: None,
        pickups: 0,
    }
}

/// Seats a vehicle's own party holds for the whole run. Zero for a fleet vehicle, which has no
/// driver in it.
fn reserved_seats(kind: AgentKind, seats: u32) -> u32 {
    match kind {
        AgentKind::AutonomousTaxi => 0,
        _ => seats,
    }
}

fn sample_range(rng: &mut ChaCha8Rng, start: f64, end: f64) -> f64 {
    if end > start {
        rng.random_range(start..end)
    } else {
        start
    }
}

/// A point drawn uniformly from the disc around the area's centre. The square root keeps the
/// draw uniform by area rather than clustering it at the centre.
fn sample_area(rng: &mut ChaCha8Rng, area: &SpawnArea) -> Coord {
    let centre = Coord::new(area.latitude, area.longitude);
    if area.jitter_radius_km <= 0.0 {
        return centre;
    }

    let bearing = rng.random_range(0.0..std::f64::consts::TAU);
    let radius_km = area.jitter_radius_km * rng.random_range(0.0..1.0f64).sqrt();
    let latitude = centre.latitude + radius_km * bearing.cos() / KM_PER_DEGREE_LATITUDE;
    let longitude = centre.longitude
        + radius_km * bearing.sin()
            / (KM_PER_DEGREE_LATITUDE * centre.latitude.to_radians().cos().max(1e-6));
    Coord::new(latitude, longitude)
}
