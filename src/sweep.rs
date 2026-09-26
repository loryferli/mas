//! Parameter sweeps: one base scenario, a set of axes, a range of seeds, one row per run.
//!
//! An axis point is a set of JSON pointers into the scenario and what to set them to, because
//! the interesting knobs are rarely one field - moving adoption means moving two cohort counts
//! at once, and a lead without its margins is a load error. A pointer that does not resolve
//! stops the sweep: a patch that quietly does nothing runs a different experiment than the one
//! the config describes, and nothing downstream could tell.
//!
//! Runs are independent by construction - a run is a pure function of its scenario, its seed and
//! the route cache - so `rayon` only decides who runs where, never what comes out. Rows are
//! collected in the cross product's own order, so the same config twice writes the same bytes.

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::events::EventLog;
use crate::incentives::IncentiveScheme;
use crate::metrics::Metrics;
use crate::routing::Router;
use crate::scenario::Scenario;
use crate::sim::Sim;

/// The on-disk sweep configuration. JSON rather than TOML so the axis values are the same
/// literals as the scenario fields they patch, and so no parser crate is added for one file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepConfig {
    /// Base scenario, resolved relative to the config file.
    pub scenario: PathBuf,
    pub seeds: Seeds,
    /// Swept in their cross product. No axes means one run per seed.
    #[serde(default)]
    pub axes: Vec<Axis>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seeds {
    pub start: u64,
    pub count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Axis {
    /// The column name in `results.csv`.
    pub name: String,
    pub points: Vec<Point>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// What goes in this axis's column for runs at this point.
    pub value: Json,
    /// JSON pointer into the scenario -> the value to put there.
    pub set: BTreeMap<String, Json>,
}

impl SweepConfig {
    pub fn load(path: &Path) -> Result<SweepConfig> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading sweep config {}", path.display()))?;
        let config: SweepConfig = serde_json::from_str(&text)
            .with_context(|| format!("parsing sweep config {}", path.display()))?;
        if config.seeds.count == 0 {
            bail!("seeds.count must be at least one");
        }
        for axis in &config.axes {
            if axis.points.is_empty() {
                bail!("axis {:?} has no points", axis.name);
            }
        }
        Ok(config)
    }

    /// The base scenario path, resolved against the config file's own directory.
    pub fn scenario_path(&self, config_path: &Path) -> PathBuf {
        match config_path.parent() {
            Some(dir) if self.scenario.is_relative() => dir.join(&self.scenario),
            _ => self.scenario.clone(),
        }
    }

    /// One entry per axis point combination, each a list of one point per axis.
    fn combinations(&self) -> Vec<Vec<&Point>> {
        let mut combinations = vec![Vec::new()];
        for axis in &self.axes {
            combinations = combinations
                .iter()
                .flat_map(|prefix| {
                    axis.points.iter().map(move |point| {
                        let mut combination = prefix.clone();
                        combination.push(point);
                        combination
                    })
                })
                .collect();
        }
        combinations
    }
}

/// Run every combination at every seed and write one tidy row each.
///
/// `scheme` is held fixed across the sweep unless the config makes it an axis, which it cannot
/// yet: no behaviour responds to a payout, so a scheme that moved between rows would move two
/// columns and explain nothing.
pub fn run(
    config_path: &Path,
    out: &Path,
    cache: &Path,
    scheme: &(dyn IncentiveScheme + Sync),
) -> Result<usize> {
    let config = SweepConfig::load(config_path)?;
    let scenario_path = config.scenario_path(config_path);
    let base: Json = serde_json::from_str(
        &std::fs::read_to_string(&scenario_path)
            .with_context(|| format!("reading scenario {}", scenario_path.display()))?,
    )
    .with_context(|| format!("parsing scenario {}", scenario_path.display()))?;

    // Read once and reparse per run: the cache is a plain map, and a `Router` is consumed by the
    // `Sim` that borrows it.
    let cache_text = match cache.exists() {
        true => Some(std::fs::read_to_string(cache)?),
        false => {
            eprintln!(
                "warning: no route cache at {}; every leg falls back to the straight line",
                cache.display()
            );
            None
        }
    };

    let combinations = config.combinations();
    let seeds: Vec<u64> = (0..config.seeds.count)
        .map(|offset| config.seeds.start + offset)
        .collect();
    let jobs: Vec<(&Vec<&Point>, u64)> = combinations
        .iter()
        .flat_map(|combination| seeds.iter().map(move |seed| (combination, *seed)))
        .collect();

    eprintln!(
        "sweep      {} runs: {} combinations x {} seeds",
        jobs.len(),
        combinations.len(),
        seeds.len()
    );

    let done = AtomicUsize::new(0);
    let rows: Vec<Row> = jobs
        .par_iter()
        .map(|(combination, seed)| {
            let row = one_run(&base, combination, *seed, cache_text.as_deref(), scheme);
            let count = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprint!("\r           {count}/{} runs", jobs.len());
            row
        })
        .collect::<Result<Vec<_>>>()?;
    eprintln!();

    let mut writer =
        csv::Writer::from_path(out).with_context(|| format!("writing {}", out.display()))?;
    let mut header: Vec<String> = vec!["scenario".to_string()];
    header.extend(config.axes.iter().map(|axis| axis.name.clone()));
    header.push("seed".to_string());
    // The metric columns come from a run rather than a second list to keep in step with
    // `Metrics`: a column added there reaches the sweep on its own.
    header.extend(rows[0].metrics.keys().cloned());
    writer.write_record(&header)?;
    for row in &rows {
        let mut record = row.head.clone();
        record.extend(row.metrics.values().map(cell));
        writer.write_record(&record)?;
    }
    writer.flush()?;
    Ok(rows.len())
}

/// One run's row, split so the metric columns can name themselves.
struct Row {
    /// Scenario name, the axis values, the seed.
    head: Vec<String>,
    /// Every field of [`Metrics`], keyed by column name.
    metrics: serde_json::Map<String, Json>,
}

fn one_run(
    base: &Json,
    points: &[&Point],
    seed: u64,
    cache_text: Option<&str>,
    scheme: &dyn IncentiveScheme,
) -> Result<Row> {
    let patched = patch(base, points)?;
    let scenario = Scenario::from_json(&patched.to_string())?;

    let mut router = Router::new(&scenario);
    if let Some(text) = cache_text {
        router.load_cache_json(text)?;
    }

    let mut log = EventLog::discarding()?;
    let mut sim = Sim::new(&scenario, seed, router);
    let summary = sim.run(&mut log)?;
    let metrics = Metrics::collect(sim.agents(), &summary, scheme);

    let mut head = vec![scenario.name.clone()];
    head.extend(points.iter().map(|point| cell(&point.value)));
    head.push(seed.to_string());
    match serde_json::to_value(&metrics)? {
        Json::Object(metrics) => Ok(Row { head, metrics }),
        _ => unreachable!("Metrics is a struct"),
    }
}

/// The base scenario with every point's pointers written into it.
///
/// Public so a test can check a committed config's pointers without running its sweep: an axis
/// point whose pointer does not resolve runs a different experiment from the one the config
/// describes, and no column downstream could tell.
pub fn patch(base: &Json, points: &[&Point]) -> Result<Json> {
    let mut scenario = base.clone();
    for point in points {
        for (pointer, value) in &point.set {
            let slot = scenario
                .pointer_mut(pointer)
                .with_context(|| format!("{pointer} does not resolve in the scenario"))?;
            *slot = value.clone();
        }
    }
    Ok(scenario)
}

/// A JSON scalar as a CSV cell: strings unquoted, everything else as written.
fn cell(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}
