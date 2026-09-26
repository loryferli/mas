//! Rider behaviours.

use super::driver::eta_s;
use super::{arrive_or_advance, Agent, Context, Influence, State, TransitionReason};
use crate::routing::Profile;
use crate::scenario::AgentKind;
use crate::world::{LineId, World};

/// A registered or ghost rider: at its departure time it
/// asks the operator which lines serve its trip, takes the quickest one it is willing to walk to,
/// registers on it, walks to that line's origin station and waits there to be picked up. It gives
/// up if nobody comes within `max_wait_s`.
///
/// The walk is the rider's own refusal: the operator ranks the lines and this is where one is
/// accepted or declined. A rider with no line inside its `max_walk_s` never registers.
///
/// Once aboard it has no agency - the driver's drop-off is what sets it down - so the riding
/// states only keep it moving with the vehicle.
///
/// A rider with a declaration lead announces its trip that far ahead of departing, and because it
/// has not set off it can sit in the pool of *every* line inside its `max_walk_s` rather than
/// having to name one station in advance. The rendezvous is then an intersection instead of a
/// value both sides have to compute identically: the match holds as long as the line the driver
/// chose is one the rider would accept, and the rider walks to that one.
pub fn decide_carpool(agent: &Agent, world: &World, context: &Context) -> Influence {
    match agent.state {
        State::WaitingDeparture => {
            if agent.declared_s.is_none() {
                if context.time_s < agent.declaration_s() {
                    return Influence::Idle;
                }
                if agent.kind == AgentKind::PolynomialRider {
                    return group_line(agent, context);
                }
                let mut acceptable = context
                    .operator
                    .ranked_lines(world, context.router, agent.origin, agent.destination)
                    .into_iter()
                    .filter(|matched| matched.walk_s <= agent.max_walk_s);
                // Declaring ahead puts it in every pool it would accept and commits it to none,
                // whether that is one line or five: a rider matched in advance is told which
                // station to walk to, it does not go and stand at one on spec. A rider matched on
                // the spot has to name one, and takes the quickest.
                return if agent.declares_ahead() {
                    let lines: Vec<_> = acceptable.map(|matched| matched.line).collect();
                    if lines.is_empty() {
                        no_line()
                    } else {
                        Influence::Declare { lines }
                    }
                } else {
                    match acceptable.next() {
                        Some(matched) => Influence::Register { line: matched.line },
                        None => no_line(),
                    }
                };
            }
            let Some(line) = agent.line else {
                // Declared, nobody has taken it, and its time has come: it sets off for the line
                // it would have named on the spot.
                if agent.walks_unclaimed && context.time_s >= agent.earliest_leave_s() {
                    return match context
                        .operator
                        .ranked_lines(world, context.router, agent.origin, agent.destination)
                        .into_iter()
                        .find(|matched| matched.walk_s <= agent.max_walk_s)
                    {
                        Some(matched) => Influence::Register { line: matched.line },
                        None => no_line(),
                    };
                }
                // Declared, and nobody has taken it yet. It stays where it is: patience runs
                // from the declaration rather than from a station it may never be sent to.
                return if context.time_s - agent.declaration_s() >= agent.max_wait_s {
                    Influence::Transition {
                        state: State::Canceled,
                        reason: TransitionReason::WaitTimedOut,
                    }
                } else {
                    Influence::Idle
                };
            };
            // The earliest departure it accepts, which without margins is the time it declared -
            // or, timing itself to the driver that claimed it, the moment that gets it to the
            // station as the driver does.
            if context.time_s < leave_s(agent, world, context, line) {
                return Influence::Idle;
            }
            let station = world.line(line).origin;
            Influence::Travel {
                to: world.station(station).coord,
                station: Some(station),
                profile: Profile::Foot,
                state: State::RideToDepartureStation,
                reason: TransitionReason::DepartureTimeReached,
            }
        }

        State::RideToDepartureStation => {
            // Diverted mid-walk: a pair-scoring policy took this rider onto a line other than the
            // one it registered on, so the station it is walking to is no longer the one it is
            // meeting a driver at. Under every other policy a claim cannot land here and the two
            // always agree, so this costs those runs one comparison and changes nothing in them.
            if let Some(station) = agent
                .line
                .map(|line| world.line(line).origin)
                .filter(|origin| agent.station != Some(*origin))
            {
                return Influence::Travel {
                    to: world.station(station).coord,
                    station: Some(station),
                    profile: Profile::Foot,
                    state: State::RideToDepartureStation,
                    reason: TransitionReason::MatchedInAdvance,
                };
            }
            arrive_or_advance(
                agent,
                State::WaitingDriver,
                TransitionReason::ArrivedAtStation,
            )
        }

        State::WaitingDriver => {
            if context.time_s - agent.state_since_s >= agent.max_wait_s {
                Influence::Transition {
                    state: State::Canceled,
                    reason: TransitionReason::WaitTimedOut,
                }
            } else {
                Influence::Idle
            }
        }

        // Aboard: the vehicle supplies the journey and the driver's drop-off ends it.
        State::RideCarpoolToDestination => Influence::Advance,

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

        // Driver states. A rider never enters one.
        State::ArrivalDepartureStation
        | State::RideCarpoolToDestinationStation
        | State::RideToOwnDestination
        | State::RideToOwnDestinationRegistered => {
            unreachable!(
                "{:?} is a driver state, reached by rider {:?}",
                agent.state, agent.id
            )
        }
    }
}

/// A group passenger takes whatever line its driver took, and gives up with it if its driver found
/// none worth driving.
fn group_line(agent: &Agent, context: &Context) -> Influence {
    let driver = context
        .agents
        .iter()
        .find(|driver| driver.kind == AgentKind::PolynomialDriver && driver.group == agent.group);
    match driver {
        Some(driver) => match driver.line {
            Some(line) => Influence::Register { line },
            None if driver.state != State::WaitingDeparture => no_line(),
            None => Influence::Idle,
        },
        // Its driver has not entered the arena yet: they spawn on the same tick, so next tick.
        None => Influence::Idle,
    }
}

/// When a rider with a line sets off: the start of its window, or - timing to its counterpart - so
/// as to reach the station when the driver that claimed it will, kept inside its own window.
fn leave_s(agent: &Agent, world: &World, context: &Context, line: LineId) -> f64 {
    let driver = agent
        .claimed_by
        .filter(|_| agent.times_to_counterpart)
        .map(|driver| &context.agents[driver.0 as usize]);
    let Some(eta_s) = driver.and_then(|driver| eta_s(driver, world, context)) else {
        return agent.earliest_leave_s();
    };
    let station = world.station(world.line(line).origin).coord;
    let walk_s = context
        .router
        .route(agent.origin, station, Profile::Foot)
        .duration_s;
    (eta_s - walk_s).clamp(agent.window.earliest_s, agent.window.latest_s) + agent.departure_noise_s
}

/// The operator had nothing this agent would accept, so it never registers and goes no further.
fn no_line() -> Influence {
    Influence::Transition {
        state: State::Canceled,
        reason: TransitionReason::NoLineAvailable,
    }
}
