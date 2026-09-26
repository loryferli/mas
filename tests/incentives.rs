//! What the service pays, and the fact that nothing in a run responds to it yet.
//!
//! The scaffold's whole claim is that switching schemes moves two columns and no others. That is
//! what makes a sweep over any other axis comparable across schemes, and it is what the
//! last test here pins field by field rather than on `passengers_served` alone.

use mas::events::EventLog;
use mas::incentives::{CompletedTrip, IncentiveScheme, NoIncentive, PerPassengerKm};
use mas::metrics::Metrics;
use mas::routing::Router;
use mas::scenario::Scenario;
use mas::sim::Sim;
use std::path::Path;

fn load(name: &str) -> Scenario {
    Scenario::load(Path::new("scenarios").join(name).as_path())
        .unwrap_or_else(|error| panic!("loading {name}: {error:#}"))
}

/// One run, priced under a scheme. The run itself never sees the scheme: it is applied to the
/// finished arena, which is the whole point of the scaffold.
fn priced(name: &str, seed: u64, scheme: &dyn IncentiveScheme) -> (Sim, Metrics) {
    let scenario = load(name);
    let mut log = EventLog::discarding().unwrap();
    let mut sim = Sim::new(&scenario, seed, Router::new(&scenario));
    let summary = sim.run(&mut log).unwrap();
    let metrics = Metrics::collect(sim.agents(), &summary, scheme);
    (sim, metrics)
}

#[test]
fn no_incentive_pays_nothing_for_a_served_trip() {
    let (sim, metrics) = priced("minimal.json", 42, &NoIncentive);
    assert_eq!(metrics.passengers_served, 1, "the trip must have happened");

    let trips = CompletedTrip::collect(sim.agents());
    assert_eq!(trips.len(), 1, "one driver carried somebody");
    assert!(trips[0].passenger_km > 0.0, "and carried them somewhere");
    assert_eq!(NoIncentive.reward(&trips[0]), 0.0);
    assert_eq!(metrics.incentive_paid, 0.0);
    assert_eq!(metrics.cost_per_passenger_served, 0.0);
}

/// Paid by the kilometre people were carried, so the total over every trip is the rate times the
/// run's own `passenger_km`. Not bit-exact: the metric sums over riders and the payment sums over
/// drivers, and floating-point addition does not commute across a regrouping.
#[test]
fn per_passenger_km_pays_the_rate_for_every_carried_kilometre() {
    let rate = 0.10;
    let (sim, metrics) = priced("baseline.json", 42, &PerPassengerKm { rate });
    assert_eq!(metrics.passengers_served, 25);
    assert!(metrics.passenger_km > 0.0);

    let expected = rate * metrics.passenger_km;
    assert!(
        (metrics.incentive_paid - expected).abs() < 1e-9,
        "paid {} for {} passenger-km at {rate}",
        metrics.incentive_paid,
        metrics.passenger_km
    );
    assert!(
        (metrics.cost_per_passenger_served - metrics.incentive_paid / 25.0).abs() < 1e-9,
        "twenty-five people carried, so the cost per passenger is a twenty-fifth of the bill"
    );

    // Empty running earns nothing. Mean occupancy above one means passenger-kilometres can
    // exceed vehicle-kilometres - four people carried ten kilometres is forty passenger-km on
    // ten of road - so the ceiling is the *loaded* kilometres times the seats. A bill above that
    // would be pricing a vehicle with nobody in it.
    let seats = f64::from(
        load("baseline.json").cohorts[0]
            .vehicle
            .as_ref()
            .expect("the driver cohorts declare a vehicle")
            .capacity,
    );
    assert!(
        metrics.incentive_paid <= rate * metrics.vehicle_km_loaded * seats,
        "paid {} against a {} ceiling on {} loaded kilometres over {seats} seats",
        metrics.incentive_paid,
        rate * metrics.vehicle_km_loaded * seats,
        metrics.vehicle_km_loaded
    );

    let trips = CompletedTrip::collect(sim.agents());
    assert_eq!(
        trips.len() as u32,
        metrics.drivers_active,
        "a trip per driver that carried somebody, and no others"
    );
}

/// The scaffold's claim, checked column by column: no driver participates, detours further or
/// picks a different line because of a payout, so switching schemes may move `incentive_paid` and
/// `cost_per_passenger_served` and nothing else. Delete this test the day a behaviour responds to
/// the incentive - that is the day the run stops being comparable across schemes.
#[test]
fn switching_schemes_moves_the_two_incentive_columns_and_no_others() {
    let (_, unpaid) = priced("baseline.json", 42, &NoIncentive);
    let (_, paid) = priced("baseline.json", 42, &PerPassengerKm { rate: 0.25 });

    assert!(
        paid.incentive_paid > 0.0,
        "the paying scheme must have paid"
    );
    assert_eq!(unpaid.incentive_paid, 0.0);
    assert_eq!(unpaid.passengers_served, paid.passengers_served);

    let same = Metrics {
        incentive_paid: unpaid.incentive_paid,
        cost_per_passenger_served: unpaid.cost_per_passenger_served,
        ..paid
    };
    assert_eq!(
        unpaid, same,
        "an incentive nothing responds to changed a run"
    );
}
