//! What the service pays a driver for a trip it carried somebody on.
//!
//! Scaffolding on purpose. A scheme reads a finished trip and returns money, and **nothing in a
//! run reads the reward back**: no driver participates, detours further or picks a different line
//! because of a payout. That behavioural response is the research question rather than plumbing,
//! and it is what would make `PolynomialDriver` mean something. Until it exists, `--incentive`
//! moves `incentive_paid` and `cost_per_passenger_served` and leaves every other metric exactly
//! where it was, which is the property the tests pin.

use crate::agents::{Agent, AgentId};

/// Currency units. Which currency is the scenario's business; the engine only ever adds them up.
pub type Money = f64;

/// One driver's finished trip, as a scheme sees it.
///
/// Per **driver**, not per rider: an incentive is paid to whoever did the driving, and a trip
/// that carried three riders is one payment. Two fields, because two fields is what the schemes
/// that exist read - a trip carries what somebody prices, not everything a trip could be
/// described by.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedTrip {
    pub driver: AgentId,
    /// Kilometres travelled by the people this driver carried, each rider's party counted in
    /// full.
    pub passenger_km: f64,
}

impl CompletedTrip {
    /// One trip per driver that carried at least one rider. A driver that carried nobody is not a
    /// trip any scheme here pays for.
    ///
    /// ponytail: a scan of the arena per carrying driver. Fleets are in the tens; index the riders
    /// by `carried_by` first if a sweep ever makes this show up in a profile.
    pub fn collect(agents: &[Agent]) -> Vec<CompletedTrip> {
        agents
            .iter()
            .filter(|agent| agent.kind.is_driver() && agent.pickups > 0)
            .map(|driver| CompletedTrip {
                driver: driver.id,
                passenger_km: agents
                    .iter()
                    .filter(|rider| rider.carried_by == Some(driver.id))
                    .fold(0.0, |sum, rider| sum + rider.loaded_km * rider.seats as f64),
            })
            .collect()
    }
}

/// What the service pays for a finished trip.
///
/// Two implementations are what justify the trait; the run itself never asks a scheme anything
/// else, and never asks it before a trip is over.
pub trait IncentiveScheme {
    fn reward(&self, trip: &CompletedTrip) -> Money;
}

/// Nobody is paid. The baseline every other scheme is measured against, and the default.
#[derive(Debug, Clone, Copy)]
pub struct NoIncentive;

impl IncentiveScheme for NoIncentive {
    fn reward(&self, _trip: &CompletedTrip) -> Money {
        0.0
    }
}

/// Paid for carrying people, by the kilometre they were carried. Empty running earns nothing, so
/// a driver that drove the line alone is paid the same as one that stayed at home.
#[derive(Debug, Clone, Copy)]
pub struct PerPassengerKm {
    /// Money per passenger-kilometre.
    pub rate: Money,
}

impl IncentiveScheme for PerPassengerKm {
    fn reward(&self, trip: &CompletedTrip) -> Money {
        self.rate * trip.passenger_km
    }
}
