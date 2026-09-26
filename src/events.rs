//! The event log: one row per state transition, written to a file.
//!
//! Nothing in a run goes to stdout. Numbers are written at fixed precision so two runs with the
//! same seed produce byte-identical files.

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::Path;

use crate::agents::{AgentId, State, TransitionReason};
use crate::scenario::AgentKind;
use crate::world::Coord;

/// `seats_used` is the seats taken in the agent's vehicle at that moment, its own party included,
/// and empty for an agent with no vehicle; `distance_km` is how far the agent has travelled so far.
/// Together they are what a time series of occupancy and of loaded and empty kilometres is derived
/// from, so nothing has to be accumulated in the tick loop to draw one.
const COLUMNS: [&str; 10] = [
    "t_s",
    "agent_id",
    "kind",
    "state",
    "reason",
    "latitude",
    "longitude",
    "seats_used",
    "distance_km",
    "detail",
];

pub struct EventLog {
    writer: csv::Writer<Box<dyn Write>>,
    rows: u64,
}

impl EventLog {
    pub fn to_file(path: &Path) -> Result<EventLog> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        EventLog::new(Box::new(file))
    }

    /// A log that keeps the row count but throws the rows away, for runs started without `--out`.
    pub fn discarding() -> Result<EventLog> {
        EventLog::new(Box::new(io::sink()))
    }

    fn new(sink: Box<dyn Write>) -> Result<EventLog> {
        let mut writer = csv::Writer::from_writer(sink);
        writer.write_record(COLUMNS)?;
        Ok(EventLog { writer, rows: 0 })
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        time_s: f64,
        agent: AgentId,
        kind: AgentKind,
        state: State,
        reason: TransitionReason,
        at: Coord,
        seats_used: Option<u32>,
        distance_km: f64,
        detail: &str,
    ) -> Result<()> {
        self.writer.write_record([
            format!("{time_s:.1}"),
            agent.0.to_string(),
            kind.to_string(),
            format!("{state:?}"),
            format!("{reason:?}"),
            format!("{:.6}", at.latitude),
            format!("{:.6}", at.longitude),
            seats_used.map_or(String::new(), |seats| seats.to_string()),
            format!("{distance_km:.4}"),
            detail.to_string(),
        ])?;
        self.rows += 1;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}
