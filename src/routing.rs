//! Routing: a committed cache of road geometry, and a straight-line fallback for everything the
//! cache does not cover.
//!
//! Nothing here touches the network during a run. The only code that does is [`build_cache`],
//! which is run by hand from the `build-cache` subcommand and whose output is committed.
//!
//! What the cache covers is every directed station-to-station pair. Agent origins and
//! destinations are jittered per seed, so a walk to the station is a different coordinate on
//! every run and would never hit a cache entry however large the file grew. Those legs take the
//! fallback, which is why the fallback has to be honest about what it approximates: it follows
//! the straight line, so it *understates distance* by the detour factor, and stretches only the
//! duration. Station-to-station legs - the ones that carry passengers, and the ones
//! `vehicle_km_loaded` is computed from - are the cached ones.

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::scenario::Scenario;
use crate::world::{haversine_km, Coord, Journey};

/// How a leg is travelled. The profile picks the speed and the detour factor, and is part of the
/// cache key: the same two points on foot and by road are two different routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Profile {
    Foot,
    Road,
}

/// Walking is close enough to direct that the fallback does not stretch it. The calibration knob
/// for walking is `walk_speed_mps`, which already absorbs the difference.
const FOOT_DETOUR_FACTOR: f64 = 1.0;

/// Cache keys round to four decimal places, about eleven metres, so a coordinate that differs
/// from the cached one only in floating-point noise still hits.
const KEY_SCALE: f64 = 10_000.0;

/// Pause between requests to the public routing server, out of politeness. `build-cache` runs by
/// hand and fetches tens of pairs, so a third of a second each costs nothing.
const FETCH_PAUSE_MS: u64 = 300;

/// A path between two points, and how long it takes to travel.
///
/// `distance_km` is always the sum along `points`, never an independent number, so there is one
/// answer to how far a route is.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub points: Vec<Coord>,
    pub duration_s: f64,
    pub distance_km: f64,
}

impl Route {
    /// A route along `points`, taking `duration_s`.
    pub fn new(points: Vec<Coord>, duration_s: f64) -> Route {
        assert!(!points.is_empty(), "a route needs at least one point");
        let distance_km = points
            .windows(2)
            .map(|pair| haversine_km(pair[0], pair[1]))
            .sum();
        Route {
            points,
            duration_s,
            distance_km,
        }
    }

    /// The pace that covers this route's geometry in its duration. Zero for a route with no
    /// length or no duration.
    fn speed_mps(&self) -> f64 {
        if self.duration_s > 0.0 {
            self.distance_km * 1000.0 / self.duration_s
        } else {
            0.0
        }
    }

    /// The journey that follows this route. A route with no length is a journey that is over
    /// before it starts, so it collapses to its single point rather than dividing by zero.
    pub fn into_journey(self) -> Journey {
        let speed_mps = self.speed_mps();
        if speed_mps > 0.0 {
            Journey::new(self.points, speed_mps)
        } else {
            Journey::new(vec![self.points[0]], 1.0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    from_latitude: i64,
    from_longitude: i64,
    to_latitude: i64,
    to_longitude: i64,
    /// Ordered along with the rest of the key, so the cache file is written in a stable order.
    profile: Profile,
}

impl Key {
    fn new(from: Coord, to: Coord, profile: Profile) -> Key {
        let round = |degrees: f64| (degrees * KEY_SCALE).round() as i64;
        Key {
            from_latitude: round(from.latitude),
            from_longitude: round(from.longitude),
            to_latitude: round(to.latitude),
            to_longitude: round(to.longitude),
            profile,
        }
    }
}

/// Answers "how do I get from here to there", from the cache when it can and from geometry when
/// it cannot. Owns the speeds and the detour factor, so a caller only has to say which profile.
///
/// The speeds are one pair for the whole run, not one per agent: they are what both sides of a
/// match price a line with, and two agents ranking the same lines at different speeds stop
/// agreeing on which station to meet at. [`Router::set_speeds`] is where the run's draw lands.
#[derive(Debug)]
pub struct Router {
    cache: HashMap<Key, Route>,
    walk_speed_mps: f64,
    drive_speed_mps: f64,
    road_detour_factor: f64,
}

impl Router {
    /// A router with an empty cache: every lookup falls back to the straight line.
    ///
    /// The speeds start at the scenario's nominal figures; [`Sim::new`](crate::sim::Sim::new)
    /// replaces them with the run's own draw before anything is routed.
    pub fn new(scenario: &Scenario) -> Router {
        Router {
            cache: HashMap::new(),
            walk_speed_mps: scenario.walk_speed_mps.nominal(),
            drive_speed_mps: scenario.drive_speed_mps.nominal(),
            road_detour_factor: scenario.road_detour_factor,
        }
    }

    /// Load a committed cache file, returning how many routes it held.
    pub fn load_cache(&mut self, path: &Path) -> Result<usize> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading route cache {}", path.display()))?;
        self.load_cache_json(&text)
            .with_context(|| format!("route cache {}", path.display()))
    }

    /// Load a cache from JSON text. Separate from [`Router::load_cache`] so a test does not need
    /// a file on disk.
    pub fn load_cache_json(&mut self, text: &str) -> Result<usize> {
        let file: CacheFile = serde_json::from_str(text).context("parsing route cache")?;
        for entry in file.routes {
            let points: Vec<Coord> = entry
                .points
                .iter()
                .map(|point| Coord::new(point[0], point[1]))
                .collect();
            if points.len() < 2 {
                bail!(
                    "cached route {:?} -> {:?} has fewer than two points",
                    entry.from,
                    entry.to
                );
            }
            self.cache.insert(
                Key::new(entry.from, entry.to, entry.profile),
                Route::new(points, entry.duration_s),
            );
        }
        Ok(self.cache.len())
    }

    /// Replace the reference speeds with the ones drawn for this run.
    ///
    /// The scenario's speeds may be distributions, and the draw belongs to the simulation's own
    /// generator so a run stays a pure function of its seed - which is why this is a setter
    /// rather than an argument to [`Router::new`].
    pub fn set_speeds(&mut self, walk_speed_mps: f64, drive_speed_mps: f64) {
        self.walk_speed_mps = walk_speed_mps;
        self.drive_speed_mps = drive_speed_mps;
    }

    /// The route from `from` to `to`. A cache hit returns the real road geometry; a miss returns
    /// the straight line, taking the detour factor longer than the geometry alone would suggest.
    pub fn route(&self, from: Coord, to: Coord, profile: Profile) -> Route {
        match self.cache.get(&Key::new(from, to, profile)) {
            Some(route) => route.clone(),
            None => self.straight_line(from, to, profile),
        }
    }

    fn straight_line(&self, from: Coord, to: Coord, profile: Profile) -> Route {
        let (speed_mps, detour_factor) = match profile {
            Profile::Foot => (self.walk_speed_mps, FOOT_DETOUR_FACTOR),
            Profile::Road => (self.drive_speed_mps, self.road_detour_factor),
        };
        let direct_km = haversine_km(from, to);
        let duration_s = direct_km * detour_factor * 1000.0 / speed_mps;
        Route::new(vec![from, to], duration_s)
    }
}

// --- the cache file -------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    routes: Vec<CachedRoute>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedRoute {
    profile: Profile,
    from: Coord,
    to: Coord,
    duration_s: f64,
    /// The geometry, as `[latitude, longitude]` pairs. An array rather than a named struct
    /// because a full road route runs to hundreds of points and the file is committed.
    points: Vec<[f64; 2]>,
}

// --- building the cache ---------------------------------------------------------------------

/// Fetch a road route for every directed station pair across `scenarios` and write the cache.
///
/// Returns how many routes were written. Pairs the routing server cannot connect are reported
/// and left out; a run falls back to the straight line for them.
pub fn build_cache(scenarios: &[Scenario], out: &Path) -> Result<usize> {
    let mut wanted: Vec<(String, Coord, Coord)> = Vec::new();
    let mut seen: HashSet<Key> = HashSet::new();
    for scenario in scenarios {
        for from in &scenario.environment.stations {
            for to in &scenario.environment.stations {
                if from.name == to.name {
                    continue;
                }
                let (from_coord, to_coord) = (
                    Coord::new(from.latitude, from.longitude),
                    Coord::new(to.latitude, to.longitude),
                );
                let key = Key::new(from_coord, to_coord, Profile::Road);
                if seen.insert(key) {
                    wanted.push((
                        format!("{} -> {}", from.name, to.name),
                        from_coord,
                        to_coord,
                    ));
                }
            }
        }
    }

    eprintln!("fetching {} station pairs", wanted.len());
    let mut routes: Vec<(Key, CachedRoute)> = Vec::new();
    let mut failed = 0;
    for (label, from, to) in &wanted {
        match fetch_road_route(*from, *to) {
            Ok(route) => {
                eprintln!(
                    "  {label}: {:.1} km, {:.0} s, {} points",
                    route.distance_km,
                    route.duration_s,
                    route.points.len()
                );
                routes.push((
                    Key::new(*from, *to, Profile::Road),
                    CachedRoute {
                        profile: Profile::Road,
                        from: *from,
                        to: *to,
                        duration_s: route.duration_s,
                        points: route
                            .points
                            .iter()
                            .map(|point| [point.latitude, point.longitude])
                            .collect(),
                    },
                ));
            }
            Err(error) => {
                eprintln!("  {label}: {error:#} - leaving it to the straight-line fallback");
                failed += 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(FETCH_PAUSE_MS));
    }

    // Sorted, so rebuilding the cache produces the same file rather than a reshuffled one.
    routes.sort_by_key(|(key, _)| *key);
    let file = CacheFile {
        routes: routes.into_iter().map(|(_, route)| route).collect(),
    };
    let written = file.routes.len();

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // One compact line per route: a full road route runs to a thousand points, so pretty-printing
    // the whole file costs half a megabyte and a diff nobody can read.
    let mut text = String::from("{\n  \"routes\": [\n");
    for (index, route) in file.routes.iter().enumerate() {
        text.push_str("    ");
        text.push_str(&serde_json::to_string(route)?);
        text.push_str(if index + 1 == file.routes.len() {
            "\n"
        } else {
            ",\n"
        });
    }
    text.push_str("  ]\n}\n");
    std::fs::write(out, text).with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "wrote {written} routes to {} ({failed} failed)",
        out.display()
    );
    Ok(written)
}

/// Fetch one road route from the public Open Source Routing Machine demo server.
///
/// ponytail: shells out to `curl` rather than adding an HTTP client crate. This runs by hand,
/// once, and its output is committed; nothing in a run reaches the network. Swap in a real
/// client if the cache builder ever needs retries or authentication.
fn fetch_road_route(from: Coord, to: Coord) -> Result<Route> {
    let url = format!(
        "https://router.project-osrm.org/route/v1/driving/\
         {:.6},{:.6};{:.6},{:.6}?overview=full&geometries=geojson",
        from.longitude, from.latitude, to.longitude, to.latitude
    );
    let output = std::process::Command::new("curl")
        .args(["-sS", "--fail", "--max-time", "30", &url])
        .output()
        .context("running curl; is it installed?")?;
    if !output.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let response: RoutingResponse =
        serde_json::from_slice(&output.stdout).context("parsing the routing response")?;
    if response.code != "Ok" {
        bail!("routing server said {:?}", response.code);
    }
    let route = response
        .routes
        .into_iter()
        .next()
        .context("routing server returned no route")?;
    if route.geometry.coordinates.len() < 2 {
        bail!("routing server returned a route with fewer than two points");
    }

    Ok(Route::new(
        route
            .geometry
            .coordinates
            .iter()
            // The service returns longitude first; everything here is latitude first.
            .map(|point| Coord::new(point[1], point[0]))
            .collect(),
        route.duration,
    ))
}

#[derive(Deserialize)]
struct RoutingResponse {
    code: String,
    #[serde(default)]
    routes: Vec<RoutingRoute>,
}

#[derive(Deserialize)]
struct RoutingRoute {
    duration: f64,
    geometry: RoutingGeometry,
}

#[derive(Deserialize)]
struct RoutingGeometry {
    coordinates: Vec<[f64; 2]>,
}
