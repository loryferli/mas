//! The autonomous fleet: the vehicle, its riders, and the operator that assigns one to the other.
//!
//! A fleet taxi has **no trip of its own**. It starts idle and exists only to serve, which is what
//! makes its empty kilometres a cost rather than an accounting artefact: a carpool driver's empty
//! kilometres are a trip somebody was making anyway, and a taxi's are pure overhead. That single
//! difference is why the fleet case reads differently in every metric, and it is the thing the
//! write-up has to be clearest about.
//!
//! Service is **door to door**. A rider is collected where it is standing and set down at its
//! destination, so the line query has no part in it and a fleet scenario may declare no lines at
//! all. The operator assigns and the taxi does not choose, so the policy reaches the fleet through
//! the operator - [`crate::policy::Policy::assign_fleet`] and
//! [`crate::policy::Policy::reposition`] - rather than through a taxi's own `decide`.
//!
//! **The operator is [`assign_context`] plus a [`crate::policy::Policy`], and it holds no
//! registry.** The arena already says which taxis are idle and which riders are waiting, and an
//! assignment already has a home on the agent - `Agent::claimed_by`, the same field a carpool
//! driver's advance claim writes. A registry would restate both. Split one out if two fleets ever
//! compete for the same riders.
//!
//! Under the fixed rules one taxi is assigned one rider and pooling happens at the pickup point.
//! `urgency` weighs riders by crowd and patience instead and sends a rider's neighbours the same
//! taxi; both are in [`crate::policy`].

use super::{
    arrive_or_advance, drop_off_or_drive_on, journey_finished, Agent, AgentId, Context, Influence,
    State, TransitionReason, SAME_PLACE_KM,
};
use crate::policy::{AssignContext, IdleTaxi, WaitingRider, NEIGHBOUR_KM};
use crate::routing::Profile;
use crate::scenario::{AgentKind, Service};
use crate::world::{haversine_km, Coord, World};

/// How far another rider's destination may be from the one the taxi is driving to and still share
/// the trip. Unless the taxi chains its drop-offs, the vehicle stops once and everyone walks the
/// remainder, so this is bounded by each rider's own `max_walk_s` as well.
const POOL_DESTINATION_KM: f64 = 1.0;

/// The two sides of an assignment, as the operator sees them, or `None` when there is no pairing
/// to make at all.
///
/// **Event-driven, and that is what keeps a model in the loop affordable.** Two filters over the
/// arena and an early return if either side is empty; if neither is, there is at least one pair to
/// make, so the expensive half - pricing, or a model call - runs exactly on the ticks where demand
/// or idleness actually changed. A per-tick global matrix over the whole fleet is the most
/// expensive thing this engine could do, and it would put a model call on every tick.
///
/// The context carries positions and patience and not arena slots: a decision names a pair by its
/// position in these two lists, so [`crate::policy`] validates against a list it was handed rather
/// than against the arena. `taxis` and `riders` are in arena order, which is arrival order, so the
/// pairing never depends on anything but the two lists.
pub fn assign_context(agents: &[Agent], time_s: f64) -> Option<AssignContext> {
    let idle: Vec<&Agent> = agents.iter().filter(|agent| is_idle(agent)).collect();
    let most_seats = idle.iter().map(|taxi| free_seats(taxi)).max()?;
    // A party larger than every idle vehicle is not a pair anybody could make, and leaving it in
    // would have this recompute every tick until the rider gave up.
    let waiting: Vec<&Agent> = agents
        .iter()
        .filter(|agent| awaits_assignment(agent))
        .filter(|rider| rider.seats <= most_seats)
        .collect();
    if waiting.is_empty() {
        return None;
    }

    let context = AssignContext {
        taxis: idle
            .iter()
            .map(|taxi| IdleTaxi {
                id: taxi.id,
                position: taxi.position(),
                free_seats: free_seats(taxi),
            })
            .collect(),
        riders: waiting
            .iter()
            .map(|rider| WaitingRider {
                neighbours: neighbours(rider, agents),
                id: rider.id,
                position: rider.position(),
                // Where this leg ends, which for a door-to-door rider is its destination.
                destination: rider.target(),
                seats: rider.seats,
                patience_spent: patience_spent(rider, time_s),
            })
            .collect(),
    };
    Some(context)
}

/// How much of its patience an agent has spent, from zero to one. A rider that has waited longer
/// is reached for from further away. On a network trip's later legs that is its transfer patience.
pub fn patience_spent(agent: &Agent, time_s: f64) -> f64 {
    let patience_s = match agent.leg {
        0 => agent.max_wait_s,
        _ => agent.transfer_max_wait_s,
    };
    match patience_s > 0.0 {
        true => ((time_s - agent.state_since_s) / patience_s).clamp(0.0, 1.0),
        false => 1.0,
    }
}

/// Other fleet riders waiting within a hundred metres of `rider` whose current leg ends where its
/// does - assigned or not, because a taxi collecting one of them collects them all.
fn neighbours(rider: &Agent, agents: &[Agent]) -> u32 {
    agents
        .iter()
        .filter(|other| other.id != rider.id && other.kind == AgentKind::AutonomousTaxiRider)
        .filter(|other| other.state == State::WaitingDriver)
        .filter(|other| haversine_km(other.position(), rider.position()) <= NEIGHBOUR_KM)
        .filter(|other| haversine_km(other.target(), rider.target()) <= SAME_PLACE_KM)
        .count() as u32
}

/// A fleet taxi with nothing to do, waiting where it was last released.
pub fn is_idle(agent: &Agent) -> bool {
    agent.kind == AgentKind::AutonomousTaxi
        && agent.state == State::WaitingDeparture
        && agent
            .vehicle
            .as_ref()
            .is_some_and(|vehicle| vehicle.riders.is_empty())
}

/// A fleet rider standing where it is, hailed and not yet assigned to anybody.
pub fn awaits_assignment(agent: &Agent) -> bool {
    agent.kind == AgentKind::AutonomousTaxiRider
        && agent.state == State::WaitingDriver
        && agent.claimed_by.is_none()
}

fn free_seats(taxi: &Agent) -> u32 {
    taxi.vehicle
        .as_ref()
        .map_or(0, |vehicle| vehicle.free_seats())
}

/// A fleet rider: it hails at its departure time, waits where it is standing, is collected there,
/// and covers whatever is left between the drop-off and its own door on foot.
///
/// There is no line, no station and no walk to one, which is the whole of what door-to-door
/// service means. It has no agency once aboard - the taxi's drop-off is what sets it down - and it
/// gives up if nobody reaches it inside `max_wait_s`.
pub fn decide_rider(agent: &Agent, context: &Context) -> Influence {
    match agent.state {
        State::WaitingDeparture => {
            if context.time_s < agent.leave_s() {
                return Influence::Idle;
            }
            // Hailing is the whole of its registration: the operator reads the arena, so there is
            // nothing to put in a registry.
            Influence::Transition {
                state: State::WaitingDriver,
                reason: TransitionReason::RegisteredWithOperator,
            }
        }

        State::WaitingDriver => {
            if context.time_s - agent.state_since_s >= agent.patience_s() {
                Influence::Transition {
                    state: State::Canceled,
                    reason: TransitionReason::WaitTimedOut,
                }
            } else {
                Influence::Idle
            }
        }

        // Aboard: the vehicle supplies the journey and the drop-off ends it.
        State::RideCarpoolToDestination => Influence::Advance,

        // Set down at a station partway along a network trip: hail the next leg from here.
        State::ArrivalDestinationStation if !agent.on_last_leg() => {
            Influence::NextLeg { rider: agent.id }
        }
        // Set down. The last leg is zero-length for the rider the taxi drove to and a short walk
        // for anyone that shared the trip.
        State::ArrivalDestinationStation => Influence::Travel {
            to: agent.destination,
            station: None,
            profile: Profile::Foot,
            state: State::RideToArrival,
            reason: TransitionReason::DroppedOff,
        },

        State::RideToArrival => arrive_or_advance(
            agent,
            State::EndJourney,
            TransitionReason::ArrivedAtDestination,
        ),

        State::Canceled | State::EndJourney => Influence::Idle,

        // A fleet rider walks to no station and drives nothing.
        State::RideToDepartureStation
        | State::ArrivalDepartureStation
        | State::RideCarpoolToDestinationStation
        | State::RideToOwnDestination
        | State::RideToOwnDestinationRegistered => unreachable!(
            "{:?} is not a state a fleet rider reaches, agent {:?}",
            agent.state, agent.id
        ),
    }
}

/// A fleet taxi: idle until the operator assigns it a rider, then out to collect that rider, on to
/// its destination, and idle again wherever it was released.
///
/// The states are the driver machine's own, read door to door instead of station to station: the
/// departure states are the rider's kerb and the destination states are the rider's door, and
/// `station` is `None` throughout because a fleet taxi never visits one.
///
/// It ignores `max_wait_s`. A taxi has no trip of its own, so nothing it could be late for makes
/// it leave, and it is released by `Sim::stand_down_fleet` once there is no demand left rather
/// than by a timer of its own - which is what keeps a fleet run ending when the work is done
/// instead of trailing off for the length of a knob.
pub fn decide_taxi(agent: &Agent, world: &World, context: &Context) -> Influence {
    match agent.state {
        // Idle, wherever it was last released - or on its way to wherever the operator sent it to
        // wait, which is the same state, because a repositioning taxi is still available.
        State::WaitingDeparture => {
            if context.time_s < agent.departure_s {
                return Influence::Idle;
            }
            // An assignment always wins: the vehicle abandons the reposition it was making and
            // goes to collect, from wherever it has got to.
            if let Some(rider) = assigned_rider(agent, context) {
                return Influence::Travel {
                    to: context.agents[rider.0 as usize].position(),
                    station: None,
                    profile: Profile::Road,
                    state: State::RideToDepartureStation,
                    reason: TransitionReason::AssignedByOperator,
                };
            }
            // `station` is where the operator has told it to wait, and it is the whole of the
            // repositioning state: the operator writes it, arriving clears nothing, and an
            // assignment's `station: None` clears it. A fleet taxi visits no station otherwise,
            // so the field was free.
            let Some(station) = agent.station else {
                return Influence::Idle;
            };
            let coord = world.station(station).coord;
            if haversine_km(agent.position(), coord) <= SAME_PLACE_KM {
                return Influence::Idle;
            }
            match journey_finished(agent) || agent.journey.is_none() {
                true => Influence::Travel {
                    to: coord,
                    station: Some(station),
                    profile: Profile::Road,
                    state: State::WaitingDeparture,
                    reason: TransitionReason::Repositioned,
                },
                // Still on the way there, and still idle while it goes.
                false => Influence::Advance,
            }
        }

        State::RideToDepartureStation => arrive_or_advance(
            agent,
            State::ArrivalDepartureStation,
            TransitionReason::ArrivedAtStation,
        ),

        State::ArrivalDepartureStation => {
            let free = free_seats(agent);
            if free > 0 {
                if let Some(rider) = next_pickup(agent, context) {
                    return Influence::Pickup {
                        rider,
                        driver: agent.id,
                    };
                }
            }
            let aboard = agent
                .vehicle
                .as_ref()
                .expect("validation guarantees a fleet taxi declares a vehicle")
                .riders
                .first()
                .copied();
            let Some(first) = aboard else {
                // The rider it came for gave up while it was on its way, and nobody else is here.
                // It stands where it is and waits for the next assignment rather than driving a
                // trip nobody is on.
                return Influence::Transition {
                    state: State::WaitingDeparture,
                    reason: TransitionReason::WaitTimedOut,
                };
            };
            Influence::Travel {
                to: context.agents[first.0 as usize].target(),
                station: None,
                profile: Profile::Road,
                state: State::RideCarpoolToDestinationStation,
                reason: match free {
                    0 => TransitionReason::VehicleFull,
                    _ => TransitionReason::DepartureTimeReached,
                },
            }
        }

        State::RideCarpoolToDestinationStation => {
            drop_off_or_drive_on(agent, world, context.agents)
        }

        // Released where it set everyone down. Where it waits next is the operator's decision,
        // and under a fixed rule that decision is to stand still.
        State::ArrivalDestinationStation => Influence::Transition {
            state: State::WaitingDeparture,
            reason: TransitionReason::DroppedOff,
        },

        State::EndJourney => Influence::Idle,

        // A fleet taxi has no trip of its own to finish, and never rides as a passenger.
        State::WaitingDriver
        | State::RideCarpoolToDestination
        | State::RideToArrival
        | State::RideToOwnDestination
        | State::RideToOwnDestinationRegistered
        | State::Canceled => unreachable!(
            "{:?} is not a state a fleet taxi reaches, agent {:?}",
            agent.state, agent.id
        ),
    }
}

/// The rider this taxi was told to collect, if it is still waiting for it.
fn assigned_rider(agent: &Agent, context: &Context) -> Option<AgentId> {
    context.fleet_assignments[agent.id.0 as usize]
}

/// Every taxi's assigned rider, indexed by the taxi's arena slot: the first waiting fleet rider, in
/// arena order, that the operator paired with it.
pub fn fleet_assignments(agents: &[Agent]) -> Vec<Option<AgentId>> {
    let mut assigned = vec![None; agents.len()];
    for rider in agents {
        if rider.kind == AgentKind::AutonomousTaxiRider && rider.state == State::WaitingDriver {
            if let Some(slot) = rider
                .claimed_by
                .and_then(|taxi| assigned.get_mut(taxi.0 as usize))
            {
                if slot.is_none() {
                    *slot = Some(rider.id);
                }
            }
        }
    }
    assigned
}

/// Who this taxi takes next, of the riders standing where it has stopped.
///
/// The rider it was assigned goes first, so the trip is driven to *that* rider's destination and
/// the pooling is what bends around it rather than the other way round. After that, anyone here
/// whose destination is close enough to share the one drop-off, who nobody else was sent for, and
/// whose own `max_walk_s` covers the walk from that drop-off to its door - the rider's own
/// refusal, which the fleet respects exactly as the line query does.
fn next_pickup(agent: &Agent, context: &Context) -> Option<AgentId> {
    let free = free_seats(agent);
    let here = agent.position();
    let aboard = agent.vehicle.as_ref()?.riders.first().copied();
    let mut candidates: Vec<&Agent> = context
        .agents
        .iter()
        .filter(|rider| rider.kind == AgentKind::AutonomousTaxiRider)
        .filter(|rider| rider.state == State::WaitingDriver && rider.seats <= free)
        // Within `SAME_PLACE_KM` and no further: anything looser would teleport a vehicle to a
        // rider it never drove to.
        .filter(|rider| haversine_km(here, rider.position()) <= SAME_PLACE_KM)
        // A rider whose assignment was lost is still standing at the kerb, and gets in if a taxi
        // stops there for somebody else.
        .filter(|rider| {
            rider.claimed_by.is_none()
                || rider.claimed_by == Some(agent.id)
                || rider.claimed_by == Some(super::NO_TAXI)
        })
        .collect();
    // Its own assignment first, then anyone sharing the trip. Both halves keep arena order.
    candidates.sort_by_key(|rider| rider.claimed_by != Some(agent.id));

    let first = aboard.map(|first| &context.agents[first.0 as usize]);
    let setting_down = first.map(|first| first.target());
    candidates
        .into_iter()
        .find(|rider| match setting_down {
            None => true,
            // A leg over the network is shared by riders taking the same next hop, and only by
            // them: the vehicle is going to one station, and anyone for another is not on its way.
            Some(destination)
                if rider.service == Service::Network
                    || first.is_some_and(|first| first.service == Service::Network) =>
            {
                haversine_km(destination, rider.target()) <= SAME_PLACE_KM
            }
            // Set down at its own door, it walks nothing, so only the radius decides.
            Some(destination) if agent.chained_drop_offs => {
                haversine_km(destination, rider.target()) <= POOL_DESTINATION_KM
            }
            Some(destination) => {
                haversine_km(destination, rider.target()) <= POOL_DESTINATION_KM
                    && context
                        .router
                        .route(destination, rider.target(), Profile::Foot)
                        .duration_s
                        <= rider.max_walk_s
            }
        })
        .map(|rider| rider.id)
}

/// A network fleet rider's legs from `from` to `to`: the station nearest its door, every station
/// along the shortest path over the scenario's lines, then its destination - or no legs at all,
/// and door-to-door service, when the same station is nearest both ends or no path joins them.
///
/// The graph is the lines taken both ways, weighted by the straight line between their stations,
/// and a station on no line is not in it. Dijkstra, with ties broken by station order, so the
/// route never depends on how a queue happened to order equals.
pub fn network_legs(world: &World, from: Coord, to: Coord) -> Vec<Coord> {
    let count = world.stations().len();
    let mut neighbours: Vec<Vec<(usize, f64)>> = vec![Vec::new(); count];
    for line in world.lines() {
        let (a, b) = (line.origin.0 as usize, line.destination.0 as usize);
        let km = haversine_km(world.stations()[a].coord, world.stations()[b].coord);
        for (x, y) in [(a, b), (b, a)] {
            if !neighbours[x].iter().any(|(n, _)| *n == y) {
                neighbours[x].push((y, km));
            }
        }
    }
    let nearest = |point: Coord| {
        (0..count)
            .filter(|&station| !neighbours[station].is_empty())
            .min_by(|&left, &right| {
                haversine_km(point, world.stations()[left].coord)
                    .total_cmp(&haversine_km(point, world.stations()[right].coord))
                    .then(left.cmp(&right))
            })
    };
    let (Some(start), Some(end)) = (nearest(from), nearest(to)) else {
        return Vec::new();
    };
    if start == end {
        return Vec::new();
    }

    let mut distance = vec![f64::INFINITY; count];
    let mut previous: Vec<Option<usize>> = vec![None; count];
    let mut done = vec![false; count];
    distance[start] = 0.0;
    // ponytail: an O(n^2) scan for the nearest open station rather than a heap. Networks are tens
    // of stations; add a binary heap if one ever has thousands.
    while let Some(current) = (0..count)
        .filter(|&station| !done[station] && distance[station].is_finite())
        .min_by(|&left, &right| {
            distance[left]
                .total_cmp(&distance[right])
                .then(left.cmp(&right))
        })
    {
        if current == end {
            break;
        }
        done[current] = true;
        for &(next, km) in &neighbours[current] {
            let through = distance[current] + km;
            if through < distance[next] {
                distance[next] = through;
                previous[next] = Some(current);
            }
        }
    }
    if !distance[end].is_finite() {
        return Vec::new();
    }
    let mut path = vec![end];
    while let Some(back) = previous[*path.last().expect("never empty")] {
        path.push(back);
    }
    path.reverse();
    path.into_iter()
        .map(|station| world.stations()[station].coord)
        .chain(std::iter::once(to))
        .collect()
}
