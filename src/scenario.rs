//! Scenario definition: the on-disk JSON a run is configured from, plus validation.
//!
//! Unknown fields are rejected rather than ignored, so a typo in a scenario file fails at load
//! instead of silently falling back to a default and quietly changing the experiment.

use crate::data::Flows;
use crate::network::{self, Construction};
use crate::policy::PolicyName;
use anyhow::{bail, Context, Result};
use rand::RngExt;
use rand_chacha::ChaCha8Rng;
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// A knob's value: one number, or a distribution to draw one from per agent.
///
/// A bare number in JSON parses as [`Value::Fixed`], so every scenario written before
/// distributions existed keeps working untouched - and drawing a `Fixed` consumes no randomness,
/// which is what keeps those runs byte-identical rather than merely equivalent.
///
/// A cohort whose knobs are all fixed is one agent repeated. Changing one field to
/// `{"weibull": {"scale": 1800, "shape": 2}}` makes it a population.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Fixed(f64),
    Drawn(Distribution),
}

/// The shapes a [`Value`] can be drawn from, each one closed-form inverse cumulative
/// distribution function.
///
/// ponytail: four expressions rather than the `rand_distr` crate. Add the crate if a shape
/// without a closed-form inverse (gamma, lognormal by rejection) is ever wanted.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Distribution {
    Uniform {
        low: f64,
        high: f64,
    },
    Normal {
        mean: f64,
        sd: f64,
    },
    /// A `shape` above one is a population whose chance of the value landing in the next
    /// interval rises the further it has already gone. For `max_wait_s` that is patience running
    /// out: a spread of give-up times with no per-tick draw, no hazard-rate code and no second
    /// path through the state machine, and `shape` is the knob for how sharply it runs out.
    Weibull {
        scale: f64,
        shape: f64,
    },
    /// One with probability `p`, zero otherwise. What a share of a cohort doing something is: a
    /// car carrying a companion seven times in a hundred is `additional_passengers:
    /// {"bernoulli": {"p": 0.07}}`.
    Bernoulli {
        p: f64,
    },
}

/// A speed floor, in metres per second. A zero speed is a journey that never arrives - a hang
/// rather than an error - so every drawn speed is clamped to at least this.
const MIN_SPEED_MPS: f64 = 0.1;

impl Default for Value {
    fn default() -> Value {
        Value::Fixed(0.0)
    }
}

impl Value {
    /// One draw. `Fixed` does not touch the generator, so giving one knob a distribution leaves
    /// every other knob's draws where they were.
    pub fn draw(&self, rng: &mut ChaCha8Rng) -> f64 {
        match self {
            Value::Fixed(value) => *value,
            Value::Drawn(distribution) => distribution.draw(rng),
        }
    }

    /// A duration or a distance: never negative, whatever the tail of the distribution says. A
    /// normal will eventually hand back a negative number.
    pub fn draw_non_negative(&self, rng: &mut ChaCha8Rng) -> f64 {
        self.draw(rng).max(0.0)
    }

    /// A speed: never below [`MIN_SPEED_MPS`].
    pub fn draw_speed(&self, rng: &mut ChaCha8Rng) -> f64 {
        self.draw(rng).max(MIN_SPEED_MPS)
    }

    /// A count of people: a whole number, never negative.
    pub fn draw_count(&self, rng: &mut ChaCha8Rng) -> u32 {
        self.draw_non_negative(rng).round() as u32
    }

    /// The figure that stands for this value without drawing: the number itself, or the middle of
    /// the distribution. Validation checks this, and the router takes its reference speeds from
    /// it before the simulation draws the run's own.
    pub fn nominal(&self) -> f64 {
        match self {
            Value::Fixed(value) => *value,
            Value::Drawn(Distribution::Uniform { low, high }) => (low + high) / 2.0,
            Value::Drawn(Distribution::Normal { mean, .. }) => *mean,
            // The scale is the 63rd percentile - close enough to the middle for a sanity check.
            Value::Drawn(Distribution::Weibull { scale, .. }) => *scale,
            Value::Drawn(Distribution::Bernoulli { p }) => *p,
        }
    }

    fn validate(&self, label: &str) -> Result<()> {
        match self {
            Value::Fixed(value) if !value.is_finite() => {
                bail!("{label}: {value} is not a finite number")
            }
            Value::Fixed(_) => Ok(()),
            Value::Drawn(distribution) => distribution.validate(label),
        }
    }
}

impl Distribution {
    fn draw(&self, rng: &mut ChaCha8Rng) -> f64 {
        match *self {
            Distribution::Uniform { low, high } => low + (high - low) * unit(rng),

            // Box-Muller. The second deviate of the pair is dropped rather than cached, so one
            // draw is one pair of calls and does not depend on how many normals came before it.
            Distribution::Normal { mean, sd } => {
                let radius = (-2.0 * unit(rng).ln()).sqrt();
                mean + sd * radius * (std::f64::consts::TAU * unit(rng)).cos()
            }

            Distribution::Weibull { scale, shape } => scale * (-unit(rng).ln()).powf(1.0 / shape),

            // `unit` is on (0, 1], so this is one with probability exactly `p`.
            Distribution::Bernoulli { p } => match unit(rng) <= p {
                true => 1.0,
                false => 0.0,
            },
        }
    }

    fn validate(&self, label: &str) -> Result<()> {
        match *self {
            Distribution::Uniform { low, high } => {
                if !low.is_finite() || !high.is_finite() || high < low {
                    bail!("{label}: uniform low {low} and high {high} must be finite, low first");
                }
            }
            Distribution::Normal { mean, sd } => {
                if !mean.is_finite() || !sd.is_finite() || sd < 0.0 {
                    bail!(
                        "{label}: normal mean {mean} and sd {sd} must be finite, sd not negative"
                    );
                }
            }
            Distribution::Weibull { scale, shape } => {
                if !scale.is_finite() || scale < 0.0 {
                    bail!("{label}: weibull scale {scale} must be finite and not negative");
                }
                if !shape.is_finite() || shape <= 0.0 {
                    bail!("{label}: weibull shape {shape} must be finite and positive");
                }
            }
            Distribution::Bernoulli { p } => {
                if !(0.0..=1.0).contains(&p) {
                    bail!("{label}: bernoulli p {p} is a probability, so it must be in [0, 1]");
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Fixed(value) => write!(f, "{value}"),
            Value::Drawn(Distribution::Uniform { low, high }) => {
                write!(f, "uniform({low}, {high})")
            }
            Value::Drawn(Distribution::Normal { mean, sd }) => write!(f, "normal({mean}, {sd})"),
            Value::Drawn(Distribution::Weibull { scale, shape }) => {
                write!(f, "weibull({scale}, {shape})")
            }
            Value::Drawn(Distribution::Bernoulli { p }) => write!(f, "bernoulli({p})"),
        }
    }
}

/// A draw from `(0, 1]`, so its logarithm is finite. `random_range` yields `[0, 1)`, and that
/// zero would put an infinity through both the normal and the Weibull.
fn unit(rng: &mut ChaCha8Rng) -> f64 {
    1.0 - rng.random_range(0.0..1.0f64)
}

/// One complete simulation configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,

    /// Length of one simulation step, in seconds.
    #[serde(default = "default_tick_s")]
    pub tick_s: f64,

    /// Hard stop, in seconds. A run also ends early once every agent has finished, so this is
    /// a safety net rather than the usual termination condition.
    #[serde(default = "default_max_time_s")]
    pub max_time_s: f64,

    /// Walking pace, in metres per second, for agents on foot. A calibration knob: real
    /// walking speed on a peri-urban network is slower than the textbook figure.
    #[serde(default = "default_walk_speed_mps")]
    pub walk_speed_mps: Value,

    /// Driving pace, in metres per second, for road legs the route cache does not cover.
    #[serde(default = "default_drive_speed_mps")]
    pub drive_speed_mps: Value,

    /// How much longer a road leg is than the straight line between its endpoints, applied to
    /// the duration of road legs the route cache has no entry for. A calibration knob: 1.3 is
    /// a reasonable peri-urban figure, and the right value is whatever the local network
    /// measures.
    #[serde(default = "default_road_detour_factor")]
    pub road_detour_factor: f64,

    /// Which line a driver takes, of the ones it would accept, and the operator's three other
    /// decisions besides. A field here rather than only a command-line flag so the sweep can patch
    /// it like any other and put every rule in one table; `--policy` overrides it for a single run.
    #[serde(default)]
    pub policy: PolicyName,

    /// How a model policy reaches the model: a command, as a person would type it, speaking
    /// line-delimited JSON over stdin and stdout. Absent means [`llm::DEFAULT_SIDECAR`].
    ///
    /// A scenario field rather than a flag for the same reason `policy` is one - the sweep patches
    /// it like anything else - and it is what lets a test point the engine at a stub script
    /// instead of at a model. Ignored by `heuristic` and `greedy`, which start no process at all.
    ///
    /// [`llm::DEFAULT_SIDECAR`]: crate::llm::DEFAULT_SIDECAR
    #[serde(default)]
    pub sidecar: Option<String>,

    pub environment: Environment,

    /// Groups of identical agents. Each cohort spawns `count` agents spread over its window.
    pub cohorts: Vec<Cohort>,
}

fn default_tick_s() -> f64 {
    1.0
}

fn default_max_time_s() -> f64 {
    // Four hours: long enough for a peak-period run on a peri-urban corridor.
    14_400.0
}

fn default_walk_speed_mps() -> Value {
    Value::Fixed(1.4)
}

fn default_drive_speed_mps() -> Value {
    // 40 km/h: a mixed peri-urban average, not a motorway cruise.
    Value::Fixed(11.1)
}

fn default_road_detour_factor() -> f64 {
    // Measured against the committed corridor: 38.8 km of road over 30.0 km of straight line.
    1.3
}

fn default_max_wait_s() -> Value {
    // Half an hour at a rural station is already a long time to stand around.
    Value::Fixed(1_800.0)
}

fn default_assigned_wait_factor() -> f64 {
    1.0
}

fn default_max_walk_s() -> Value {
    // Half an hour on foot is the outside of what anyone will walk to reach a line. A calibration
    // knob: the figure that matters is what the local population actually accepts.
    Value::Fixed(1_800.0)
}

/// The static world: where the stations are and which lines connect them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub stations: Vec<Station>,

    /// Lines, declared by hand. Left out when `construction` derives them instead, and a load
    /// error alongside one: two answers to what the network is means one of them is being ignored.
    #[serde(default)]
    pub networks: Vec<Network>,

    /// Derive the lines from the station set instead of declaring them. See
    /// [`crate::network`] for what each construction is. Resolved at load, so by the time
    /// anything downstream sees the scenario a generated line set and a declared one are the same
    /// thing.
    #[serde(default)]
    pub construction: Option<Construction>,

    /// The demand table `flow-ranked` ranks station pairs by, as a path resolved from wherever
    /// the binary is run. Read by that construction and no other.
    ///
    /// A sibling of `construction` rather than a field inside it so a sweep can move the
    /// construction with one pointer and no change to the shape of the base scenario. The other
    /// three constructions are pure geometry and leave it alone, which is the asymmetry the
    /// comparison turns on.
    #[serde(default)]
    pub flows: Option<PathBuf>,
}

/// A boarding point. Riders walk to one, drivers pick up at one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,

    /// The census area this boarding point stands in, as `data/stations.json` records it. Only
    /// `flow-ranked` reads it, and it is required by that construction alone.
    #[serde(default)]
    pub area_code: Option<String>,
}

/// A set of lines run by one operator.
///
/// The operator is also the matching authority for its own lines: riders and drivers register
/// with it, and it decides who is offered to whom.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub name: String,
    pub operator: String,
    pub lines: Vec<Line>,
}

/// A directed origin–destination line between two declared stations.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    /// Name of a station declared in `environment.stations`.
    pub origin: String,
    /// Name of a station declared in `environment.stations`.
    pub destination: String,

    /// Departure guarantee for this line, if the operator offers one.
    #[serde(default)]
    pub departure_guarantee: Option<DepartureGuarantee>,
}

/// The operator's promise that a rider who waits too long still gets a vehicle: once a rider
/// has waited past the trigger, a guarantee vehicle is dispatched to the line's origin.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepartureGuarantee {
    /// Rider wait, in seconds, that fires a guarantee vehicle.
    pub trigger_after_wait_s: f64,

    /// Minimum gap, in seconds, between two guarantee vehicles on the same line.
    pub cooldown_s: f64,

    pub capacity: u32,

    /// Where the guarantee vehicle starts from - a depot, not necessarily a station.
    pub latitude: f64,
    pub longitude: f64,
}

/// A group of identical agents spawned over a window.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cohort {
    pub kind: AgentKind,

    /// Free-text label, carried into the event log so runs stay readable.
    pub name: String,

    pub count: u32,

    pub spawn_window: SpawnWindow,

    pub origin: SpawnArea,

    /// Where the agent is going. Absent for an [`AgentKind::AutonomousTaxi`], which has no trip
    /// of its own - and a load error for anything else, because an agent with nowhere to be is
    /// not a trip anybody is making.
    #[serde(default)]
    pub destination: Option<SpawnArea>,

    /// Seconds between an agent spawning and its nominal departure time.
    #[serde(default)]
    pub departure_offset_s: Value,

    /// How long an agent waits at a station before giving up.
    #[serde(default = "default_max_wait_s")]
    pub max_wait_s: Value,

    /// How long an agent will spend on foot reaching a line and leaving it. A line asking for
    /// more is one the agent declines, so a cohort with a small figure is one that will only use
    /// a line on its doorstep. Drivers ignore it.
    #[serde(default = "default_max_walk_s")]
    pub max_walk_s: Value,

    /// How far ahead of departure this agent declares its trip to the operator. Zero means it
    /// does not declare in advance and is matched only on the spot.
    #[serde(default)]
    pub advanced_declaration_lead_s: f64,

    /// How much earlier than the declared time this agent will accept departing.
    #[serde(default)]
    pub earliness_margin_s: f64,

    /// How much later than the declared time this agent will accept departing.
    #[serde(default)]
    pub lateness_margin_s: f64,

    /// How far off its declared time an agent actually leaves, in seconds: a draw per agent, added
    /// to the declaration lead and floored at zero, so a declared trip leaves
    /// `max(0, lead + draw) - lead` after the time it declared and never before it existed. The
    /// declaration and the departure window stay on the declared time - the operator matches on
    /// what it was told - and only the leaving moves. Zero, the default, leaves on time.
    #[serde(default)]
    pub departure_noise_s: Value,

    /// People travelling with this agent beyond the agent itself, so a pickup consumes
    /// `additional_passengers + 1` seats.
    #[serde(default)]
    pub additional_passengers: Value,

    /// How much longer than driving straight there this driver will accept its trip becoming, as a
    /// percentage: 110 means it declines anything costing more than a tenth extra. Absent means it
    /// accepts any detour, which is what every scenario did before the knob existed.
    ///
    /// This is the driver's own refusal, the counterpart of a rider's `max_walk_s`: the operator
    /// ranks the lines and never refuses, so without this a driver is committed to the best line
    /// however far out of its way that line is. Rejected for riders - a rider does not detour, it
    /// walks.
    #[serde(default)]
    pub max_detour_pct: Option<Value>,

    /// Whether a registered driver goes to its line's station whatever happens (`always`, what
    /// every scenario did before the knob existed), or drives towards its own destination and
    /// stops at the station only if a rider needs it (`on_demand`).
    #[serde(default)]
    pub station_approach: StationApproach,

    /// Whether a carpool driver standing at its station also takes riders who never registered -
    /// walk-ups, as a ghost driver does - rather than only the ones the operator offers it. Off by
    /// default.
    #[serde(default)]
    pub takes_unregistered_riders: bool,

    /// Whether a registered driver, having set its riders down, chooses a line again from where it
    /// stands instead of driving home - and carries again. A chained line has to start ahead of it
    /// and end nearer its own destination than it is, so a chain always ends. Off by default.
    #[serde(default)]
    pub rechain: bool,

    /// Whether a rider that declared ahead and was claimed by nobody sets off anyway at its
    /// departure time, for the first line it would accept, instead of waiting to be claimed. Needs a
    /// declaration lead; off by default.
    #[serde(default)]
    pub walks_unclaimed: bool,

    /// How many passengers travel with each driver of a `PolynomialDriver` cohort: the pre-formed
    /// group, spawned alongside it as `PolynomialRider`s sharing its origin, departure and
    /// destination. Required for that kind and rejected for every other.
    #[serde(default)]
    pub group_size: Option<Value>,

    /// Whether a declaring agent times its leaving to the one it was matched with, instead of
    /// leaving at the start of its own window. A driver leaves so as to reach the station when the
    /// earliest rider it claimed does; a claimed rider leaves so as to reach the station when its
    /// driver will. Either way the leaving stays inside the agent's own departure window. Needs a
    /// declaration lead; off by default.
    #[serde(default)]
    pub times_to_counterpart: bool,

    /// How far from a moving vehicle a waiting rider may stand and be picked up, in kilometres.
    /// Ghost and opportunistic drivers only, and only on their own route: a ghost with it takes no
    /// line and drives home, an opportunistic driver needs `station_approach: on_demand`. A
    /// candidate is refused if carrying it would stretch the drive home past `max_detour_pct`,
    /// measured on routed time through the rider's drop point. Absent, nobody is picked up on the
    /// road.
    #[serde(default)]
    pub perception_radius_km: Option<f64>,

    /// When an opportunistic driver registers with the operator: when it spawns, as every other
    /// registered driver does, or only once it has picked somebody up on the road.
    #[serde(default)]
    pub registers: Registers,

    /// How a fleet rider travels: collected at its door and set down at its own (`door_to_door`,
    /// what every scenario did before the knob existed), or over the scenario's lines
    /// (`network`) - to the station nearest its door, along the shortest path of lines one hop at
    /// a time, and from the station nearest its destination to its door, each leg a separate hail
    /// and every change a wait at a station. Fleet riders only.
    #[serde(default)]
    pub service: Service,

    /// A network fleet rider's patience at every change after the first leg, in seconds. Absent,
    /// the same `max_wait_s` as the first.
    #[serde(default)]
    pub transfer_max_wait_s: Option<Value>,

    /// How much longer a fleet rider will wait once a taxi has been assigned to it: its patience
    /// is multiplied by this from the assignment on. One, the default, changes nothing.
    #[serde(default = "default_assigned_wait_factor")]
    pub assigned_wait_factor: f64,

    /// Whether a fleet taxi sets each rider down at its own door, one after another in pickup
    /// order, instead of everyone at the first rider's door with the rest walking the remainder.
    /// The one-kilometre pooling radius still decides who shares the vehicle. Off by default.
    #[serde(default)]
    pub chained_drop_offs: bool,

    /// Required for drivers, rejected for riders.
    #[serde(default)]
    pub vehicle: Option<Vehicle>,
}

/// How a fleet rider is served.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    #[default]
    DoorToDoor,
    Network,
}

/// When a driver tells the operator it is travelling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Registers {
    #[default]
    AtSpawn,
    /// Drive home unregistered, and register on a line the first time a rider is picked up on the
    /// road - after which the driver behaves as an on-demand registered driver.
    AfterFirstPickup,
}

/// How a registered driver treats the station of the line it registered on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StationApproach {
    /// Drive to the station, wait there up to `max_wait_s` for riders, then drive the line.
    #[default]
    Always,
    /// Drive towards its own destination, still registered. Divert to the station only if a rider
    /// is waiting there or one it claimed will be by the time it arrives - and only while the
    /// station lies ahead: within 45 degrees of its heading and nearer than its destination. Once
    /// the line is behind it (its origin more than 90 degrees off the way to the line's
    /// destination) it unregisters and drives the rest of its trip as a private car. At the
    /// station it takes whoever is there and leaves at once: nobody there, and it carries on.
    OnDemand,
}

/// When a cohort's agents spawn: uniformly over a window, or at a time drawn from a [`Value`].
///
/// The window is what every scenario wrote before the drawn form existed, and it draws exactly
/// what it always drew. The drawn form is for a peak - `{"normal": {"mean": 27000, "sd": 1800}}`
/// is a morning rush - and is clamped to the run, because a normal's tail reaches before zero and
/// past the clock cap.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SpawnWindow {
    Range(SpawnRange),
    Drawn(Value),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnRange {
    pub start_s: f64,
    pub end_s: f64,
}

impl SpawnWindow {
    /// The earliest and latest an agent can spawn, for reporting. A drawn time reports the middle
    /// of its distribution at both ends.
    pub fn bounds_s(&self) -> (f64, f64) {
        match self {
            SpawnWindow::Range(range) => (range.start_s, range.end_s),
            SpawnWindow::Drawn(value) => (value.nominal(), value.nominal()),
        }
    }
}

/// A point with optional uniform random scatter around it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnArea {
    pub latitude: f64,
    pub longitude: f64,

    /// Radius, in kilometres, of the uniform scatter applied per agent. Zero puts every agent
    /// in the cohort at exactly this coordinate.
    #[serde(default)]
    pub jitter_radius_km: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vehicle {
    /// Total seats, including the driver's.
    pub capacity: u32,
}

/// The behaviour types a cohort can spawn.
///
/// Three axes separate them: whether the operator knows about the agent (*registered*),
/// whether it can be matched ahead of time rather than only on the spot (*uses the app*), and
/// whether it is willing to carry anyone (*picks up*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum AgentKind {
    /// The base every driver is built on: drives its own trip, registers with nobody, carries only
    /// its own party. Spawned on its own it is the private car - the trip a carpool service is
    /// trying to replace, and the baseline a service is read against.
    PrivateDriver,
    /// Registered, uses the app, picks up.
    CarpoolDriver,
    /// Not registered, no app, but will pick up at a station opportunistically.
    GhostDriver,
    /// Registered but no app: visible to the operator, matched only on the spot.
    OpportunisticDriver,
    /// Registered, but never carries anyone.
    NeglectfulDriver,
    /// The vehicle the departure guarantee dispatches. Not a cohort kind: the operator calls one
    /// mid-run when a rider has waited past the trigger, so a scenario declaring one is a load
    /// error rather than a fleet.
    TaxiDriver,
    /// The driver of a pre-formed group - people who travel together anyway - registering as
    /// driver and passengers so the group collects the incentive for a trip it would have made
    /// regardless. It picks up its own group at the station and nobody else, and leaves as soon as
    /// all of them are aboard. Every unit of incentive paid for its trip buys no new ride, which is
    /// what the kind is for.
    PolynomialDriver,
    /// A vehicle of the on-demand fleet: no trip of its own, no line, and no choice - the fleet
    /// operator assigns it a rider and it collects that rider at its door.
    ///
    /// Off the three axes the other kinds are separated on: the operator knows it because the
    /// operator *owns* it, and it neither declares ahead nor is matched on a line. Its empty
    /// kilometres are pure overhead rather than a trip somebody was making anyway, which is what
    /// makes the fleet case read differently in every metric.
    AutonomousTaxi,

    /// Registered, uses the app.
    CarpoolRider,
    /// Not registered, no app. Turns up at a station and hopes.
    GhostRider,
    /// A passenger of a pre-formed group. Never a cohort of its own: its driver's cohort spawns it,
    /// and it boards its own group's driver and nobody else's.
    PolynomialRider,
    /// Registered with the fleet operator, and served door to door: collected where it is standing
    /// and set down at its destination, so it walks to no station and takes no line.
    AutonomousTaxiRider,
}

impl AgentKind {
    pub fn is_driver(self) -> bool {
        use AgentKind::*;
        matches!(
            self,
            PrivateDriver
                | CarpoolDriver
                | GhostDriver
                | OpportunisticDriver
                | NeglectfulDriver
                | TaxiDriver
                | PolynomialDriver
                | AutonomousTaxi
        )
    }

    pub fn is_rider(self) -> bool {
        !self.is_driver()
    }

    /// Whether this kind can be matched ahead of time rather than only where it is standing. The
    /// second of the three axes, and what `advanced_declaration_lead_s` needs.
    pub fn uses_app(self) -> bool {
        use AgentKind::*;
        // The fleet kinds are off this axis: advance declaration puts an agent in the pool of a
        // *line*, and door-to-door service has no lines to pool on.
        !matches!(
            self,
            PrivateDriver
                | GhostRider
                | GhostDriver
                | OpportunisticDriver
                | AutonomousTaxi
                | AutonomousTaxiRider
        )
    }

    /// Whether the operator knows this agent exists. The first of the three axes, and the whole of
    /// what makes a ghost a ghost: it picks a line exactly as anyone else does - the operator's
    /// ranking is public - it simply never registers on it, so no query the operator answers can
    /// offer it to anybody. Being found is then down to a driver standing at the same station.
    pub fn is_registered(self) -> bool {
        use AgentKind::*;
        !matches!(self, PrivateDriver | GhostRider | GhostDriver)
    }

    /// Whether this kind's behaviour has been written yet. A scenario may spawn a kind that
    /// has not; those agents do nothing, and the run warns so the result is not mistaken for
    /// a finding.
    pub fn behaviour_implemented(self) -> bool {
        true
    }

    /// Whether this kind belongs to the on-demand fleet, which is served door to door and never
    /// matched onto a line.
    pub fn is_fleet(self) -> bool {
        use AgentKind::*;
        matches!(self, AutonomousTaxi | AutonomousTaxiRider)
    }

    /// Whether this kind needs the world to have a line in it. Everything but the fleet and the
    /// private car does: a rider with no line to be matched onto has no service at all, while a
    /// fleet is served door to door and a private car uses no service, so a world with no lines is
    /// what those scenarios mean.
    pub fn needs_a_line(self) -> bool {
        !self.is_fleet() && self != AgentKind::PrivateDriver
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Scenario {
    /// Read and validate a scenario file.
    pub fn load(path: &Path) -> Result<Scenario> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading scenario {}", path.display()))?;
        Scenario::from_json(&text).with_context(|| format!("scenario {}", path.display()))
    }

    /// Parse and validate a scenario from JSON text.
    pub fn from_json(text: &str) -> Result<Scenario> {
        let mut scenario: Scenario = serde_json::from_str(text).context("parsing scenario")?;
        scenario
            .build_network()
            .context("constructing the scenario's network")?;
        scenario.validate().context("validating scenario")?;
        Ok(scenario)
    }

    /// The command a model policy reaches the model through.
    pub fn sidecar(&self) -> &str {
        self.sidecar
            .as_deref()
            .unwrap_or(crate::llm::DEFAULT_SIDECAR)
    }

    /// Turn `environment.construction` into declared lines, if the scenario asked for one.
    ///
    /// Done at load rather than in [`crate::world::World`] so that **nothing downstream knows
    /// which construction it was given**: by the time anything reads the environment, a derived
    /// line set and a hand-declared one are the same lines. It is also what keeps `build-cache`
    /// unchanged - it collects station pairs, and every construction is a subset of those.
    ///
    /// Each undirected pair becomes two directed lines, in station order, so a construction is
    /// symmetric: whatever it connects, it connects both ways. A
    /// [`Construction::directed`] one is the exception - its pairs arrive already oriented and
    /// become one line each, which is the whole of what that construction changes.
    fn build_network(&mut self) -> Result<()> {
        let Some(construction) = self.environment.construction else {
            return Ok(());
        };
        if !self.environment.networks.is_empty() {
            bail!(
                "the environment declares a {construction} construction and {} network(s); the \
                 construction derives the lines, so declaring them too means one of the two is \
                 being ignored",
                self.environment.networks.len()
            );
        }
        // Read only by the two constructions that price a pair against the demand. The other
        // three are pure geometry, and that asymmetry is the whole point of the comparison.
        let flows = match construction.reads_demand() {
            true => {
                let path =
                    self.environment.flows.as_ref().with_context(|| {
                        format!("a {construction} network needs environment.flows")
                    })?;
                Some(Flows::load(path)?)
            }
            false => None,
        };
        let pairs = network::pairs(&self.environment.stations, construction, flows.as_ref())?;
        let one_way = construction.directed();
        let lines = pairs
            .into_iter()
            .flat_map(|(left, right)| {
                let (from, to) = (
                    self.environment.stations[left].name.clone(),
                    self.environment.stations[right].name.clone(),
                );
                let forward = Line {
                    origin: from.clone(),
                    destination: to.clone(),
                    departure_guarantee: None,
                };
                let back = Line {
                    origin: to,
                    destination: from,
                    departure_guarantee: None,
                };
                match one_way {
                    true => vec![forward],
                    false => vec![forward, back],
                }
            })
            .collect();
        self.environment.networks.push(Network {
            name: construction.to_string(),
            operator: "constructed".to_string(),
            lines,
        });
        Ok(())
    }

    /// Total agents this scenario will spawn.
    pub fn total_agents(&self) -> u32 {
        self.cohorts.iter().map(|c| c.count).sum()
    }

    /// Kinds present in the scenario whose behaviour is not implemented yet.
    pub fn unimplemented_kinds(&self) -> Vec<AgentKind> {
        let mut seen: Vec<AgentKind> = Vec::new();
        for cohort in &self.cohorts {
            if cohort.count > 0
                && !cohort.kind.behaviour_implemented()
                && !seen.contains(&cohort.kind)
            {
                seen.push(cohort.kind);
            }
        }
        seen
    }

    /// Reject anything the simulation could not run, naming the offender.
    fn validate(&self) -> Result<()> {
        if self.tick_s <= 0.0 {
            bail!("tick_s must be positive, got {}", self.tick_s);
        }
        if self.max_time_s <= 0.0 {
            bail!("max_time_s must be positive, got {}", self.max_time_s);
        }
        for (speed, field) in [
            (&self.walk_speed_mps, "walk_speed_mps"),
            (&self.drive_speed_mps, "drive_speed_mps"),
        ] {
            speed.validate(field)?;
            // The nominal figure, because a distribution's tail is clamped at the draw rather
            // than rejected here: a normal reaching zero is a slow walker, not a bad scenario.
            if speed.nominal() <= 0.0 {
                bail!("{field} must be positive and finite, got {speed}");
            }
        }
        if !self.road_detour_factor.is_finite() || self.road_detour_factor < 1.0 {
            bail!(
                "road_detour_factor must be at least 1.0 and finite, got {}",
                self.road_detour_factor
            );
        }
        // A world of private cars or a door-to-door fleet needs no boarding point at all.
        let needs_stations = self
            .cohorts
            .iter()
            .any(|cohort| cohort.kind.needs_a_line() || cohort.service == Service::Network);
        if self.environment.stations.is_empty() && needs_stations {
            bail!("no stations declared");
        }

        let mut station_names: HashSet<&str> = HashSet::new();
        for station in &self.environment.stations {
            check_coordinate(station.latitude, station.longitude, &station.name)?;
            if !station_names.insert(station.name.as_str()) {
                bail!("duplicate station name {:?}", station.name);
            }
        }

        let mut line_count = 0usize;
        for network in &self.environment.networks {
            for line in &network.lines {
                line_count += 1;
                for endpoint in [&line.origin, &line.destination] {
                    if !station_names.contains(endpoint.as_str()) {
                        bail!(
                            "line {:?} -> {:?} on network {:?} references undeclared station {:?}",
                            line.origin,
                            line.destination,
                            network.name,
                            endpoint
                        );
                    }
                }
                if line.origin == line.destination {
                    bail!(
                        "line on network {:?} has the same origin and destination {:?}",
                        network.name,
                        line.origin
                    );
                }
                if let Some(guarantee) = &line.departure_guarantee {
                    let label = format!("departure guarantee on {:?}", line.origin);
                    check_coordinate(guarantee.latitude, guarantee.longitude, &label)?;
                    if guarantee.trigger_after_wait_s < 0.0 {
                        bail!("{label}: trigger_after_wait_s must not be negative");
                    }
                    if guarantee.cooldown_s < 0.0 {
                        bail!("{label}: cooldown_s must not be negative");
                    }
                    // The vehicle's own driver takes one of them, so a capacity of one is a
                    // guarantee that can carry nobody - a figure that means something other than
                    // what it says.
                    if guarantee.capacity < 2 {
                        bail!("{label}: capacity must be at least 2, one seat being the driver's");
                    }
                }
            }
        }
        // A fleet scenario is served door to door, so a world with no lines is what it means.
        // Anything else with no lines is a scenario whose riders have nothing to be matched onto.
        if line_count == 0 && self.cohorts.iter().any(|cohort| cohort.kind.needs_a_line()) {
            bail!("no lines declared; riders would have nothing to be matched onto");
        }
        if line_count == 0
            && self
                .cohorts
                .iter()
                .any(|cohort| cohort.service == Service::Network)
        {
            bail!("no lines declared; a network fleet rider travels over them");
        }

        for cohort in &self.cohorts {
            cohort.validate()?;
        }

        Ok(())
    }
}

impl Cohort {
    fn validate(&self) -> Result<()> {
        let label = &self.name;

        if self.kind == AgentKind::TaxiDriver {
            bail!(
                "cohort {label:?}: a departure guarantee dispatches {} mid-run, so no cohort \
                 spawns one",
                self.kind
            );
        }
        match &self.spawn_window {
            SpawnWindow::Range(range) => {
                if range.start_s < 0.0 {
                    bail!("cohort {label:?}: spawn_window.start_s must not be negative");
                }
                if range.end_s < range.start_s {
                    bail!(
                        "cohort {label:?}: spawn_window ends ({}) before it starts ({})",
                        range.end_s,
                        range.start_s
                    );
                }
            }
            SpawnWindow::Drawn(value) => {
                value.validate(&format!("cohort {label:?}: spawn_window"))?;
                if value.nominal() < 0.0 {
                    bail!("cohort {label:?}: spawn_window must not be centred before zero");
                }
            }
        }

        // A fleet taxi has no trip of its own, which is the whole of what makes its empty
        // kilometres a cost rather than a trip somebody was making anyway. Anything else needs
        // somewhere to be.
        match (self.kind, &self.destination) {
            (AgentKind::AutonomousTaxi, Some(_)) => bail!(
                "cohort {label:?}: {} has no trip of its own, so it declares no destination",
                self.kind
            ),
            (kind, None) if kind != AgentKind::AutonomousTaxi => {
                bail!("cohort {label:?}: {kind} needs a destination")
            }
            _ => {}
        }

        for (point, which) in [
            (Some(&self.origin), "origin"),
            (self.destination.as_ref(), "destination"),
        ] {
            let Some(point) = point else { continue };
            check_coordinate(point.latitude, point.longitude, &format!("{label} {which}"))?;
            if point.jitter_radius_km < 0.0 {
                bail!("cohort {label:?}: {which} jitter_radius_km must not be negative");
            }
        }

        for (value, field) in [
            (
                self.advanced_declaration_lead_s,
                "advanced_declaration_lead_s",
            ),
            (self.earliness_margin_s, "earliness_margin_s"),
            (self.lateness_margin_s, "lateness_margin_s"),
        ] {
            if value < 0.0 {
                bail!("cohort {label:?}: {field} must not be negative, got {value}");
            }
        }

        // A knob that would be silently ignored is a scenario that does not mean what it says.
        // The margins only ever bound an advance match, and only a kind with the app can ask for
        // one.
        if self.advanced_declaration_lead_s > 0.0 && !self.kind.uses_app() {
            bail!(
                "cohort {label:?}: {} does not use the app, so it cannot declare ahead",
                self.kind
            );
        }
        if self.advanced_declaration_lead_s == 0.0
            && (self.earliness_margin_s > 0.0 || self.lateness_margin_s > 0.0)
        {
            bail!(
                "cohort {label:?}: earliness and lateness margins bound an advance match, so they \
                 need advanced_declaration_lead_s above zero"
            );
        }

        for (value, field) in [
            (&self.departure_offset_s, "departure_offset_s"),
            (&self.max_wait_s, "max_wait_s"),
            (&self.max_walk_s, "max_walk_s"),
            (&self.additional_passengers, "additional_passengers"),
        ] {
            value.validate(&format!("cohort {label:?}: {field}"))?;
            if value.nominal() < 0.0 {
                bail!("cohort {label:?}: {field} must not be negative, got {value}");
            }
        }

        // Noise may be negative - leaving early is the point of a lead - so only its shape is
        // checked. A fleet taxi has no trip of its own to be late for.
        self.departure_noise_s
            .validate(&format!("cohort {label:?}: departure_noise_s"))?;
        if self.kind == AgentKind::AutonomousTaxi && self.departure_noise_s != Value::Fixed(0.0) {
            bail!(
                "cohort {label:?}: {} has no trip of its own, so departure_noise_s means nothing \
                 to it",
                self.kind
            );
        }

        // The percentage is of the direct trip, so anything under a hundred asks for a detour
        // shorter than driving straight there.
        if let Some(detour) = &self.max_detour_pct {
            detour.validate(&format!("cohort {label:?}: max_detour_pct"))?;
            if detour.nominal() < 100.0 {
                bail!(
                    "cohort {label:?}: max_detour_pct is a percentage of the direct trip, so it \
                     must be at least 100, got {detour}"
                );
            }
            if self.kind.is_rider() {
                bail!(
                    "cohort {label:?}: {} is a rider, and a rider walks rather than detours, so \
                     max_detour_pct means nothing to it",
                    self.kind
                );
            }
            if self.kind == AgentKind::AutonomousTaxi {
                bail!(
                    "cohort {label:?}: {} has no trip of its own to detour from, so \
                     max_detour_pct means nothing to it",
                    self.kind
                );
            }
            if self.kind == AgentKind::PrivateDriver {
                bail!(
                    "cohort {label:?}: {} takes no line, so it is never offered a detour to \
                     refuse",
                    self.kind
                );
            }
        }

        // Only a driver the operator knows about has a line's station to approach.
        if self.station_approach == StationApproach::OnDemand
            && !matches!(
                self.kind,
                AgentKind::CarpoolDriver | AgentKind::OpportunisticDriver
            )
        {
            bail!(
                "cohort {label:?}: station_approach is how a registered driver treats its line's \
                 station, and {} is not one",
                self.kind
            );
        }

        match (self.kind, &self.group_size) {
            (AgentKind::PolynomialRider, _) => bail!(
                "cohort {label:?}: a {} is spawned with its group's driver, so it is never a \
                 cohort of its own - give the PolynomialDriver cohort a group_size",
                self.kind
            ),
            (AgentKind::PolynomialDriver, None) => bail!(
                "cohort {label:?}: a PolynomialDriver travels with its group, so it needs a \
                 group_size"
            ),
            (AgentKind::PolynomialDriver, Some(size)) => {
                size.validate(&format!("cohort {label:?}: group_size"))?;
                if size.nominal() < 0.0 {
                    bail!("cohort {label:?}: group_size must not be negative, got {size}");
                }
            }
            (kind, Some(_)) => bail!(
                "cohort {label:?}: group_size is a pre-formed group's passengers, and {kind} \
                 does not travel with one"
            ),
            _ => {}
        }

        if self.walks_unclaimed
            && (self.advanced_declaration_lead_s == 0.0 || !self.kind.is_rider())
        {
            bail!(
                "cohort {label:?}: walks_unclaimed is a declaring rider setting off unclaimed, so it \
                 needs a rider with advanced_declaration_lead_s above zero"
            );
        }

        if self.times_to_counterpart && self.advanced_declaration_lead_s == 0.0 {
            bail!(
                "cohort {label:?}: times_to_counterpart times the leaving to an advance match, so \
                 it needs advanced_declaration_lead_s above zero"
            );
        }

        if let Some(radius_km) = self.perception_radius_km {
            if !radius_km.is_finite() || radius_km <= 0.0 {
                bail!("cohort {label:?}: perception_radius_km must be positive, got {radius_km}");
            }
            match self.kind {
                AgentKind::GhostDriver => {}
                AgentKind::OpportunisticDriver
                    if self.station_approach == StationApproach::OnDemand => {}
                AgentKind::OpportunisticDriver => bail!(
                    "cohort {label:?}: an opportunistic driver picks up on the road only while \
                     driving its own route, so perception_radius_km needs station_approach \
                     on_demand"
                ),
                kind => bail!(
                    "cohort {label:?}: perception_radius_km is for ghost and opportunistic \
                     drivers, and {kind} is neither"
                ),
            }
        }
        if self.registers == Registers::AfterFirstPickup
            && (self.kind != AgentKind::OpportunisticDriver || self.perception_radius_km.is_none())
        {
            bail!(
                "cohort {label:?}: registering after the first pickup is an opportunistic driver \
                 picking somebody up on the road, so it needs that kind and perception_radius_km"
            );
        }

        let fleet_rider = self.kind == AgentKind::AutonomousTaxiRider;
        if self.service == Service::Network && !fleet_rider {
            bail!(
                "cohort {label:?}: service is how a fleet rider travels, and {} is not one",
                self.kind
            );
        }
        if let Some(transfer) = &self.transfer_max_wait_s {
            if self.service != Service::Network {
                bail!(
                    "cohort {label:?}: transfer_max_wait_s is the patience at a change of taxi, \
                     and only a network fleet rider changes"
                );
            }
            transfer.validate(&format!("cohort {label:?}: transfer_max_wait_s"))?;
        }
        if !self.assigned_wait_factor.is_finite() || self.assigned_wait_factor < 1.0 {
            bail!(
                "cohort {label:?}: assigned_wait_factor must be at least 1, got {}",
                self.assigned_wait_factor
            );
        }
        if self.assigned_wait_factor != 1.0 && !fleet_rider {
            bail!(
                "cohort {label:?}: assigned_wait_factor stretches a fleet rider's patience once a \
                 taxi is assigned, and {} is not one",
                self.kind
            );
        }

        if self.chained_drop_offs && self.kind != AgentKind::AutonomousTaxi {
            bail!(
                "cohort {label:?}: chained_drop_offs is how a fleet taxi sets its riders down, and \
                 {} is not one - a carpool vehicle always sets each rider down at its own line's \
                 destination",
                self.kind
            );
        }

        if self.takes_unregistered_riders && self.kind != AgentKind::CarpoolDriver {
            bail!(
                "cohort {label:?}: takes_unregistered_riders is a carpool driver taking walk-ups at \
                 its station, and {} is not one",
                self.kind
            );
        }

        if self.rechain
            && !matches!(
                self.kind,
                AgentKind::CarpoolDriver | AgentKind::OpportunisticDriver
            )
        {
            bail!(
                "cohort {label:?}: rechain is a registered driver choosing a line again after a \
                 drop-off, and {} is not one",
                self.kind
            );
        }

        match (self.kind.is_driver(), &self.vehicle) {
            (true, None) => bail!("cohort {label:?}: {} needs a vehicle", self.kind),
            (false, Some(_)) => {
                bail!("cohort {label:?}: {} must not declare a vehicle", self.kind)
            }
            _ => {}
        }
        // Nobody rides with a robot: every seat in a fleet vehicle is one the service can sell,
        // which is why its capacity counts passengers rather than passengers plus a driver.
        if self.kind == AgentKind::AutonomousTaxi && self.additional_passengers.nominal() > 0.0 {
            bail!(
                "cohort {label:?}: {} carries nobody of its own, so additional_passengers means \
                 nothing to it",
                self.kind
            );
        }
        if let Some(vehicle) = &self.vehicle {
            if vehicle.capacity == 0 {
                bail!("cohort {label:?}: vehicle capacity must be at least 1");
            }
            // The driver occupies a seat, so the group must leave at least that one free. Only
            // a fixed figure is an error: a drawn party is clamped to the seats there are when
            // the agent spawns, because a distribution with a long tail is not a mistake.
            if let Value::Fixed(additional) = self.additional_passengers {
                if additional.round() as u32 >= vehicle.capacity {
                    bail!(
                        "cohort {label:?}: {} additional passengers do not fit a capacity of {}",
                        self.additional_passengers,
                        vehicle.capacity
                    );
                }
            }
        }

        Ok(())
    }
}

fn check_coordinate(latitude: f64, longitude: f64, label: &str) -> Result<()> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        bail!("{label}: latitude {latitude} is out of range");
    }
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        bail!("{label}: longitude {longitude} is out of range");
    }
    Ok(())
}
