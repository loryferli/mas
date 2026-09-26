//! Translating a scenario from the agent-list format into this engine's own.
//!
//! The agent-list format is a flat list of classes with a spawn distribution, a departure window and
//! a vehicle each, and a set of stations and networks beside it. Translation is pure: no behaviour
//! lives here, and every choice it makes is either a field-for-field mapping or a knob written out
//! explicitly, so a reader of the output sees the whole of what the scenario asks for.
//!
//! Two variants. `as-ran` reproduces the format's own engine as it ran: a 30 s tick, walking at
//! 4 m/s, a declaring rider that never times itself to its driver, no departure guarantee (the
//! format's engine never dispatched one), a fleet vehicle one seat short of its capacity and, on a
//! carpool-lane scenario, a rider with no patience. `corrected` keeps the model and drops those:
//! a 1 s tick, 1.4 m/s, riders timing to their drivers, the guarantee, the full capacity and half an
//! hour of patience. The difference between the two runs is a finding about the published figures.
//!
//! Unknown fields and classes are errors, like everywhere else a scenario is read.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

use crate::scenario::Scenario;

/// Which engine the output reproduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Variant {
    AsRan,
    Corrected,
}

/// Long enough for the latest evening peak and every trip after it, and short enough that a rider
/// with no patience at all still ends the run.
const MAX_TIME_S: f64 = 2.0 * 86_400.0;

/// A cohort whose engine gave its riders no limit on walking, or no patience, is given the run.
const UNLIMITED_S: f64 = MAX_TIME_S;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    simulation: Simulation,
    environment: SourceEnvironment,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Simulation {
    start: Start,
    // Always empty in every scenario the format was run with; present, it must stay so.
    #[serde(default)]
    #[allow(dead_code)]
    running: Option<Empty>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    #[serde(default)]
    agents: Vec<SourceAgent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEnvironment {
    #[serde(default)]
    stations: Vec<SourceStation>,
    #[serde(default)]
    networks: Vec<SourceNetwork>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatLon {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceStation {
    #[allow(dead_code)] // which network a station belongs to is the line's business
    orchestrator: String,
    coordinates: LatLon,
    name: String,
    // Display only.
    #[serde(default)]
    radius: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceNetwork {
    name: String,
    orchestrator: String,
    lines: Vec<SourceLine>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceLine {
    origin: String,
    destination: String,
    // Display only.
    #[serde(default)]
    polyline: Option<String>,
    #[serde(default)]
    color: Option<Vec<u8>>,
    #[serde(default)]
    gd: Option<SourceGuarantee>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceGuarantee {
    threshold: f64,
    trigger: f64,
    minimum_between_two_start: f64,
    capacity: u32,
    coordinates: LatLon,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAgent {
    class: String,
    name: String,
    distribution: SourceDistribution,
    #[serde(default)]
    origin: Option<Point>,
    #[serde(default)]
    destination: Option<Point>,
    #[serde(default)]
    departure_time_window: Option<Window>,
    #[serde(default)]
    vehicle: Option<SourceVehicle>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceDistribution {
    count: u32,
    start: f64,
    end: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Point {
    latitude: f64,
    longitude: f64,
    neighborhood: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    #[serde(default)]
    shift: f64,
    /// Read by no class this translates to anything but a carpool copy; noted and dropped.
    #[serde(default)]
    margin: Option<f64>,
    #[serde(default)]
    atc: f64,
    #[serde(default)]
    atp: f64,
    #[serde(default)]
    incertitude: Option<[f64; 2]>,
    #[serde(default)]
    earliness_margin: f64,
    #[serde(default)]
    lateness_margin: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceVehicle {
    capacity: u32,
}

/// Which of the format's two engines a scenario belongs to: the one with a fleet and a private car,
/// or the carpool-lane one before it. The two differ in seats, companions and spawn times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Lane,
    Fleet,
}

/// The translated scenario as JSON text, validated by loading it, and every note a reader should
/// see about what was dropped or approximated.
///
/// `road_detour_factor` is the study area's own, measured on its cached road geometry; the format
/// had none, because its router was a road network.
pub fn translate(
    text: &str,
    name: &str,
    variant: Variant,
    road_detour_factor: f64,
) -> Result<(String, Vec<String>)> {
    let source: Source = serde_json::from_str(text).context("parsing the agent-list scenario")?;
    let mut notes: Vec<String> = Vec::new();

    let fleet_classes = [
        "Driver",
        "AutonomousTaxi",
        "AutonomousTaxiRider",
        "AutonomousTaxiOrchestrator",
    ];
    let engine = match source
        .simulation
        .start
        .agents
        .iter()
        .any(|agent| fleet_classes.contains(&agent.class.as_str()))
    {
        true => Engine::Fleet,
        false => Engine::Lane,
    };
    let fleet_operators: Vec<&str> = source
        .simulation
        .start
        .agents
        .iter()
        .filter(|agent| agent.class == "AutonomousTaxiOrchestrator")
        .map(|agent| agent.name.as_str())
        .collect();
    let network_fleet = source.environment.networks.iter().any(|network| {
        fleet_operators.contains(&network.orchestrator.as_str()) && !network.lines.is_empty()
    });

    if source
        .environment
        .stations
        .iter()
        .any(|s| s.radius.is_some())
        || source
            .environment
            .networks
            .iter()
            .flat_map(|n| &n.lines)
            .any(|l| l.polyline.is_some() || l.color.is_some())
    {
        notes.push(
            "station radius, line polyline and line colour are display-only and dropped".into(),
        );
    }

    let stations: Vec<Json> = source
        .environment
        .stations
        .iter()
        .map(|station| {
            json!({
                "name": station.name,
                "latitude": station.coordinates.latitude,
                "longitude": station.coordinates.longitude,
            })
        })
        .collect();

    let mut networks: Vec<Json> = Vec::new();
    for network in &source.environment.networks {
        let mut lines: Vec<Json> = Vec::new();
        for line in &network.lines {
            let mut out = Map::new();
            out.insert("origin".into(), json!(line.origin));
            out.insert("destination".into(), json!(line.destination));
            if let Some(gd) = &line.gd {
                // The format's operator dispatches only while both are positive.
                let armed = gd.trigger > 0.0 && gd.threshold > 0.0;
                match (armed, variant) {
                    (false, _) => notes.push(format!(
                        "the guarantee on {} -> {} has a trigger or threshold of zero, so it never \
                         fired, and is dropped",
                        line.origin, line.destination
                    )),
                    (true, Variant::AsRan) => notes.push(format!(
                        "the guarantee on {} -> {} is dropped: the format's engine created the \
                         vehicle and never put it in the world",
                        line.origin, line.destination
                    )),
                    (true, Variant::Corrected) => {
                        out.insert(
                            "departure_guarantee".into(),
                            json!({
                                "trigger_after_wait_s": gd.trigger,
                                "cooldown_s": gd.minimum_between_two_start,
                                // Its capacity counted riders; here it counts the driver's seat too.
                                "capacity": gd.capacity + 1,
                                "latitude": gd.coordinates.latitude,
                                "longitude": gd.coordinates.longitude,
                            }),
                        );
                    }
                }
            }
            lines.push(Json::Object(out));
        }
        networks.push(json!({
            "name": network.name,
            "operator": network.orchestrator,
            "lines": lines,
        }));
    }

    let mut cohorts: Vec<Json> = Vec::new();
    for agent in &source.simulation.start.agents {
        if agent.class.ends_with("Orchestrator") {
            continue;
        }
        cohorts.push(cohort(agent, engine, variant, network_fleet, &mut notes)?);
    }

    let fleet_present = source
        .simulation
        .start
        .agents
        .iter()
        .any(|agent| agent.class == "AutonomousTaxi");
    let scenario = json!({
        "name": name,
        "tick_s": match variant { Variant::AsRan => 30.0, Variant::Corrected => 1.0 },
        "max_time_s": MAX_TIME_S,
        "walk_speed_mps": match variant { Variant::AsRan => 4.0, Variant::Corrected => 1.4 },
        "drive_speed_mps": 20.0,
        "road_detour_factor": road_detour_factor,
        // The format's fleet operator weighs riders by crowd and urgency; its line choice is the
        // ranking's first.
        "policy": match (fleet_present, variant) {
            (false, _) => "heuristic",
            (true, Variant::AsRan) => "urgency-as-ran",
            (true, Variant::Corrected) => "urgency",
        },
        "environment": { "stations": stations, "networks": networks },
        "cohorts": cohorts,
    });
    let text = serde_json::to_string_pretty(&scenario)? + "\n";
    Scenario::from_json(&text).context("the translated scenario does not load")?;
    Ok((text, notes))
}

fn cohort(
    agent: &SourceAgent,
    engine: Engine,
    variant: Variant,
    network_fleet: bool,
    notes: &mut Vec<String>,
) -> Result<Json> {
    let label = &agent.name;
    let kind = match agent.class.as_str() {
        "Driver" => "PrivateDriver",
        "CarpoolDriver" | "GhostDriver" | "OpportunisticDriver" | "NeglectfulDriver" => {
            agent.class.as_str()
        }
        "CarpoolRider" | "GhostRider" | "AutonomousTaxi" | "AutonomousTaxiRider" => {
            agent.class.as_str()
        }
        // The format's pre-formed group is a carpool copy, not a group: it is translated as the
        // carpool kind it behaves as.
        "PolynomialDriver" => {
            notes.push(format!(
                "{label:?}: PolynomialDriver translated as the CarpoolDriver it copies"
            ));
            "CarpoolDriver"
        }
        "PolynomialRider" => {
            notes.push(format!(
                "{label:?}: PolynomialRider translated as the CarpoolRider it copies"
            ));
            "CarpoolRider"
        }
        "TaxiDriver" => {
            bail!("cohort {label:?}: a TaxiDriver is dispatched by a guarantee, never spawned")
        }
        other => bail!("cohort {label:?}: unknown class {other:?}"),
    };
    let rider = matches!(kind, "CarpoolRider" | "GhostRider" | "AutonomousTaxiRider");
    let carpool_class = matches!(
        agent.class.as_str(),
        "CarpoolDriver"
            | "NeglectfulDriver"
            | "PolynomialDriver"
            | "CarpoolRider"
            | "PolynomialRider"
    );
    let window = agent.departure_time_window.as_ref();
    let default_window = Window::default();
    let window = window.unwrap_or(&default_window);
    if window.margin.is_some_and(|margin| margin != 0.0) {
        notes.push(format!(
            "{label:?}: departure_time_window.margin is read by no translated class"
        ));
    }

    let mut out = Map::new();
    out.insert("kind".into(), json!(kind));
    out.insert("name".into(), json!(label));
    out.insert("count".into(), json!(agent.distribution.count));

    // Spawn times: the lane engine drew them uniformly over the window; the fleet engine ignored the
    // window and read the peak off the cohort's name.
    let spawn = match engine {
        Engine::Lane => {
            json!({ "start_s": agent.distribution.start, "end_s": agent.distribution.end })
        }
        Engine::Fleet if agent.name.contains("outbound") => {
            json!({ "normal": { "mean": 27000.0, "sd": 1800.0 } })
        }
        Engine::Fleet if agent.name.contains("return") => {
            json!({ "normal": { "mean": 66600.0, "sd": 1800.0 } })
        }
        Engine::Fleet if agent.name.contains("Taxi") => json!(18000.0),
        Engine::Fleet => json!(0.0),
    };
    out.insert("spawn_window".into(), spawn);

    // ponytail: the format scatters a point uniformly over a square of side twice the neighbourhood,
    // in degrees; here the scatter is a disc of that radius. Same centre and scale, different shape.
    let area = |point: &Point| json!({ "latitude": point.latitude, "longitude": point.longitude, "jitter_radius_km": point.neighborhood });
    let origin = agent
        .origin
        .as_ref()
        .with_context(|| format!("cohort {label:?}: no origin"))?;
    out.insert("origin".into(), area(origin));
    if kind != "AutonomousTaxi" {
        let destination = agent
            .destination
            .as_ref()
            .with_context(|| format!("cohort {label:?}: no destination"))?;
        out.insert("destination".into(), area(destination));
    }

    // Declaring: the lead is how far ahead of departing the trip is told, and the declared departure
    // is that far after spawning. Only the carpool classes read the window at all.
    let lead_s = match carpool_class {
        true if rider => window.atp,
        true => window.atc,
        false => 0.0,
    };
    // The neglectful driver and every carpool class leave at spawn + shift (+ lead); the others set
    // off at spawn whatever the window says.
    let offset_s = match carpool_class {
        true => window.shift + lead_s,
        false => 0.0,
    };
    if offset_s != 0.0 {
        out.insert("departure_offset_s".into(), json!(offset_s));
    }
    if lead_s > 0.0 {
        // Written out even at zero, so a sweep can move them: a pointer has to resolve.
        out.insert("advanced_declaration_lead_s".into(), json!(lead_s));
        out.insert("earliness_margin_s".into(), json!(window.earliness_margin));
        out.insert("lateness_margin_s".into(), json!(window.lateness_margin));
        // A declaring driver times itself to its rider; a declaring rider was meant to and, in the
        // format's engine, never did.
        let times = match (rider, variant) {
            (false, _) => true,
            (true, Variant::AsRan) => false,
            (true, Variant::Corrected) => true,
        };
        out.insert("times_to_counterpart".into(), json!(times));
        // The format's declaring rider sets off at its time whether or not a driver took it.
        if rider {
            out.insert("walks_unclaimed".into(), json!(true));
        }
    } else if window.earliness_margin > 0.0 || window.lateness_margin > 0.0 {
        notes.push(format!(
            "{label:?}: margins without a lead bound nothing and are dropped"
        ));
    }
    if carpool_class {
        // Zero written out as zero, which draws nothing, and still gives a sweep a pointer.
        let noise = match window.incertitude {
            Some([mean, sd]) if mean != 0.0 || sd != 0.0 => {
                json!({ "normal": { "mean": mean, "sd": sd } })
            }
            _ => json!(0.0),
        };
        out.insert("departure_noise_s".into(), noise);
    }

    // Patience and walking.
    if rider {
        out.insert("max_walk_s".into(), json!(UNLIMITED_S));
        let patience_s = match (kind, engine, variant) {
            // The lane engine's carpool rider had no patience, and its ghost rider froze past
            // 1500 s of waiting.
            ("CarpoolRider", Engine::Lane, Variant::AsRan) => UNLIMITED_S,
            ("GhostRider", Engine::Lane, Variant::AsRan) => 1500.0,
            _ => 1800.0,
        };
        out.insert("max_wait_s".into(), json!(patience_s));
    }
    if kind == "AutonomousTaxiRider" {
        out.insert("assigned_wait_factor".into(), json!(1.5));
        out.insert(
            "additional_passengers".into(),
            json!({ "bernoulli": { "p": 0.07 } }),
        );
        if network_fleet {
            out.insert("service".into(), json!("network"));
            out.insert("transfer_max_wait_s".into(), json!(900.0));
        }
    }

    // The drivers' behaviour on the road.
    match kind {
        // The format's carpool driver takes whoever is waiting where it stops, ghost riders
        // included; its opportunistic and guarantee drivers take registered riders only.
        "CarpoolDriver" => {
            out.insert("station_approach".into(), json!("on_demand"));
            out.insert("rechain".into(), json!(true));
            out.insert("takes_unregistered_riders".into(), json!(true));
        }
        "GhostDriver" => {
            out.insert("perception_radius_km".into(), json!(0.1));
            out.insert("max_detour_pct".into(), json!(110.0));
        }
        "OpportunisticDriver" => {
            out.insert("station_approach".into(), json!("on_demand"));
            out.insert("perception_radius_km".into(), json!(0.1));
            out.insert("max_detour_pct".into(), json!(110.0));
            out.insert("registers".into(), json!("after_first_pickup"));
        }
        _ => {}
    }

    // Companions: one car in three on the lane engine, every driver; 7% of private cars on the fleet
    // engine.
    let driver = !rider;
    let companion_p = match (engine, kind) {
        (Engine::Lane, _) if driver && kind != "AutonomousTaxi" => Some(1.0 / 3.0),
        (Engine::Fleet, "PrivateDriver") => Some(0.07),
        _ => None,
    };
    if let Some(p) = companion_p {
        out.insert(
            "additional_passengers".into(),
            json!({ "bernoulli": { "p": p } }),
        );
    }

    if driver {
        let vehicle = agent
            .vehicle
            .as_ref()
            .with_context(|| format!("cohort {label:?}: a driver needs a vehicle"))?;
        let capacity = match (engine, kind, variant) {
            // The lane engine counted riders only; here the driver's seat is counted too.
            (Engine::Lane, _, _) => vehicle.capacity + 1,
            // The fleet engine's test was strict, so a vehicle carried one fewer than it said.
            (Engine::Fleet, "AutonomousTaxi", Variant::AsRan) => {
                vehicle.capacity.saturating_sub(1).max(1)
            }
            _ => vehicle.capacity,
        };
        out.insert("vehicle".into(), json!({ "capacity": capacity }));
    } else if agent.vehicle.is_some() {
        notes.push(format!("{label:?}: a rider's vehicle is dropped"));
    }
    Ok(Json::Object(out))
}
