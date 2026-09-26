//! Network structuring: one station set, five ways of connecting it.
//!
//! The hypothesis this module exists to test is that a *structured* network beats a free-form one
//! for on-demand mobility - fewer loaded vehicle-kilometres, behaviour converging on collective
//! transport. It is allowed to come back negative, and the comparison is a sweep axis rather than
//! a subcommand, so the answer arrives as one set of rows over the same seeds and the same demand.
//!
//! What comes out is a set of station pairs. For every construction but one they are
//! **undirected** and a scenario turns each into two directed lines; [`Construction::FlowDirected`]
//! is the exception and returns them already oriented, one line each. Nothing downstream knows
//! which construction produced them: by the time the world is built, a generated line set and a
//! hand-declared one are the same thing.
//!
//! Only [`Construction::FlowRanked`] and [`Construction::FlowDirected`] read the demand table. The
//! other three are pure geometry, and that asymmetry is the point - they are the constructions that
//! could in principle know something the others cannot, and the ones a language model proposing a
//! line set has to beat.

use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;

use crate::data::Flows;
use crate::scenario::Station;
use crate::world::{haversine_km, Coord, KM_PER_DEGREE_LATITUDE};

/// How a line set is derived from a station set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Construction {
    /// Every station reachable from every other, which is what a door-to-door service is.
    FreeForm,
    /// The triangulation of the station set: local, planar, no long edges.
    Delaunay,
    /// The sparsest connected structure there is.
    MinimumSpanningTree,
    /// Station pairs sorted by the commute between the areas they stand in, highest first, until
    /// every station is connected.
    FlowRanked,
    /// [`Construction::FlowRanked`]'s pairs, each oriented the way the commute between its two
    /// areas actually runs, and taken **one way only**.
    FlowDirected,
}

impl fmt::Display for Construction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Construction::FreeForm => "free-form",
            Construction::Delaunay => "delaunay",
            Construction::MinimumSpanningTree => "minimum-spanning-tree",
            Construction::FlowRanked => "flow-ranked",
            Construction::FlowDirected => "flow-directed",
        };
        f.write_str(name)
    }
}

impl Construction {
    /// Whether this construction reads the demand table. Two do, and they read it differently:
    /// one for how big a flow is, one for which way it runs.
    pub fn reads_demand(self) -> bool {
        matches!(self, Construction::FlowRanked | Construction::FlowDirected)
    }

    /// Whether the pairs come back already oriented, so each is **one** line rather than two.
    ///
    /// Every other construction is symmetric - whatever it connects, it connects both ways - and
    /// on a radial commute that means half of its lines run away from the work. This is the one
    /// bit a construction has to have to avoid that, and it is why it is a flag on the
    /// construction rather than a second field in the scenario.
    pub fn directed(self) -> bool {
        self == Construction::FlowDirected
    }
}

/// The station pairs `construction` connects, lower index first and sorted - except under a
/// [`Construction::directed`] construction, where the tuple is `(origin, destination)` in the
/// order the lines run and the sort is by origin.
///
/// `flows` is required by - and only read by - the two constructions [`Construction::reads_demand`]
/// names.
pub fn pairs(
    stations: &[Station],
    construction: Construction,
    flows: Option<&Flows>,
) -> Result<Vec<(usize, usize)>> {
    if stations.len() < 2 {
        bail!(
            "a {construction} network needs at least two stations, got {}",
            stations.len()
        );
    }
    let coords: Vec<Coord> = stations
        .iter()
        .map(|station| Coord::new(station.latitude, station.longitude))
        .collect();
    Ok(match construction {
        Construction::FreeForm => every_pair(coords.len()),
        Construction::Delaunay => delaunay(&coords),
        Construction::MinimumSpanningTree => minimum_spanning_tree(&coords),
        Construction::FlowRanked => flow_ranked(stations, flows)?,
        Construction::FlowDirected => flow_directed(stations, flows)?,
    })
}

fn every_pair(count: usize) -> Vec<(usize, usize)> {
    (0..count)
        .flat_map(|left| (left + 1..count).map(move |right| (left, right)))
        .collect()
}

/// Prim's algorithm on straight-line distance: exactly `n - 1` edges, and connected.
///
/// ponytail: O(n²) Prim with a linear scan for the nearest outside node rather than a heap.
/// Stations number in the tens. Swap for a heap if a scenario ever has thousands.
fn minimum_spanning_tree(coords: &[Coord]) -> Vec<(usize, usize)> {
    let mut inside = vec![false; coords.len()];
    inside[0] = true;
    let mut edges: Vec<(usize, usize)> = Vec::with_capacity(coords.len() - 1);
    for _ in 1..coords.len() {
        let mut best: Option<(f64, usize, usize)> = None;
        for from in 0..coords.len() {
            if !inside[from] {
                continue;
            }
            for to in 0..coords.len() {
                if inside[to] {
                    continue;
                }
                let distance_km = haversine_km(coords[from], coords[to]);
                // `<` keeps the first of equals, and the scan order is the station order, so a
                // tie resolves the same way on every run.
                if best.is_none_or(|(shortest_km, _, _)| distance_km < shortest_km) {
                    best = Some((distance_km, from, to));
                }
            }
        }
        let (_, from, to) = best.expect("a node outside the tree while one remains");
        inside[to] = true;
        edges.push(ordered(from, to));
    }
    edges.sort_unstable();
    edges
}

/// The Delaunay triangulation's edges: every triple whose circumcircle holds no other station.
///
/// ponytail: brute force over triples rather than incremental Bowyer–Watson. That is O(n⁴) and
/// stations number in the tens, and it is a third of the code with no super-triangle and no
/// degenerate-case bookkeeping. Swap for the incremental construction if a scenario ever has
/// hundreds of stations.
///
/// ponytail: a point exactly on a circumcircle counts as outside it, so four exactly cocircular
/// stations yield both diagonals - a superset of a triangulation rather than one of the two valid
/// ones. Real coordinates are never exactly cocircular; pick a rule if a synthetic scenario ever
/// puts four stations on a circle on purpose.
fn delaunay(coords: &[Coord]) -> Vec<(usize, usize)> {
    let plane = project(coords);
    let count = plane.len();
    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for first in 0..count {
        for second in first + 1..count {
            for third in second + 1..count {
                let Some((centre, radius_squared)) =
                    circumcircle(plane[first], plane[second], plane[third])
                else {
                    // Collinear: no circumcircle, so no triangle.
                    continue;
                };
                let empty = (0..count)
                    .filter(|other| ![first, second, third].contains(other))
                    .all(|other| squared_distance(plane[other], centre) > radius_squared);
                if empty {
                    edges.insert((first, second));
                    edges.insert((first, third));
                    edges.insert((second, third));
                }
            }
        }
    }
    edges.into_iter().collect()
}

/// Station pairs ranked by the commute between the areas they stand in, highest first, taken
/// until every station is connected.
///
/// A pair whose two endpoints are already connected is still taken if its flow ranks above one
/// that would connect something: the rule is "the busiest pairs, until nobody is stranded", which
/// is what makes this denser than a tree where demand is concentrated and no denser where it is
/// even. If the ranking runs out before everything is connected - a station whose area the demand
/// table says nothing about - what comes back is what the flows supported, and the world is then a
/// network with an unreachable station, which is a result rather than an error.
fn flow_ranked(stations: &[Station], flows: Option<&Flows>) -> Result<Vec<(usize, usize)>> {
    let Some(flows) = flows else {
        bail!("a flow-ranked network needs a demand table; set environment.flows");
    };
    let areas = station_areas(stations)?;

    // Both directions of the commute: a line is directed, but which pair of stations to connect
    // is not.
    let commute = |left: &str, right: &str| -> u32 {
        flows
            .rows
            .iter()
            .filter(|row| {
                (row.origin_code == left && row.destination_code == right)
                    || (row.origin_code == right && row.destination_code == left)
            })
            .map(|row| row.total_count)
            .sum()
    };

    let mut ranked: Vec<((usize, usize), u32)> = every_pair(stations.len())
        .into_iter()
        .map(|(left, right)| ((left, right), commute(areas[left], areas[right])))
        .collect();
    // Busiest first, and the station pair itself breaks a tie, so the answer never depends on
    // the sort's own partitioning.
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    let mut components: Vec<usize> = (0..stations.len()).collect();
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (pair, _flow) in ranked {
        if components.iter().all(|label| *label == components[0]) {
            break;
        }
        let (from, to) = (components[pair.0], components[pair.1]);
        for label in components.iter_mut() {
            if *label == to {
                *label = from;
            }
        }
        taken.push(pair);
    }
    taken.sort_unstable();
    Ok(taken)
}

/// [`flow_ranked`]'s pairs, each oriented the way the commute between its two areas runs, and
/// taken **one way only**.
///
/// **The one construction here that reads the demand's direction, and the reason it exists is a
/// measurement.** Every other construction is symmetric, so on a radial morning commute half of
/// its lines run away from the work and the operator's ranking sometimes picks one; six
/// hand-declared one-way lines beat all four of them on every seed wherever vehicles are scarce.
/// This is that asymmetry derived rather than drawn by hand.
///
/// Deliberately **[`flow_ranked`]'s pairs and nothing else**, so the comparison between the two
/// moves exactly one variable. Which stations to connect is already answered; all that changes is
/// that each pair becomes one line instead of two, in the direction the table says the commute
/// actually runs. A construction that also re-picked the pairs would leave a reader unable to say
/// which half of the change paid.
///
/// A pair whose two stations stand in the *same* area has the same flow both ways - the table has
/// one row for it - so nothing prefers a direction and it keeps station order. That is a real case
/// here: three of the corridor's seven stations stand in Ceredigion 001.
fn flow_directed(stations: &[Station], flows: Option<&Flows>) -> Result<Vec<(usize, usize)>> {
    let pairs = flow_ranked(stations, flows)?;
    let flows = flows.expect("flow_ranked has already refused a missing demand table");
    let areas = station_areas(stations)?;
    let mut directed: Vec<(usize, usize)> = pairs
        .into_iter()
        .map(|(left, right)| {
            let out = flow_between(flows, areas[left], areas[right]);
            let back = flow_between(flows, areas[right], areas[left]);
            match out >= back {
                true => (left, right),
                false => (right, left),
            }
        })
        .collect();
    directed.sort_unstable();
    Ok(directed)
}

/// The area each station stands in, which is how a station is priced against a demand table that
/// only knows areas.
fn station_areas(stations: &[Station]) -> Result<Vec<&str>> {
    stations
        .iter()
        .map(|station| match &station.area_code {
            Some(code) => Ok(code.as_str()),
            None => bail!(
                "station {:?} has no area_code, so a construction that reads the demand cannot \
                 price it; the codes are in data/stations.json",
                station.name
            ),
        })
        .collect()
}

/// The commute the table reports from one area to another, in people a day. **Directed**: a line
/// is, and the two directions of a rural commute are rarely the same size - 1,235 people a day
/// travel from Ceredigion 011 into Aberystwyth and 216 the other way.
fn flow_between(flows: &Flows, from: &str, to: &str) -> u32 {
    flows
        .rows
        .iter()
        .filter(|row| row.origin_code == from && row.destination_code == to)
        .map(|row| row.total_count)
        .sum()
}

fn ordered(left: usize, right: usize) -> (usize, usize) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

/// Coordinates as kilometres on a plane centred on the station set. Only relative geometry
/// matters to a circumcircle, and over a corridor of tens of kilometres the equirectangular
/// error is far below the spacing between stations.
fn project(coords: &[Coord]) -> Vec<(f64, f64)> {
    let mean_latitude =
        coords.iter().map(|point| point.latitude).sum::<f64>() / coords.len() as f64;
    let scale = KM_PER_DEGREE_LATITUDE * mean_latitude.to_radians().cos();
    coords
        .iter()
        .map(|point| {
            (
                point.longitude * scale,
                point.latitude * KM_PER_DEGREE_LATITUDE,
            )
        })
        .collect()
}

fn squared_distance(from: (f64, f64), to: (f64, f64)) -> f64 {
    (from.0 - to.0).powi(2) + (from.1 - to.1).powi(2)
}

/// The circle through three points, as its centre and squared radius. `None` for a collinear
/// triple, whose circle has no centre.
fn circumcircle(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> Option<((f64, f64), f64)> {
    let twice_area = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    if twice_area.abs() < 1e-9 {
        return None;
    }
    let (sa, sb, sc) = (
        a.0 * a.0 + a.1 * a.1,
        b.0 * b.0 + b.1 * b.1,
        c.0 * c.0 + c.1 * c.1,
    );
    let centre = (
        (sa * (b.1 - c.1) + sb * (c.1 - a.1) + sc * (a.1 - b.1)) / twice_area,
        (sa * (c.0 - b.0) + sb * (a.0 - c.0) + sc * (b.0 - a.0)) / twice_area,
    );
    Some((centre, squared_distance(a, centre)))
}
