//! The agent arena and the influence–reaction cycle.
//!
//! Every tick runs `perceive → decide → apply`. [`decide`] is a pure function of an agent, the
//! world and what it is allowed to know: it mutates nothing and returns an [`Influence`], which
//! the simulation resolves in a separate pass. No agent ever writes into another, so a result
//! never depends on which agent happened to be updated first.
//!
//! Pickup and drop-off are where that matters most. A driver cannot reach into a rider and seat
//! it; it emits [`Influence::Pickup`], and the apply pass checks the rider is still waiting and
//! the seats are still free before anything moves. Two drivers reaching for the same rider on the
//! same tick both emit; only the first to be applied gets one.

pub mod driver;
pub mod rider;
pub mod taxi;

use anyhow::Result;

use crate::operator::Operator;
use crate::policy::Policy;
use crate::routing::{Profile, Router};
use crate::scenario::{AgentKind, Service, StationApproach};
use crate::world::{haversine_km, Coord, Journey, LineId, StationId, World};

/// Index of an agent in the arena, assigned when it enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId(pub u32);

/// What a fleet rider is `claimed_by` when it was told a taxi is coming and none ever will - an
/// assignment `urgency-as-ran` overwrote. No arena slot is ever this.
pub const NO_TAXI: AgentId = AgentId(u32::MAX);

/// Where an agent is in its journey.
///
/// Every `match` on this is exhaustive, so a variant added for a later behaviour is a compile
/// error in every machine that has not been taught to handle it. Rider states and driver states
/// share one enum: they overlap where the behaviour does, and a machine that reaches a state
/// belonging to the other family is a bug the exhaustive match surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Spawned, and not yet registered or past its departure time.
    WaitingDeparture,
    /// On the way to the station it will be picked up at, or picks up at.
    RideToDepartureStation,

    // --- rider ---
    /// At the departure station, waiting for a driver.
    WaitingDriver,
    /// Aboard a vehicle, on the way to the destination station.
    RideCarpoolToDestination,
    /// Set down at the destination station.
    ArrivalDestinationStation,
    /// Covering the last leg from the destination station under its own steam.
    RideToArrival,
    /// Gave up before being carried.
    Canceled,

    // --- driver ---
    /// At the departure station, picking riders up until it is full or out of patience.
    ArrivalDepartureStation,
    /// Driving the line, carrying whoever it picked up.
    RideCarpoolToDestinationStation,
    /// Carrying on from the destination station to where it was going anyway.
    RideToOwnDestination,
    /// Driving its own trip while still registered on its line, ready to divert to the line's
    /// station if a rider needs it. Only a driver whose `station_approach` is `on_demand`.
    RideToOwnDestinationRegistered,

    /// Done, one way or another.
    EndJourney,
}

impl State {
    /// Whether this agent still has something left to do. A run ends when none do.
    pub fn is_active(self) -> bool {
        !matches!(self, State::Canceled | State::EndJourney)
    }
}

/// Why a transition fired. The state machine does not depend on this; the analysis does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    Spawned,
    RegisteredWithOperator,
    DepartureTimeReached,
    ArrivedAtStation,
    /// A rider was seated in a vehicle.
    MatchedToDriver,
    /// A driver left the departure station because every seat was taken.
    VehicleFull,
    /// A rider was set down at the destination station.
    DroppedOff,
    ArrivedAtDestination,
    /// A rider's patience ran out, or a driver stopped waiting for riders that never came.
    WaitTimedOut,
    /// The operator dispatched a guarantee vehicle to a rider that had waited past the trigger.
    GuaranteeDispatched,
    /// The operator declined to spend a guarantee vehicle on a rider past the trigger. A decision
    /// rather than an omission, so it leaves a row in the log like every other one.
    GuaranteeDeclined,
    /// The operator sent an idle fleet vehicle somewhere else to wait. Still idle while it goes.
    Repositioned,
    /// The fleet operator assigned a taxi to a rider, and the taxi set off to collect it. The
    /// fleet's counterpart of a carpool match: nobody chose, the operator paired them.
    AssignedByOperator,
    /// A fleet taxi was released, having no demand left to serve.
    StoodDown,
    /// A ghost took a line without telling the operator, so nothing the operator answers will
    /// offer it to anybody.
    TookLineUnregistered,
    /// Every line cost this driver more of a detour than it would accept, so it drove its own
    /// trip and carried nobody.
    DetourRefused,
    /// The operator had no line to offer.
    NoLineAvailable,
    /// An on-demand driver turned towards its line's station because a rider there needed it.
    RiderNeedsStop,
    /// An on-demand driver reached the station and found nobody, so it carried on its own way.
    NobodyAtStation,
    /// An on-demand driver took everyone who was at the station and left without waiting.
    TookEveryoneWaiting,
    /// An on-demand driver's line fell behind it, so it unregistered and drove on as a private car.
    LinePassed,
    /// A driver set its riders down and started choosing a line again from where it stood.
    Rechained,
    /// A driver picked somebody up on the road and set off to set them down.
    PickedUpOnTheRoad,
    /// A network fleet rider was set down at a station on its way and hailed the next leg.
    Transferred,
    /// A pre-formed group's driver had its whole group aboard and left.
    GroupAboard,
    /// A driver took a rider that had declared ahead and not set off yet, so the rider now knows
    /// which of the lines it declared on to walk to.
    MatchedInAdvance,
    ClockRanOut,
}

/// The departure times an agent will accept: the time it declared, widened by the margins its
/// cohort gave.
///
/// An explicit interval computed once, rather than an inequality chain repeated at each site
/// that has to decide whether two agents can travel together - a sign error in one of those is
/// silent and changes results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DepartureWindow {
    pub earliest_s: f64,
    pub latest_s: f64,
}

impl DepartureWindow {
    /// Whether the two agents could depart at the same time. This is the whole of what the
    /// margins do: they decide who may be matched with whom ahead of time.
    pub fn overlaps(self, other: DepartureWindow) -> bool {
        self.earliest_s <= other.latest_s && other.earliest_s <= self.latest_s
    }
}

/// A driver's vehicle, and who is in it.
///
/// Occupancy lives on the driver rather than in the world: a vehicle has no existence apart from
/// the agent driving it, and putting it here is what keeps seat accounting in one place.
#[derive(Debug)]
pub struct Vehicle {
    pub capacity: u32,
    /// Seats the driver's own party takes for the whole run - the driver, plus anyone already
    /// travelling with it. Never freed.
    pub reserved: u32,
    /// Seats taken right now, the driver's own party included.
    pub seats_used: u32,
    /// Riders aboard, in pickup order.
    pub riders: Vec<AgentId>,
}

impl Vehicle {
    pub fn free_seats(&self) -> u32 {
        self.capacity - self.seats_used
    }

    /// People being carried beyond the driver's own party.
    pub fn carried(&self) -> u32 {
        self.seats_used - self.reserved
    }
}

/// One agent in the arena.
#[derive(Debug)]
pub struct Agent {
    /// Index into the arena, assigned on spawn.
    pub id: AgentId,
    pub kind: AgentKind,

    /// The cohort label, carried so the event log stays readable.
    pub label: String,

    pub origin: Coord,
    pub destination: Coord,

    /// Seats this agent needs: itself, plus anyone travelling with it.
    pub seats: u32,

    /// When the agent entered the simulation.
    pub spawn_s: f64,
    /// The earliest the agent is willing to set off.
    pub departure_s: f64,
    /// How much later than it declared this agent actually leaves: the cohort's
    /// `departure_noise_s` drawn and floored so it never leaves before `lead` ahead of spawning
    /// would allow. Zero unless the cohort asked for noise.
    pub departure_noise_s: f64,
    /// How long it will wait at a station: for a rider, before giving up; for a driver, before
    /// leaving with whoever is aboard.
    pub max_wait_s: f64,

    /// How long this agent is willing to spend on foot reaching a line and leaving it. A line
    /// asking for more is one it declines; the threshold is the agent's, not the operator's, so
    /// one cohort walks twenty minutes and another five. Ignored by drivers.
    pub max_walk_s: f64,

    /// How much longer than driving straight there a driver will let its trip become, as a
    /// percentage of the direct route. `None` accepts any detour. The driver's counterpart of
    /// `max_walk_s`: the operator ranks and never refuses, so this is where a driver declines.
    pub max_detour_pct: Option<f64>,

    /// Whether a registered driver goes to its station regardless, or only when a rider needs it.
    pub station_approach: StationApproach,
    /// Whether a carpool driver at its station also takes riders who never registered.
    pub takes_unregistered_riders: bool,
    /// Whether a registered driver chooses a line again after setting its riders down.
    pub rechain: bool,
    /// How many times it has. Nonzero means its origin is the station it last set down at, and
    /// only lines ahead of it that bring it nearer home are offered.
    pub chains: u32,
    /// A fleet taxi that sets each rider down at its own door rather than all at the first one's.
    pub chained_drop_offs: bool,
    /// The pre-formed group this agent belongs to - its driver and the passengers spawned with it
    /// share the number. `None` for everyone else.
    pub group: Option<u32>,
    /// How a fleet rider is served: door to door, or over the lines.
    pub service: Service,
    /// A network fleet rider's legs: every station it changes at, in order, then its door. Empty for
    /// anyone served door to door, whose one leg ends at `destination`.
    pub legs: Vec<Coord>,
    /// Which of them it is on.
    pub leg: usize,
    /// A network fleet rider's patience at every change after the first leg.
    pub transfer_max_wait_s: f64,
    /// How much its patience stretches once a taxi has been assigned to it.
    pub assigned_wait_factor: f64,
    /// Whether this declaring rider sets off unclaimed at its departure time.
    pub walks_unclaimed: bool,
    /// Whether this declaring agent times its leaving to the one it was matched with.
    pub times_to_counterpart: bool,
    /// How far from this moving vehicle a waiting rider may be and still be picked up. `None` for
    /// every driver that only picks up at a station.
    pub perception_radius_km: Option<f64>,
    /// An opportunistic driver that registers only after its first pickup on the road.
    pub registers_after_first_pickup: bool,

    /// How far ahead of `departure_s` this agent announces its trip to the operator. Zero is an
    /// agent that is matched only on the spot.
    pub declaration_lead_s: f64,
    /// The departure times it will accept, around `departure_s`.
    pub window: DepartureWindow,
    /// When the operator was told about this trip. `None` until it is.
    pub declared_s: Option<f64>,
    /// When the agent actually set off, against the `departure_s` it declared.
    pub departed_s: Option<f64>,
    /// The driver that took this rider before it set off, so it knows which of the lines it
    /// declared on to walk to. `None` for a rider matched at the station, and for every driver.
    pub claimed_by: Option<AgentId>,
    /// The driver that actually carried this rider. Set when it is seated, and never cleared:
    /// after the run it is the only record of which vehicle a rider's kilometres belong to, which
    /// is what lets a trip be priced per driver rather than per rider. `None` for a rider nobody
    /// picked up, and for every driver.
    pub carried_by: Option<AgentId>,

    pub state: State,
    pub reason: TransitionReason,
    /// When the current state was entered, so a wait is `time_s - state_since_s`.
    pub state_since_s: f64,

    /// The line the operator matched this agent onto. `None` until it registers.
    pub line: Option<LineId>,
    /// The station this agent is heading for or standing at.
    pub station: Option<StationId>,
    pub journey: Option<Journey>,

    /// A driver's vehicle. `None` for riders.
    pub vehicle: Option<Vehicle>,

    /// Distance covered across every journey so far.
    pub distance_km: f64,
    /// Of that, the part covered while carrying someone (drivers) or being carried (riders).
    pub loaded_km: f64,
    /// How long a rider waited at the departure station before it was picked up. `None` if it
    /// never was.
    pub wait_s: Option<f64>,
    /// How many riders a driver picked up.
    pub pickups: u32,
}

impl Agent {
    /// Where the agent is: partway along its current journey, or at its origin before it starts.
    pub fn position(&self) -> Coord {
        match &self.journey {
            Some(journey) => journey.position(),
            None => self.origin,
        }
    }

    /// Whether this agent completed the trip it set out on, as opposed to giving up or being cut
    /// off by the clock cap.
    pub fn arrived(&self) -> bool {
        self.state == State::EndJourney && self.reason == TransitionReason::ArrivedAtDestination
    }

    /// Where this agent's current leg ends: the next station of a network fleet trip, or its own
    /// destination for everyone else.
    pub fn target(&self) -> Coord {
        self.legs.get(self.leg).copied().unwrap_or(self.destination)
    }

    /// Whether this agent is on the last leg of its trip, or has only the one.
    pub fn on_last_leg(&self) -> bool {
        self.leg + 1 >= self.legs.len()
    }

    /// How long it will wait on its current leg before giving up: its first-leg patience or its
    /// transfer patience, stretched once a taxi is coming for it.
    pub fn patience_s(&self) -> f64 {
        let base_s = match self.leg {
            0 => self.max_wait_s,
            _ => self.transfer_max_wait_s,
        };
        match self.claimed_by {
            Some(_) => base_s * self.assigned_wait_factor,
            None => base_s,
        }
    }

    /// When this agent actually sets off at the earliest: the start of its departure window, moved
    /// by its departure noise. The window itself stays where it was declared.
    pub fn earliest_leave_s(&self) -> f64 {
        self.window.earliest_s + self.departure_noise_s
    }

    /// When this agent actually sets off, for a machine with no window to leave inside: the time it
    /// declared, moved by its departure noise.
    pub fn leave_s(&self) -> f64 {
        self.departure_s + self.departure_noise_s
    }

    /// Whether this agent announces its trip before setting off, which is what lets it be
    /// matched in advance instead of only where it is standing.
    pub fn declares_ahead(&self) -> bool {
        self.declaration_lead_s > 0.0
    }

    /// When this agent tells the operator about its trip: its lead ahead of departure, and never
    /// before it exists. With no lead that is the departure time itself, which is an agent
    /// registering as it leaves.
    pub fn declaration_s(&self) -> f64 {
        (self.departure_s - self.declaration_lead_s).max(self.spawn_s)
    }

    /// A rider that declared ahead, is sitting in the pool of every line it would accept, and
    /// has not been taken by any driver yet.
    pub fn awaits_advance_match(&self) -> bool {
        self.state == State::WaitingDeparture && self.declares_ahead() && self.claimed_by.is_none()
    }

    /// A rider a pair-scoring policy may still divert: it registered on a line and is on its way
    /// to that line's station, so where it is standing is not yet settled and it can be sent to a
    /// different one.
    ///
    /// Deliberately **not** a rider already waiting at a station. Diverting one of those would
    /// make it walk away from a stop it had already reached, and re-entering a walking state
    /// restarts the clock its patience is measured against - a rider would gain a fresh half hour
    /// every time somebody claimed it. A rider still walking has no wait to restart.
    ///
    /// It needs the app for the same reason advance matching does: being told which stop to walk
    /// to is a message, and a kind that gets no messages is matched where it is standing.
    pub fn awaits_pair_match(&self) -> bool {
        self.state == State::RideToDepartureStation
            && self.claimed_by.is_none()
            && self.kind.uses_app()
    }
}

/// Whether `driver` may carry `rider` at all: a pre-formed group's driver carries its own group and
/// nobody else, and a group's passenger rides with its own driver and nobody else's.
pub fn may_carry(driver: &Agent, rider: &Agent) -> bool {
    match (
        driver.kind == AgentKind::PolynomialDriver,
        rider.kind == AgentKind::PolynomialRider,
    ) {
        (false, false) => true,
        (true, true) => driver.group.is_some() && driver.group == rider.group,
        _ => false,
    }
}

/// A group passenger of `driver`'s that is still to come: not aboard, not given up.
pub fn group_still_coming(driver: &Agent, agents: &[Agent]) -> bool {
    driver.group.is_some()
        && agents.iter().any(|rider| {
            rider.kind == AgentKind::PolynomialRider
                && rider.group == driver.group
                && matches!(
                    rider.state,
                    State::WaitingDeparture | State::RideToDepartureStation | State::WaitingDriver
                )
        })
}

/// When a rider will be standing at `station` if nothing times it: now if it already is, the rest
/// of its walk if it is walking, and its whole walk from when it leaves if it has not.
pub fn rider_arrival_s(rider: &Agent, station: Coord, context: &Context) -> Option<f64> {
    let walk_s = |from| {
        context
            .router
            .route(from, station, Profile::Foot)
            .duration_s
    };
    match rider.state {
        State::WaitingDriver => Some(context.time_s),
        State::RideToDepartureStation => Some(context.time_s + walk_s(rider.position())),
        State::WaitingDeparture => {
            Some(context.time_s.max(rider.earliest_leave_s()) + walk_s(rider.origin))
        }
        _ => None,
    }
}

/// Seats a driver has promised to riders it took in advance and has not picked up yet.
///
/// Derived from the arena rather than held on the vehicle, so a rider that gives up, or is taken
/// by another driver on the same line, releases the seat without anything having to remember to.
///
/// ponytail: a scan of the arena per claiming driver per tick. Fleets are in the tens; hold the
/// count on the vehicle if a sweep ever makes this show up in a profile.
pub fn claimed_seats(driver: AgentId, agents: &[Agent]) -> u32 {
    agents
        .iter()
        .filter(|agent| agent.claimed_by == Some(driver))
        .filter(|agent| {
            matches!(
                agent.state,
                State::WaitingDeparture | State::RideToDepartureStation | State::WaitingDriver
            )
        })
        .map(|agent| agent.seats)
        .sum()
}

/// Where a rider aboard `driver` is set down: its own line's destination station for a carpool
/// rider, and for a fleet rider its own door - or, unless the taxi chains its drop-offs, the door of
/// the first rider aboard, which everyone else walks on from.
pub fn drop_point(driver: &Agent, rider: &Agent, world: &World, agents: &[Agent]) -> DropPoint {
    if rider.kind == AgentKind::AutonomousTaxiRider {
        let door = match driver.chained_drop_offs {
            true => rider.target(),
            false => {
                let first = driver
                    .vehicle
                    .as_ref()
                    .and_then(|vehicle| vehicle.riders.first())
                    .map_or(rider.id, |id| *id);
                agents[first.0 as usize].target()
            }
        };
        return DropPoint::Door(door);
    }
    let line = rider
        .line
        .or(driver.line)
        .expect("a carpool rider aboard is on a line");
    DropPoint::Station(world.line(line).destination)
}

/// A place a rider gets out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropPoint {
    Station(StationId),
    Door(Coord),
}

impl DropPoint {
    /// Whether `driver` has reached it. A station is reached by name - the leg that ends there may
    /// end on a road-snapped point a few metres off the station's own coordinate - and a door by
    /// being within fifty metres of it.
    pub fn reached_by(self, driver: &Agent) -> bool {
        match self {
            DropPoint::Station(station) => driver.station == Some(station),
            DropPoint::Door(door) => haversine_km(driver.position(), door) <= SAME_PLACE_KM,
        }
    }

    pub fn coord(self, world: &World) -> Coord {
        match self {
            DropPoint::Station(station) => world.station(station).coord,
            DropPoint::Door(door) => door,
        }
    }

    pub fn station(self) -> Option<StationId> {
        match self {
            DropPoint::Station(station) => Some(station),
            DropPoint::Door(_) => None,
        }
    }
}

/// How close counts as the same place for a vehicle and someone at a kerb: fifty metres.
pub const SAME_PLACE_KM: f64 = 0.05;

/// The end of a vehicle's loaded leg: set down whoever gets out here, and when nobody aboard does,
/// drive on to the next rider's drop point, in pickup order.
pub fn drop_off_or_drive_on(agent: &Agent, world: &World, agents: &[Agent]) -> Influence {
    if !journey_finished(agent) {
        return Influence::Advance;
    }
    let riders: &[AgentId] = agent
        .vehicle
        .as_ref()
        .map_or(&[], |vehicle| vehicle.riders.as_slice());
    let points: Vec<DropPoint> = riders
        .iter()
        .map(|id| drop_point(agent, &agents[id.0 as usize], world, agents))
        .collect();
    // Emitted with an empty vehicle too: the drop-off is also what moves the driver on, so there
    // is one exit from the loaded leg rather than two.
    if points.is_empty() || points.iter().any(|point| point.reached_by(agent)) {
        return Influence::DropOff { driver: agent.id };
    }
    let next = points[0];
    Influence::Travel {
        to: next.coord(world),
        station: next.station(),
        profile: Profile::Road,
        state: agent.state,
        reason: TransitionReason::DroppedOff,
    }
}

/// The end of a leg, for every machine that has one: the transition once the journey is over,
/// and another tick of it until then.
pub fn arrive_or_advance(agent: &Agent, state: State, reason: TransitionReason) -> Influence {
    if journey_finished(agent) {
        Influence::Transition { state, reason }
    } else {
        Influence::Advance
    }
}

/// Whether the agent's current journey has reached its last point.
pub fn journey_finished(agent: &Agent) -> bool {
    agent
        .journey
        .as_ref()
        .is_some_and(|journey| journey.finished())
}

/// What an agent wants to happen this tick. Resolved by the simulation's apply pass.
#[derive(Debug)]
pub enum Influence {
    /// Nothing to do.
    Idle,
    /// Keep going along the current journey.
    Advance,
    /// Take `line` as the trip this agent is making, without moving or changing state. A
    /// commitment to a rendezvous: the agent sets off for that line's station.
    ///
    /// Whether the operator is told is the kind's business, not the influence's: an unregistered
    /// kind takes the line and stays invisible to every query the operator answers, which is the
    /// whole of what being a ghost costs it.
    Register { line: LineId },
    /// Announce a trip ahead of departing, on every line the agent would accept. It takes none of
    /// them and stays where it is, so a driver on any of them can take it and the station is only
    /// settled once one does. Riders only - a driver commits to the line it will drive.
    Declare { lines: Vec<LineId> },
    /// Take `rider` before it has set off, on the line `driver` is registered on. Refused by the
    /// apply pass if another driver got there first or the rider no longer fits.
    Claim { rider: AgentId, driver: AgentId },
    /// Set off towards `to` on `profile`, entering `state` on the way. The apply pass asks the
    /// router for the geometry, so an agent decides where it is going without knowing how the
    /// road gets there. Anyone aboard comes along.
    Travel {
        to: Coord,
        station: Option<StationId>,
        profile: Profile,
        state: State,
        reason: TransitionReason,
    },
    /// Change state without moving.
    Transition {
        state: State,
        reason: TransitionReason,
    },
    /// Seat `rider` in `driver`'s vehicle. Refused by the apply pass if the rider has already
    /// been taken this tick or no longer fits.
    Pickup { rider: AgentId, driver: AgentId },
    /// Set down everyone aboard `driver` at the station it has reached.
    DropOff { driver: AgentId },
    /// A network fleet rider set down partway: it starts the next leg of its trip from here, hailing
    /// again where it stands.
    NextLeg { rider: AgentId },
    /// Start the trip again from here: the driver has set its riders down and will choose a line
    /// once more, from where it stands, towards the destination it always had.
    Rechain { driver: AgentId },
}

/// What an agent is allowed to know about the run when deciding.
///
/// The arena is the one it saw at the start of the tick, so no agent ever reads a change another
/// agent made in the same tick.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub time_s: f64,
    pub operator: &'a Operator,
    pub agents: &'a [Agent],
    /// What every leg costs in time, so an agent can weigh a line before committing to it.
    pub router: &'a Router,
    /// Whether the run's policy matches `(driver, rider)` pairs across lines. It reaches the
    /// operator's queries and both machines, which is why it rides the context rather than being
    /// read off the policy: a `Context` is what every agent sees at once, and the policy itself is
    /// borrowed mutably by whichever agent is deciding.
    pub pairs: bool,
    /// For each arena slot, the first waiting fleet rider assigned to that slot's taxi, in arena
    /// order. Built once a tick so a fleet of thousands does not scan the arena once per taxi.
    pub fleet_assignments: &'a [Option<AgentId>],
}

/// Dispatch to the behaviour for this agent's kind.
///
/// `policy` decides the one thing here that is a policy rather than a behaviour: which line a
/// driver takes. It is passed rather than held on the [`Context`] because a policy is allowed to
/// be stateful, and a `Context` is what every agent reads at once. The operator's three other
/// decisions do not pass through an agent at all and are made in `sim.rs`.
///
/// This returns a `Result` for one reason: a policy may hold a pipe to a model, and a pipe that
/// has closed must end the run rather than quietly becoming a different policy.
///
/// Kinds whose behaviour is not written yet return [`Influence::Idle`]; the run warns about them
/// at startup and the clock cap ends the run.
pub fn decide(
    agent: &Agent,
    world: &World,
    context: &Context,
    policy: &mut dyn Policy,
) -> Result<Influence> {
    use AgentKind::*;
    Ok(match agent.kind {
        // The base machine on its own: the private car.
        PrivateDriver => driver::decide_private(agent, context),
        // A ghost rider runs the same machine: it picks a line from the same public ranking and
        // simply never registers on it, so only a driver at its station can find it.
        CarpoolRider | GhostRider => rider::decide_carpool(agent, world, context),
        // One machine for every driver the operator knows about. An opportunistic driver has no
        // app, and validation keeps a declaration lead off a kind without one, so the advance
        // claim disables itself rather than needing a branch here; a guarantee vehicle is
        // dispatched onto a line it is already registered on, so it skips the line query, and
        // stands down rather than driving that line with nobody aboard.
        CarpoolDriver | OpportunisticDriver => {
            driver::decide_registered(agent, world, context, Some(policy))?
        }
        // The one driver that chooses nothing: the operator called it for one line, so no policy
        // is offered it and the shared machine drives the line it arrived registered on.
        TaxiDriver => driver::decide_guarantee(agent, world, context),
        GhostDriver => driver::decide_ghost(agent, world, context, policy)?,
        NeglectfulDriver => driver::decide_neglectful(agent, world, context),
        // The on-demand fleet: door to door, and no line query on either side. The operator
        // assigns, so no policy is threaded here - a fleet taxi has nothing to choose between.
        AutonomousTaxi => taxi::decide_taxi(agent, world, context),
        AutonomousTaxiRider => taxi::decide_rider(agent, context),
        // A pre-formed group: the driver runs the registered machine, which carries only its own
        // group, and the passengers take the line their driver took.
        PolynomialDriver => driver::decide_registered(agent, world, context, Some(policy))?,
        PolynomialRider => rider::decide_carpool(agent, world, context),
    })
}
