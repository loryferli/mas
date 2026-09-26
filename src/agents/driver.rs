//! Driver behaviours.

use anyhow::Result;

use super::{
    arrive_or_advance, claimed_seats, drop_off_or_drive_on, drop_point, group_still_coming,
    may_carry, rider_arrival_s, Agent, AgentId, Context, Influence, State, TransitionReason,
};
use crate::operator::{on_the_way, COINCIDENT_KM};
use crate::policy::{Action, DecisionContext, Policy};
use crate::routing::Profile;
use crate::scenario::{AgentKind, StationApproach};
use crate::world::{angle_between_degrees, bearing_degrees, haversine_km, LineId, World};

/// A neglectful driver signs up and then drives the trip it was making anyway. It registers, so
/// the operator counts a seat and offers it to riders, and it never goes near the station: the
/// vehicle a platform believes it has and no rider ever gets into.
///
/// It keeps its registration for the whole drive rather than dropping it on departure, which is
/// what makes it cost the service something - a rider waiting on that line is waiting partly for
/// this.
pub fn decide_neglectful(agent: &Agent, world: &World, context: &Context) -> Influence {
    match agent.state {
        State::WaitingDeparture => {
            if agent.line.is_none() {
                // Registering is the whole of its participation, so it registers on the line the
                // operator would have given it whatever the detour: it is never going to drive it.
                let quickest = context
                    .operator
                    .ranked_lines(world, context.router, agent.origin, agent.destination)
                    .into_iter()
                    .next();
                return match quickest {
                    Some(matched) => Influence::Register { line: matched.line },
                    None => drive_own_trip(agent, TransitionReason::NoLineAvailable),
                };
            }
            if context.time_s < agent.leave_s() {
                return Influence::Idle;
            }
            drive_own_trip(agent, TransitionReason::DepartureTimeReached)
        }

        // Its own trip, with nobody aboard and no station on the way.
        _ => own_trip_tail(agent),
    }
}

/// The base driver: at its departure time it drives to where it is going with its own party, sets
/// them down there and is done. It registers with nobody and picks up nobody.
///
/// **Every other driver machine is built on this one.** Each of them, once it has no station left
/// to stop at, falls through to [`drive_own_trip`] and [`own_trip_tail`] for the part of the trip
/// that is its own; what they add is everything before that - a line, a station, riders. Spawned on
/// its own it is `PrivateDriver`, the private car a service is read against.
pub fn decide_private(agent: &Agent, context: &Context) -> Influence {
    match agent.state {
        State::WaitingDeparture => match context.time_s < agent.leave_s() {
            true => Influence::Idle,
            false => drive_own_trip(agent, TransitionReason::DepartureTimeReached),
        },
        _ => own_trip_tail(agent),
    }
}

/// Set off for where this agent was going anyway, carrying nobody it picked up. The exit for every
/// driver that ends up with no line to drive, and the last leg of every driver that had one.
fn drive_own_trip(agent: &Agent, reason: TransitionReason) -> Influence {
    Influence::Travel {
        to: agent.destination,
        station: None,
        profile: Profile::Road,
        state: State::RideToOwnDestination,
        reason,
    }
}

/// The tail every driver shares once it is heading for its own destination with no station left to
/// stop at.
fn own_trip_tail(agent: &Agent) -> Influence {
    match agent.state {
        State::RideToOwnDestination => arrive_or_advance(
            agent,
            State::EndJourney,
            TransitionReason::ArrivedAtDestination,
        ),
        State::EndJourney => Influence::Idle,
        other => unreachable!(
            "{other:?} is not on a driver's own trip, agent {:?}",
            agent.id
        ),
    }
}

/// Which line this driver takes, or nothing if none is worth it.
///
/// The choice itself is a [`Policy`] and not a behaviour: it is the dispatcher's answer to "who
/// should drive where", and it is what a study of a low-flow service compares. The driver's part
/// is the refusal - `max_detour_pct`, exactly as `max_walk_s` is the rider's - and the policy is
/// handed it in the context and may not overrule it.
///
/// [`Operator::line_options`] is the one query behind it, and it carries the detour in kilometres
/// and the riders waiting on each line. A driver therefore never reads the ranking's `cost_s`,
/// which prices a rider's walk it is not going to take.
///
/// [`Operator::line_options`]: crate::operator::Operator::line_options
fn choose_line(
    agent: &Agent,
    world: &World,
    context: &Context,
    policy: &mut dyn Policy,
) -> Result<Option<LineId>> {
    let mut options =
        context
            .operator
            .line_options(world, context.router, agent, context.agents, context.pairs);
    if agent.chains > 0 {
        options.retain(|option| chains_forward(agent, world, option.line));
        // Nothing ahead: no question to ask, and a model is not asked it.
        if options.is_empty() {
            return Ok(None);
        }
    }
    let decision = DecisionContext {
        time_s: context.time_s,
        options,
        max_detour_pct: agent.max_detour_pct,
    };
    Ok(match policy.decide(&decision)? {
        Action::TakeLine(line) => Some(line),
        Action::DriveOwnTrip => None,
    })
}

/// A driver the operator knows about. It declares its trip as soon as it spawns, so the operator
/// knows about the seat before the driver has set off; at its departure time it drives to the
/// line's origin station, picks up whoever the operator offers it until the vehicle is full or its
/// patience runs out, drives the line, sets everyone down at the destination station and carries on
/// to where it was going anyway.
///
/// Three kinds run this one machine, and the differences fall out of their configuration rather
/// than out of a branch here. A carpool driver uses the app, so a lead of its own also lets it take
/// riders that have declared ahead and not set off yet - a rider starts walking to this driver's
/// station before either has left, and that is the only thing the lead changes for a driver, which
/// registers as soon as it spawns either way. An opportunistic driver has no app, and validation
/// keeps a declaration lead off a kind without one, so the advance claim below can never fire for
/// it and it is matched only on the spot. A guarantee vehicle is dispatched onto a line it is
/// already registered on, so the line query never runs for it.
///
/// ponytail: no `GetRoute` or `Registration` state. The router answers synchronously and
/// registration is resolved in the same apply pass that receives it, so neither is a state an
/// agent can be observed in - a variant nothing ever rests in is a variant that only shows up in
/// exhaustive matches. Advance declaration did not need them: a claim is resolved in the apply
/// pass that receives it, like a registration.
pub fn decide_registered(
    agent: &Agent,
    world: &World,
    context: &Context,
    mut policy: Option<&mut dyn Policy>,
) -> Result<Influence> {
    // Reborrowed rather than `as_deref_mut`: an `Option` does not let the trait object's lifetime
    // shorten, and the policy is needed again below.
    let reborrowed: Option<&mut dyn Policy> = match policy.as_mut() {
        Some(policy) => Some(&mut **policy),
        None => None,
    };
    if let Some(influence) = on_the_road(agent, world, context, reborrowed)? {
        return Ok(influence);
    }
    Ok(match agent.state {
        // A driver that picks up only on the road and has no line to register on yet: it sets off
        // home at its time, like a private car, and looks out of the window.
        State::WaitingDeparture
            if agent.perception_radius_km.is_some()
                && (agent.kind == AgentKind::GhostDriver || agent.registers_after_first_pickup) =>
        {
            match context.time_s < agent.leave_s() {
                true => Influence::Idle,
                false => drive_own_trip(agent, TransitionReason::DepartureTimeReached),
            }
        }
        State::WaitingDeparture => {
            // The line choice, asked again every tick the driver spends at home rather than once
            // at spawn. It registers on the first of them, so the operator knows about the seat
            // early - a driver's `advanced_declaration_lead_s` does not move that, it decides
            // whether the driver will take riders that have not set off yet - and it re-asks
            // afterwards because demand moves while it waits. `heuristic` reads nothing that
            // moves and answers the same line every tick, so it registers once and every run it
            // produced before is untouched; `greedy` switches lines as riders appear on them.
            //
            // `None` is the guarantee vehicle: called for one line, with no alternative to weigh.
            //
            // A driver that has already claimed a rider is committed and stops reconsidering: the
            // claim is what told that rider which station to walk to, so switching lines now would
            // leave it standing there waiting for a vehicle that is going somewhere else - which
            // is a debug invariant, not a matter of taste. Reconsidering is free only while
            // nothing depends on the choice.
            let promised = claimed_seats(agent.id, context.agents) > 0;
            if let Some(policy) = policy.filter(|_| !promised) {
                match choose_line(agent, world, context, policy)? {
                    Some(line) if agent.line != Some(line) => {
                        return Ok(Influence::Register { line })
                    }
                    // Nothing worth the detour. It drives the trip it was making anyway, which is
                    // what a driver that never signed up does - not vanishing off the road.
                    None if agent.line.is_none() => {
                        let reason = match agent.chains {
                            0 => TransitionReason::DetourRefused,
                            _ => TransitionReason::NoLineAvailable,
                        };
                        return Ok(drive_own_trip(agent, reason));
                    }
                    _ => {}
                }
            }
            debug_assert!(
                agent.line.is_some(),
                "a driver holds the line its policy chose before it sets off"
            );
            // Still at home with a seat to fill: take a rider that declared ahead, so it can
            // start walking to this line's station while the driver waits for its own departure.
            if let Some(claim) = claim_declared_rider(agent, world, context) {
                return Ok(claim);
            }
            if context.time_s < leave_s(agent, world, context) {
                return Ok(Influence::Idle);
            }
            if agent.station_approach == StationApproach::OnDemand {
                return Ok(match rider_needs_stop(agent, world, context) {
                    true => to_station(agent, world, TransitionReason::DepartureTimeReached),
                    false => on_the_way_home(agent, TransitionReason::DepartureTimeReached),
                });
            }
            to_station(agent, world, TransitionReason::DepartureTimeReached)
        }

        // On its own way, still registered: behind it the line is no use to anybody, ahead of it a
        // rider may need it to stop.
        State::RideToOwnDestinationRegistered => {
            if line_behind(agent, world) {
                Influence::Transition {
                    state: State::RideToOwnDestination,
                    reason: TransitionReason::LinePassed,
                }
            } else if rider_needs_stop(agent, world, context) {
                to_station(agent, world, TransitionReason::RiderNeedsStop)
            } else {
                arrive_or_advance(
                    agent,
                    State::EndJourney,
                    TransitionReason::ArrivedAtDestination,
                )
            }
        }

        State::RideToDepartureStation => arrive_or_advance(
            agent,
            State::ArrivalDepartureStation,
            TransitionReason::ArrivedAtStation,
        ),

        State::ArrivalDepartureStation => {
            let vehicle = agent
                .vehicle
                .as_ref()
                .expect("validation guarantees a driver cohort declares a vehicle");
            let free_seats = vehicle.free_seats();

            // Takes whoever is there, one a tick, and leaves the moment nobody else is: no
            // patience to spend, because it only came because somebody needed it.
            if agent.station_approach == StationApproach::OnDemand {
                let taking = context
                    .operator
                    .riders_for(agent, context.agents)
                    .into_iter()
                    .find(|id| context.agents[id.0 as usize].seats <= free_seats)
                    .or_else(|| walk_up(agent, world, context));
                if let (true, Some(rider)) = (free_seats > 0, taking) {
                    return Ok(Influence::Pickup {
                        rider,
                        driver: agent.id,
                    });
                }
                if vehicle.carried() == 0 {
                    return Ok(on_the_way_home(agent, TransitionReason::NobodyAtStation));
                }
                let line = world.line(agent.line.expect("registered before it set off"));
                return Ok(Influence::Travel {
                    to: world.station(line.destination).coord,
                    station: Some(line.destination),
                    profile: Profile::Road,
                    state: State::RideCarpoolToDestinationStation,
                    reason: match free_seats {
                        0 => TransitionReason::VehicleFull,
                        _ => TransitionReason::TookEveryoneWaiting,
                    },
                });
            }

            if free_seats > 0 {
                let taking = context
                    .operator
                    .riders_for(agent, context.agents)
                    .into_iter()
                    .find(|id| context.agents[id.0 as usize].seats <= free_seats)
                    .or_else(|| walk_up(agent, world, context));
                if let Some(rider) = taking {
                    return Ok(Influence::Pickup {
                        rider,
                        driver: agent.id,
                    });
                }

                // Nobody here to take. A rider that declared ahead can still be sent walking,
                // which is the whole difference advance declaration makes to a driver.
                if let Some(claim) = claim_declared_rider(agent, world, context) {
                    return Ok(claim);
                }
            }

            let out_of_patience = context.time_s - agent.state_since_s >= agent.max_wait_s;
            // A pre-formed group's driver is waiting for its own people and nobody else, so it goes
            // the moment the last of them is aboard.
            let group_aboard = agent.kind == AgentKind::PolynomialDriver
                && !group_still_coming(agent, context.agents);
            if free_seats > 0 && !out_of_patience && !group_aboard {
                return Ok(Influence::Idle);
            }
            let line = world.line(agent.line.expect("registered before it set off"));
            Influence::Travel {
                to: world.station(line.destination).coord,
                station: Some(line.destination),
                profile: Profile::Road,
                state: State::RideCarpoolToDestinationStation,
                reason: if free_seats == 0 {
                    TransitionReason::VehicleFull
                } else if group_aboard {
                    TransitionReason::GroupAboard
                } else {
                    TransitionReason::WaitTimedOut
                },
            }
        }

        State::RideCarpoolToDestinationStation => {
            drop_off_or_drive_on(agent, world, context.agents)
        }

        // The line is behind it: the rest is the base driver's own trip - or, chaining, the start of
        // another one from here.
        State::ArrivalDestinationStation => {
            // Set down riders it picked up on the road, somewhere other than its own line's end:
            // an on-demand driver with its line still ahead carries on looking out for it.
            let line_still_ahead = agent
                .line
                .is_some_and(|line| agent.station != Some(world.line(line).destination));
            if agent.rechain {
                Influence::Rechain { driver: agent.id }
            } else if agent.station_approach == StationApproach::OnDemand && line_still_ahead {
                on_the_way_home(agent, TransitionReason::DroppedOff)
            } else {
                drive_own_trip(agent, TransitionReason::DroppedOff)
            }
        }
        State::RideToOwnDestination | State::EndJourney => own_trip_tail(agent),

        // Rider states. A driver never enters one.
        State::WaitingDriver
        | State::RideCarpoolToDestination
        | State::RideToArrival
        | State::Canceled => {
            unreachable!(
                "{:?} is a rider state, reached by driver {:?}",
                agent.state, agent.id
            )
        }
    })
}

/// What a driver that picks up on the road does before anything else this tick, if it does
/// anything: register on its first pickup if it waited to, take a rider standing within its
/// perception radius that the detour allows, or - carrying somebody off its loaded leg - head for
/// that rider's drop point.
fn on_the_road(
    agent: &Agent,
    world: &World,
    context: &Context,
    policy: Option<&mut dyn Policy>,
) -> Result<Option<Influence>> {
    let Some(radius_km) = agent.perception_radius_km else {
        return Ok(None);
    };
    if !matches!(
        agent.state,
        State::RideToOwnDestination
            | State::RideToOwnDestinationRegistered
            | State::RideCarpoolToDestinationStation
    ) {
        return Ok(None);
    }
    let vehicle = agent.vehicle.as_ref().expect("a driver has a vehicle");
    if agent.registers_after_first_pickup && agent.line.is_none() && vehicle.carried() > 0 {
        if let Some(policy) = policy {
            if let Some(line) = choose_line(agent, world, context, policy)? {
                return Ok(Some(Influence::Register { line }));
            }
        }
    }
    let here = agent.position();
    let free_seats = vehicle.free_seats();
    let candidate = context
        .agents
        .iter()
        .filter(|rider| rider.state == State::WaitingDriver && rider.seats <= free_seats)
        .filter(|rider| rider.kind.is_rider() && !rider.kind.is_fleet())
        .filter(|rider| may_carry(agent, rider))
        // A ghost stops for anybody; an opportunistic driver only for riders the operator knows.
        .filter(|rider| agent.kind == AgentKind::GhostDriver || rider.kind.is_registered())
        .filter(|rider| rider.claimed_by.is_none_or(|driver| driver == agent.id))
        .filter(|rider| haversine_km(here, rider.position()) <= radius_km)
        .find(|rider| detour_allows(agent, world, context, rider));
    if let Some(rider) = candidate {
        return Ok(Some(Influence::Pickup {
            rider: rider.id,
            driver: agent.id,
        }));
    }
    let first = vehicle.riders.first();
    if let (Some(first), false) = (first, agent.state == State::RideCarpoolToDestinationStation) {
        let point = drop_point(
            agent,
            &context.agents[first.0 as usize],
            world,
            context.agents,
        );
        return Ok(Some(Influence::Travel {
            to: point.coord(world),
            station: point.station(),
            profile: Profile::Road,
            state: State::RideCarpoolToDestinationStation,
            reason: TransitionReason::PickedUpOnTheRoad,
        }));
    }
    Ok(None)
}

/// The driver's refusal, rider by rider, on routed time: driving on through this rider's drop point
/// against driving straight home, as a percentage. Absent a bound, any rider.
fn detour_allows(agent: &Agent, world: &World, context: &Context, rider: &Agent) -> bool {
    let Some(limit_pct) = agent.max_detour_pct else {
        return true;
    };
    let here = agent.position();
    let via = drop_point(agent, rider, world, context.agents).coord(world);
    let time_s = |from, to| context.router.route(from, to, Profile::Road).duration_s;
    let direct_s = time_s(here, agent.destination);
    if direct_s <= 0.0 {
        return true;
    }
    (time_s(here, via) + time_s(via, agent.destination)) * 100.0 / direct_s <= limit_pct
}

/// Whether a chaining driver may take `line` from where it stands: the line's origin lies ahead of
/// it on the way to the line's destination (no more than 90 degrees off, and no further than that
/// destination - the registration rule a line has to pass), and the line ends nearer the driver's
/// own destination than the driver is now. The second half is what makes every chain end: each
/// link brings it strictly closer to home.
fn chains_forward(agent: &Agent, world: &World, line: LineId) -> bool {
    let line = world.line(line);
    let here = agent.position();
    let origin = world.station(line.origin).coord;
    let destination = world.station(line.destination).coord;
    let to_origin_km = haversine_km(here, origin);
    let ahead = to_origin_km <= COINCIDENT_KM
        || (to_origin_km <= haversine_km(here, destination)
            && angle_between_degrees(
                bearing_degrees(here, destination),
                bearing_degrees(here, origin),
            ) <= 90.0);
    ahead && haversine_km(destination, agent.destination) < haversine_km(here, agent.destination)
}

/// When a registered driver at home sets off for its station: the start of its window, or - timing
/// to its counterpart - so as to arrive when the earliest rider it claimed does, kept inside its
/// own window. Either way its departure noise moves it after.
///
/// The rider's arrival read here is its untimed one, so a driver and a rider both timing to the
/// other do not chase each other: the driver sets its time off where the rider would be, and the
/// rider sets its time off the driver.
pub fn leave_s(agent: &Agent, world: &World, context: &Context) -> f64 {
    if !agent.times_to_counterpart {
        return agent.earliest_leave_s();
    }
    let Some(line) = agent.line else {
        return agent.earliest_leave_s();
    };
    let station = world.station(world.line(line).origin).coord;
    let earliest_arrival_s = context
        .agents
        .iter()
        .filter(|rider| rider.claimed_by == Some(agent.id))
        .filter_map(|rider| rider_arrival_s(rider, station, context))
        .min_by(f64::total_cmp);
    let Some(arrival_s) = earliest_arrival_s else {
        return agent.earliest_leave_s();
    };
    let approach_s = context
        .router
        .route(agent.position(), station, Profile::Road)
        .duration_s;
    (arrival_s - approach_s).clamp(agent.window.earliest_s, agent.window.latest_s)
        + agent.departure_noise_s
}

/// When this driver will be at its line's station, for a rider timing itself to it: its leaving
/// plus the drive there while it is still at home, what is left of the drive once it is on its
/// way, and now once it is there.
pub fn eta_s(agent: &Agent, world: &World, context: &Context) -> Option<f64> {
    let station = world.station(world.line(agent.line?).origin).coord;
    let drive_s = |from| {
        context
            .router
            .route(from, station, Profile::Road)
            .duration_s
    };
    Some(match agent.state {
        State::WaitingDeparture => {
            context.time_s.max(leave_s(agent, world, context)) + drive_s(agent.position())
        }
        State::RideToDepartureStation => context.time_s + drive_s(agent.position()),
        _ => context.time_s,
    })
}

/// Set off for the origin station of the line this driver registered on.
fn to_station(agent: &Agent, world: &World, reason: TransitionReason) -> Influence {
    let station = world
        .line(
            agent
                .line
                .expect("a driver holds its line before it sets off"),
        )
        .origin;
    Influence::Travel {
        to: world.station(station).coord,
        station: Some(station),
        profile: Profile::Road,
        state: State::RideToDepartureStation,
        reason,
    }
}

/// An on-demand driver heading for its own destination, still registered on its line.
fn on_the_way_home(agent: &Agent, reason: TransitionReason) -> Influence {
    Influence::Travel {
        to: agent.destination,
        station: None,
        profile: Profile::Road,
        state: State::RideToOwnDestinationRegistered,
        reason,
    }
}

/// Whether an on-demand driver should turn towards its line's station now: the station lies
/// ahead of it (within 45 degrees of its way and nearer than its destination), and a rider on its
/// line is waiting there, or one it claimed will be by the time the driver arrives.
///
/// The second half is the claimed rider's own honest arrival - its remaining walk, or its whole
/// walk from when it leaves - against the driver's drive there from where it is.
fn rider_needs_stop(agent: &Agent, world: &World, context: &Context) -> bool {
    let Some(line) = agent.line else {
        return false;
    };
    let station = world.station(world.line(line).origin).coord;
    if !on_the_way(agent, station) {
        return false;
    }
    let free_seats = agent
        .vehicle
        .as_ref()
        .map_or(0, |vehicle| vehicle.free_seats());
    let fits = |seats: u32| seats <= free_seats;
    let waiting = context
        .operator
        .riders_for(agent, context.agents)
        .into_iter()
        .any(|id| fits(context.agents[id.0 as usize].seats));
    if waiting {
        return true;
    }
    let arrive_s = context.time_s
        + context
            .router
            .route(agent.position(), station, Profile::Road)
            .duration_s;
    context
        .agents
        .iter()
        .filter(|rider| rider.claimed_by == Some(agent.id) && fits(rider.seats))
        .filter(|rider| rider.state != State::WaitingDriver)
        .filter_map(|rider| rider_arrival_s(rider, station, context))
        .any(|there_s| there_s <= arrive_s)
}

/// Whether the line's origin has fallen behind this driver: more than 90 degrees off the way to
/// the line's destination. Standing on the station itself is not behind it.
fn line_behind(agent: &Agent, world: &World) -> bool {
    let Some(line) = agent.line else {
        return true;
    };
    let line = world.line(line);
    let here = agent.position();
    let origin = world.station(line.origin).coord;
    if haversine_km(here, origin) <= COINCIDENT_KM {
        return false;
    }
    let destination = world.station(line.destination).coord;
    angle_between_degrees(
        bearing_degrees(here, destination),
        bearing_degrees(here, origin),
    ) >= 90.0
}

/// Take a rider that has declared its trip and not set off yet, if this driver declares ahead
/// too, has a seat that is not already promised, and their departure windows overlap.
///
/// Advance matching is mutual: a driver without a lead of its own is matched where it is
/// standing, which is what an operator with no app can offer. The seats already promised are
/// subtracted so a driver does not send five riders walking towards one free seat.
///
/// ponytail: a claim reserves nothing. A driver that runs out of patience before its claimed
/// rider arrives leaves without it, and the rider waits at that station for the next driver on
/// the line. Reserve the seat for a bounded hold if a sweep ever shows riders losing rides they
/// were promised.
fn claim_declared_rider(agent: &Agent, world: &World, context: &Context) -> Option<Influence> {
    if !agent.declares_ahead() && !context.pairs {
        return None;
    }
    let vehicle = agent.vehicle.as_ref()?;
    let free_seats = vehicle
        .free_seats()
        .saturating_sub(claimed_seats(agent.id, context.agents));
    if free_seats == 0 {
        return None;
    }
    let fits = |id: &AgentId| context.agents[id.0 as usize].seats <= free_seats;
    let declared = match agent.declares_ahead() {
        true => context
            .operator
            .declared_riders_for(agent, context.agents)
            .into_iter()
            .find(fits),
        false => None,
    };
    // The pair, if the run is scoring them: the rider still walking whose remaining walk to this
    // driver's line is the shortest, whatever line it registered on. Riders that declared ahead
    // come first because they have not set off at all, so diverting one costs nobody a step.
    let rider = declared.or_else(|| match context.pairs {
        true => context
            .operator
            .divertible_to(world, context.router, agent.line?, context.agents)
            .into_iter()
            .map(|(id, _walk_s)| id)
            .find(fits),
        false => None,
    })?;
    Some(Influence::Claim {
        rider,
        driver: agent.id,
    })
}

/// The vehicle a departure guarantee dispatched. The registered machine drives it, with one rule
/// of its own: a guarantee vehicle has no trip apart from the one it was called for, so if it
/// leaves the station with nobody aboard - the rider that triggered it gave up while it was on its
/// way - it stands down instead of driving the line empty and charging the service thirty-odd
/// kilometres for a ride nobody took.
///
/// The check reads the decision rather than duplicating it, because "leaving the station" is
/// exactly where the shared machine has already weighed the pickup and found nobody. Standing down
/// takes it out of the registry, so the line is free for the next vehicle the trigger calls.
pub fn decide_guarantee(agent: &Agent, world: &World, context: &Context) -> Influence {
    let influence = decide_registered(agent, world, context, None)
        .expect("a guarantee vehicle is handed no policy, so nothing here can fail");
    if agent.state == State::ArrivalDepartureStation
        && matches!(influence, Influence::Travel { .. })
        && agent
            .vehicle
            .as_ref()
            .is_some_and(|vehicle| vehicle.carried() == 0)
    {
        return Influence::Transition {
            state: State::EndJourney,
            reason: TransitionReason::WaitTimedOut,
        };
    }
    influence
}

/// A ghost driver is a driver that never registers. It reads the operator's ranking like anyone
/// else - the ranking is public, and refusing to look at it would be a different kind of agent -
/// takes a line without telling anybody, and picks up whoever happens to be standing at its
/// station. The registration is the only thing it withholds, and the cost of withholding it is that
/// no query the operator answers can ever offer it a rider.
///
/// Everything else is the shared machine, including the detour it will accept.
pub fn decide_ghost(
    agent: &Agent,
    world: &World,
    context: &Context,
    policy: &mut dyn Policy,
) -> Result<Influence> {
    match agent.state {
        State::ArrivalDepartureStation => {
            let vehicle = agent
                .vehicle
                .as_ref()
                .expect("validation guarantees a driver cohort declares a vehicle");
            if vehicle.free_seats() > 0 {
                if let Some(rider) = hitchhiker(agent, world, context) {
                    return Ok(Influence::Pickup {
                        rider,
                        driver: agent.id,
                    });
                }
            }
            // Nothing else differs: it waits its patience out and drives the line either way.
            decide_registered(agent, world, context, Some(policy))
        }

        // Choosing a line, driving it, setting down and going home are the same for every driver.
        // Only the registration and the query differ, and neither is in this machine.
        _ => decide_registered(agent, world, context, Some(policy)),
    }
}

/// A walk-up this carpool driver takes if it takes them at all: an unregistered rider standing at
/// its station, found the way a ghost finds one.
fn walk_up(agent: &Agent, world: &World, context: &Context) -> Option<AgentId> {
    if !agent.takes_unregistered_riders {
        return None;
    }
    standing_here(agent, world, context)
        .find(|rider| !rider.kind.is_registered())
        .map(|rider| rider.id)
}

/// Whoever is standing at this driver's station waiting, bound for the station it is driving to.
///
/// The registry query a registered driver gets does the same job - `Operator::riders_for` filters
/// by line - but a ghost is in no registry, so it looks at who is actually here. It therefore also
/// finds registered riders, which is right: a car stops and you get in, whoever signed up with whom.
///
/// The station match is what keeps the rider going where it asked to go: it chose that line, and
/// its own `max_walk_s` already decided the walk at both ends was acceptable. The driver's side of
/// the bargain is `max_detour_pct`, weighed once when it chose the line rather than here, where a
/// driver would be re-deciding a trip it has already committed to.
fn hitchhiker(agent: &Agent, world: &World, context: &Context) -> Option<AgentId> {
    standing_here(agent, world, context)
        .next()
        .map(|rider| rider.id)
}

/// Riders waiting at this driver's station for the station it drives to, that fit its free seats
/// and that it may carry, in arena order.
fn standing_here<'a>(
    agent: &'a Agent,
    world: &'a World,
    context: &'a Context,
) -> impl Iterator<Item = &'a Agent> + 'a {
    let free_seats = agent
        .vehicle
        .as_ref()
        .map_or(0, |vehicle| vehicle.free_seats());
    let here = agent.station;
    let setting_down = agent.line.map(|line| world.line(line).destination);
    context
        .agents
        .iter()
        .filter(move |_| here.is_some() && setting_down.is_some())
        .filter(|rider| rider.kind.is_rider() && rider.state == State::WaitingDriver)
        .filter(move |rider| may_carry(agent, rider))
        .filter(move |rider| rider.station == here && rider.seats <= free_seats)
        .filter(move |rider| {
            rider
                .line
                .is_some_and(|line| Some(world.line(line).destination) == setting_down)
        })
}
