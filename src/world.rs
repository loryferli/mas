//! The static world and the geometry that moves agents through it.
//!
//! Stations and lines live in `Vec`s and are referred to by typed index rather than by reference,
//! so nothing in the simulation needs `Rc<RefCell<_>>` to look at two things at once.
//!
//! A scenario's networks are flattened away here: matching keys on `LineId`, so a grouping of
//! lines under an operator had no reader. `Environment::networks` is still what a scenario
//! declares and what the world is built from - and a constructed line set is written into that
//! same field at load, so nothing here knows whether it was derived or declared by hand. The
//! grouping comes back the day two operators compete for one line.

use serde::{Deserialize, Serialize};

use crate::routing::{Profile, Route, Router};
use crate::scenario::{DepartureGuarantee, Environment};

/// A geographic point, in degrees. Serialised as itself in the committed route cache.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Coord {
    pub latitude: f64,
    pub longitude: f64,
}

impl Coord {
    pub fn new(latitude: f64, longitude: f64) -> Coord {
        Coord {
            latitude,
            longitude,
        }
    }
}

/// Kilometres per degree of latitude. Constant everywhere; longitude is this scaled by the
/// cosine of the latitude.
pub const KM_PER_DEGREE_LATITUDE: f64 = 111.32;

/// Great-circle distance in kilometres.
pub fn haversine_km(from: Coord, to: Coord) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0088;
    let from_latitude = from.latitude.to_radians();
    let to_latitude = to.latitude.to_radians();
    let delta_latitude = to_latitude - from_latitude;
    let delta_longitude = (to.longitude - from.longitude).to_radians();
    let chord = (delta_latitude / 2.0).sin().powi(2)
        + from_latitude.cos() * to_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * chord.sqrt().clamp(-1.0, 1.0).asin()
}

/// Initial bearing from `from` to `to`, in degrees clockwise from north.
///
/// Undefined for two coincident points; callers check the distance first.
pub fn bearing_degrees(from: Coord, to: Coord) -> f64 {
    let from_latitude = from.latitude.to_radians();
    let to_latitude = to.latitude.to_radians();
    let delta_longitude = (to.longitude - from.longitude).to_radians();
    let y = delta_longitude.sin() * to_latitude.cos();
    let x = from_latitude.cos() * to_latitude.sin()
        - from_latitude.sin() * to_latitude.cos() * delta_longitude.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// The smaller of the two angles between two bearings, in degrees, so always `0..=180`.
pub fn angle_between_degrees(left: f64, right: f64) -> f64 {
    let difference = (left - right).abs().rem_euclid(360.0);
    difference.min(360.0 - difference)
}

macro_rules! typed_index {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

typed_index!(StationId);
typed_index!(LineId);

/// A boarding point. Riders walk to one, drivers pick up at one.
#[derive(Debug)]
pub struct Station {
    pub name: String,
    pub coord: Coord,
}

/// A directed origin–destination pair between two stations.
#[derive(Debug)]
pub struct Line {
    pub origin: StationId,
    pub destination: StationId,

    /// The road route from origin to destination: real geometry when the cache has it, the
    /// straight line when it does not.
    pub route: Route,

    pub departure_guarantee: Option<DepartureGuarantee>,
}

#[derive(Debug)]
pub struct World {
    stations: Vec<Station>,
    lines: Vec<Line>,
}

impl World {
    /// Build the world from a validated environment, routing every line through `router`.
    /// Station names have already been checked to be unique and every line endpoint to exist,
    /// so the lookups here cannot fail.
    pub fn from_environment(environment: &Environment, router: &Router) -> World {
        let stations: Vec<Station> = environment
            .stations
            .iter()
            .map(|station| Station {
                name: station.name.clone(),
                coord: Coord::new(station.latitude, station.longitude),
            })
            .collect();

        let station_id = |name: &str| {
            StationId(
                stations
                    .iter()
                    .position(|station| station.name == name)
                    .expect("validation guarantees every line endpoint is a declared station")
                    as u32,
            )
        };

        let mut lines: Vec<Line> = Vec::new();
        for network in &environment.networks {
            for line in &network.lines {
                let origin = station_id(&line.origin);
                let destination = station_id(&line.destination);
                lines.push(Line {
                    origin,
                    destination,
                    route: router.route(
                        stations[origin.index()].coord,
                        stations[destination.index()].coord,
                        Profile::Road,
                    ),
                    departure_guarantee: line.departure_guarantee,
                });
            }
        }

        World { stations, lines }
    }

    pub fn station(&self, id: StationId) -> &Station {
        &self.stations[id.index()]
    }

    pub fn line(&self, id: LineId) -> &Line {
        &self.lines[id.index()]
    }

    pub fn stations(&self) -> &[Station] {
        &self.stations
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }
}

/// Progress along a fixed sequence of points at a fixed speed.
///
/// Each tick consumes `speed_mps * tick_s` metres of the remaining path, walking through whole
/// points and interpolating the final partial segment.
#[derive(Debug, Clone)]
pub struct Journey {
    points: Vec<Coord>,
    /// The point being travelled towards; equal to `points.len()` once the journey is over.
    next: usize,
    position: Coord,
    traveled_km: f64,
    speed_mps: f64,
}

impl Journey {
    /// A journey along `points`, starting at the first of them. A single point is a journey that
    /// is already over.
    pub fn new(points: Vec<Coord>, speed_mps: f64) -> Journey {
        assert!(!points.is_empty(), "a journey needs at least one point");
        assert!(speed_mps > 0.0, "speed must be positive, got {speed_mps}");
        let position = points[0];
        Journey {
            points,
            next: 1,
            position,
            traveled_km: 0.0,
            speed_mps,
        }
    }

    pub fn position(&self) -> Coord {
        self.position
    }

    pub fn traveled_km(&self) -> f64 {
        self.traveled_km
    }

    pub fn finished(&self) -> bool {
        self.next >= self.points.len()
    }

    /// Advance by one tick, returning the distance covered in kilometres.
    pub fn advance(&mut self, tick_s: f64) -> f64 {
        let mut budget_km = self.speed_mps * tick_s / 1000.0;
        let start_km = self.traveled_km;

        while !self.finished() && budget_km > 0.0 {
            let target = self.points[self.next];
            let leg_km = haversine_km(self.position, target);
            if leg_km <= budget_km {
                self.position = target;
                self.next += 1;
                self.traveled_km += leg_km;
                budget_km -= leg_km;
            } else {
                // Linear interpolation in degrees. ponytail: at leg lengths of a few kilometres
                // the error against a great-circle interpolation is well under a metre.
                let fraction = budget_km / leg_km;
                self.position = Coord::new(
                    self.position.latitude + (target.latitude - self.position.latitude) * fraction,
                    self.position.longitude
                        + (target.longitude - self.position.longitude) * fraction,
                );
                self.traveled_km += budget_km;
                budget_km = 0.0;
            }
        }

        let moved_km = self.traveled_km - start_km;
        debug_assert!(moved_km >= 0.0, "distance travelled must never decrease");
        moved_km
    }
}
