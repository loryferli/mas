//! The study area's committed data: the travel-to-work demand table, and the boarding points.
//!
//! Both files are fetched by `scripts/fetch-data.py`, run by hand, and committed - the same rule
//! as the route cache. Nothing here touches the network; these are readers for files that are
//! already on disk, and `data/README.md` carries the sources, the licences and the exact filters
//! that produced them.
//!
//! The area is an Office for National Statistics middle-layer super output area, about seven
//! thousand people, which is the coarsest grain that still resolves a corridor of a few
//! settlements. `area_code` is its census code and is what a flow row and a station agree on.

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use std::path::Path;

/// One ordered pair of areas, its total commute, and how that commute splits by mode.
///
/// `total_count` is the source's own row total, not a sum computed here, so
/// [`Flow::mode_total`] disagreeing with it means the extract is wrong rather than rounded.
#[derive(Debug, Clone, Deserialize)]
pub struct Flow {
    pub origin_code: String,
    pub origin_name: String,
    pub origin_latitude: f64,
    pub origin_longitude: f64,
    pub destination_code: String,
    pub destination_name: String,
    pub destination_latitude: f64,
    pub destination_longitude: f64,
    pub total_count: u32,
    /// Always zero: the extract asks for area-to-area flows only, so the source's special
    /// workplace codes - mainly at or from home, no fixed place, offshore, outside the United
    /// Kingdom - are not in it. The column is kept so the mode columns still sum to the total
    /// and the exclusion is visible rather than lost.
    pub work_at_home_count: u32,
    pub metro_count: u32,
    pub train_count: u32,
    pub bus_count: u32,
    pub taxi_count: u32,
    pub motorcycle_count: u32,
    pub car_driver_count: u32,
    pub car_passenger_count: u32,
    pub bicycle_count: u32,
    pub on_foot_count: u32,
    pub other_count: u32,
}

impl Flow {
    /// The mode columns added up. Equal to `total_count` on a sound extract.
    pub fn mode_total(&self) -> u32 {
        self.work_at_home_count
            + self.metro_count
            + self.train_count
            + self.bus_count
            + self.taxi_count
            + self.motorcycle_count
            + self.car_driver_count
            + self.car_passenger_count
            + self.bicycle_count
            + self.on_foot_count
            + self.other_count
    }
}

/// The demand table: every ordered pair of areas in the corridor, in the file's own order.
#[derive(Debug, Clone)]
pub struct Flows {
    pub rows: Vec<Flow>,
}

impl Flows {
    pub fn load(path: &Path) -> Result<Flows> {
        let mut reader = csv::Reader::from_path(path)
            .with_context(|| format!("reading demand table {}", path.display()))?;
        let rows = reader
            .deserialize()
            .collect::<std::result::Result<Vec<Flow>, _>>()
            .with_context(|| format!("parsing demand table {}", path.display()))?;
        if rows.is_empty() {
            bail!("demand table {} has no rows", path.display());
        }
        Ok(Flows { rows })
    }

    /// The area codes the table covers, in first-seen order.
    pub fn areas(&self) -> Vec<&str> {
        let mut codes: Vec<&str> = Vec::new();
        for row in &self.rows {
            for code in [row.origin_code.as_str(), row.destination_code.as_str()] {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
        codes
    }
}

/// A boarding point: where a line can start or end, and how it came to be in the set.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardingPoint {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The area it stands in. A station and a flow row agree on this code.
    pub area_code: String,
    /// `rail`, `bus_station`, `park_ride` or `bus_stop`.
    pub kind: String,
    /// Which selection rule put it here, in words, so a reader can see the set was derived and
    /// not hand-picked.
    pub selected_by: String,
    /// The element it came from, as `type/id`, so a figure can be traced back to the map.
    pub openstreetmap: String,
}

/// The committed boarding points, with the corridor they were selected for.
#[derive(Debug, Clone, Deserialize)]
pub struct Stations {
    pub source: String,
    pub licence: String,
    pub attribution: String,
    pub corridor: Vec<String>,
    pub stations: Vec<BoardingPoint>,
}

impl Stations {
    pub fn load(path: &Path) -> Result<Stations> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading boarding points {}", path.display()))?;
        let stations: Stations = serde_json::from_str(&text)
            .with_context(|| format!("parsing boarding points {}", path.display()))?;
        if stations.stations.is_empty() {
            bail!("boarding points {} hold no stations", path.display());
        }
        Ok(stations)
    }

    /// The point of this name, if the set has one.
    pub fn named(&self, name: &str) -> Option<&BoardingPoint> {
        self.stations.iter().find(|point| point.name == name)
    }
}
