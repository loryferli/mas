use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use mas::events::EventLog;
use mas::import::{self, Variant};
use mas::incentives::{CompletedTrip, IncentiveScheme, Money, NoIncentive, PerPassengerKm};
use mas::metrics::Metrics;
use mas::policy::PolicyName;
use mas::routing::{self, Router};
use mas::scenario::{AgentKind, Scenario};
use mas::sim::Sim;
use mas::sweep;
use std::path::{Path, PathBuf};

/// The committed route cache, relative to wherever the binary is run from.
const DEFAULT_ROUTES: &str = "data/routes.json";

#[derive(Parser)]
#[command(
    name = "mas",
    about = "Shared-mobility simulation on low-flow networks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The incentive schemes a run can be priced under.
#[derive(Clone, Copy, ValueEnum)]
enum Incentive {
    /// Nobody is paid: the baseline every other scheme is measured against.
    None,
    /// Paid by the kilometre people were carried. Empty running earns nothing.
    PerPassengerKm,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single scenario.
    Run {
        #[arg(long)]
        scenario: PathBuf,

        /// Seed for every random draw in the run. The same seed and scenario always produce
        /// the same result.
        #[arg(long, default_value_t = 0)]
        seed: u64,

        /// Directory for the event log and metrics.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Committed route cache. A missing file is a warning, not an error: every leg then
        /// falls back to the straight line.
        #[arg(long, default_value = DEFAULT_ROUTES)]
        routes: PathBuf,

        /// What the service pays a driver for a trip it carried somebody on. Nothing in the run
        /// responds to the payout, so this moves `incentive_paid` and
        /// `cost_per_passenger_served` and leaves every other metric untouched.
        #[arg(long, value_enum, default_value_t = Incentive::None)]
        incentive: Incentive,

        /// Money per passenger-kilometre, for `--incentive per-passenger-km`. Zero pays nothing,
        /// which is `none` by another route.
        #[arg(long, default_value_t = 0.0)]
        rate: Money,

        /// Which line a driver takes, of the ones it would accept, and the operator's three other
        /// decisions besides. Overrides the scenario's own `policy` field; absent, the scenario
        /// decides. The sweep has no flag of its own: the field is a JSON pointer, so a policy
        /// axis puts every rule in one table.
        #[arg(long, value_enum)]
        policy: Option<PolicyName>,

        /// How a model policy reaches the model: a command speaking line-delimited JSON over
        /// stdin and stdout. Overrides the scenario's own `sidecar` field. Ignored by the two
        /// fixed rules, which start no process at all.
        #[arg(long)]
        sidecar: Option<String>,
    },

    /// Run a scenario across a set of axes and a seed range, one row per run.
    Sweep {
        /// Sweep configuration: the base scenario, the axes and the seed range.
        #[arg(long)]
        config: PathBuf,

        /// Where the tidy rows go.
        #[arg(long, default_value = "results.csv")]
        out: PathBuf,

        #[arg(long, default_value = DEFAULT_ROUTES)]
        routes: PathBuf,

        /// Held fixed across the sweep: no behaviour responds to a payout, so a scheme that
        /// moved between rows would move two columns and explain nothing.
        #[arg(long, value_enum, default_value_t = Incentive::None)]
        incentive: Incentive,

        #[arg(long, default_value_t = 0.0)]
        rate: Money,
    },

    /// Translate a scenario from the agent-list format into this engine's own, and write it.
    Import {
        /// The agent-list scenario to translate.
        source: PathBuf,

        /// Where the translated scenario goes.
        #[arg(long)]
        out: PathBuf,

        /// `as-ran` reproduces the format's own engine; `corrected` keeps its model without its
        /// bugs.
        #[arg(long, value_enum, default_value_t = Variant::AsRan)]
        variant: Variant,

        /// The study area's road detour factor, measured on its route cache.
        #[arg(long, default_value_t = 1.3)]
        road_detour_factor: f64,
    },

    /// Fetch road geometry for every station pair in a scenario directory and write the cache.
    ///
    /// This is the only command that touches the network. Run it by hand and commit the result.
    BuildCache {
        /// Directory of scenario files to collect station pairs from.
        #[arg(long, default_value = "scenarios")]
        scenarios: PathBuf,

        #[arg(long, default_value = DEFAULT_ROUTES)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            scenario,
            seed,
            out,
            routes,
            incentive,
            rate,
            policy,
            sidecar,
        } => run(
            scenario,
            seed,
            out,
            routes,
            scheme_for(incentive, rate).as_ref(),
            policy,
            sidecar,
        ),
        Command::Sweep {
            config,
            out,
            routes,
            incentive,
            rate,
        } => {
            let rows = sweep::run(&config, &out, &routes, scheme_for(incentive, rate).as_ref())?;
            eprintln!("wrote {} rows to {}", rows, out.display());
            Ok(())
        }
        Command::Import {
            source,
            out,
            variant,
            road_detour_factor,
        } => {
            let text = std::fs::read_to_string(&source)
                .with_context(|| format!("reading {}", source.display()))?;
            let name = out.file_stem().map_or("imported".to_string(), |stem| {
                stem.to_string_lossy().into_owned()
            });
            let (scenario, notes) = import::translate(&text, &name, variant, road_detour_factor)
                .with_context(|| format!("translating {}", source.display()))?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, scenario).with_context(|| format!("writing {}", out.display()))?;
            for note in notes {
                eprintln!("note: {note}");
            }
            eprintln!("wrote {}", out.display());
            Ok(())
        }
        Command::BuildCache { scenarios, out } => build_cache(&scenarios, &out),
    }
}

fn scheme_for(incentive: Incentive, rate: Money) -> Box<dyn IncentiveScheme + Sync> {
    match incentive {
        Incentive::None => Box::new(NoIncentive),
        Incentive::PerPassengerKm => Box::new(PerPassengerKm { rate }),
    }
}

fn build_cache(scenarios_dir: &Path, out: &Path) -> Result<()> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(scenarios_dir)
        .with_context(|| format!("reading {}", scenarios_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();

    let scenarios: Vec<Scenario> = paths
        .iter()
        .map(|path| Scenario::load(path))
        .collect::<Result<Vec<_>>>()?;
    eprintln!(
        "collecting station pairs from {} scenarios in {}",
        scenarios.len(),
        scenarios_dir.display()
    );
    routing::build_cache(&scenarios, out)?;
    Ok(())
}

/// Build a router for this scenario, loading the cache if it is there and saying so if it is not.
fn router_for(scenario: &Scenario, routes: &Path) -> Result<Router> {
    let mut router = Router::new(scenario);
    if routes.exists() {
        let count = router.load_cache(routes)?;
        eprintln!("routes     {count} cached from {}", routes.display());
    } else {
        eprintln!(
            "warning: no route cache at {}; every leg falls back to the straight line. \
             Run `build-cache` to fetch one.",
            routes.display()
        );
    }
    Ok(router)
}

fn run(
    path: PathBuf,
    seed: u64,
    out: Option<PathBuf>,
    routes: PathBuf,
    scheme: &dyn IncentiveScheme,
    policy: Option<PolicyName>,
    sidecar: Option<String>,
) -> Result<()> {
    let mut scenario = Scenario::load(&path)?;
    // Patched into the scenario rather than carried alongside it, so there is one place a run
    // reads its policy from and `Sim::new` stays a function of the scenario and the seed.
    if let Some(policy) = policy {
        scenario.policy = policy;
    }
    if let Some(sidecar) = sidecar {
        scenario.sidecar = Some(sidecar);
    }
    print_summary(&scenario, seed);
    let router = router_for(&scenario, &routes)?;

    let events_path = out.as_ref().map(|dir| dir.join("events.csv"));
    let mut log = match &events_path {
        Some(path) => EventLog::to_file(path)?,
        None => EventLog::discarding()?,
    };

    let mut sim = Sim::new(&scenario, seed, router);
    let summary = sim.run(&mut log)?;
    let metrics = Metrics::collect(sim.agents(), &summary, scheme);

    eprintln!(
        "\nran {} ticks to {:.0} s: {} agents spawned, {} finished, {} canceled, {} events",
        summary.ticks,
        sim.time_s(),
        summary.agents_spawned,
        summary.finished,
        summary.canceled,
        summary.events
    );
    print_metrics(&metrics);
    // The share of the incentive a pre-formed group collected for a trip it was making anyway:
    // reported here rather than as a `Metrics` column, which would change every committed sweep's
    // header for a figure only a scenario with groups in it has.
    let to_groups: Money = CompletedTrip::collect(sim.agents())
        .iter()
        .filter(|trip| sim.agents()[trip.driver.0 as usize].kind == AgentKind::PolynomialDriver)
        .map(|trip| scheme.reward(trip))
        .sum();
    if to_groups > 0.0 {
        println!(
            "  of which to groups    {:.2} ({:.0}%)",
            to_groups,
            to_groups * 100.0 / metrics.incentive_paid
        );
    }
    if summary.stopped_by_clock {
        eprintln!(
            "warning: stopped at the {:.0} s clock cap with {} agents still going; \
             a cut-off run's averages are not a finding",
            scenario.max_time_s, summary.stranded
        );
    }

    match (&events_path, &out) {
        (Some(events), Some(dir)) => {
            let metrics_path = dir.join("metrics.json");
            std::fs::write(&metrics_path, metrics.to_json()? + "\n")
                .with_context(|| format!("writing {}", metrics_path.display()))?;
            eprintln!("wrote {} and {}", events.display(), metrics_path.display());
        }
        _ => eprintln!("note: pass --out to keep the event log and the metrics"),
    }
    Ok(())
}

fn print_metrics(metrics: &Metrics) {
    println!("\npassengers served       {}", metrics.passengers_served);
    println!("service rate            {:.3}", metrics.service_rate);
    println!("drivers active          {}", metrics.drivers_active);
    println!("vehicle km total        {:.2}", metrics.vehicle_km_total);
    println!("vehicle km loaded       {:.2}", metrics.vehicle_km_loaded);
    println!("vehicle km empty        {:.2}", metrics.vehicle_km_empty);
    println!(
        "empty distance share    {:.3}",
        metrics.empty_distance_share
    );
    println!("passenger km            {:.2}", metrics.passenger_km);
    println!("mean occupancy          {:.3}", metrics.mean_occupancy);
    println!("mean wait               {:.0} s", metrics.mean_wait_s);
    println!(
        "mean journey time       {:.0} s",
        metrics.mean_journey_time_s
    );
    println!("incentive paid          {:.2}", metrics.incentive_paid);
    println!(
        "cost per passenger      {:.2}",
        metrics.cost_per_passenger_served
    );
    // Printed only when there was a model in the loop: on a fixed rule every one of these is
    // zero, and six zero lines under every run would be noise.
    if metrics.model_decisions > 0 {
        println!("\nmodel decisions         {}", metrics.model_decisions);
        println!("model calls             {}", metrics.model_calls);
        println!("usd per decision        {:.6}", metrics.usd_per_decision);
        println!("tokens per decision     {:.1}", metrics.tokens_per_decision);
        println!(
            "decision latency        {:.0} ms",
            metrics.decision_latency_ms
        );
        println!("invalid action rate     {:.3}", metrics.invalid_action_rate);
    }
}

fn print_summary(scenario: &Scenario, seed: u64) {
    println!("scenario   {}  (seed {seed})", scenario.name);
    println!("policy     {:?}", scenario.policy);
    if scenario.policy.needs_model() {
        println!("sidecar    {}", scenario.sidecar());
    }
    println!(
        "clock      {} s per tick, stopping at {} s",
        scenario.tick_s, scenario.max_time_s
    );

    println!("\nstations   {}", scenario.environment.stations.len());
    for station in &scenario.environment.stations {
        println!(
            "  {:<28} {:.5}, {:.5}",
            station.name, station.latitude, station.longitude
        );
    }

    for network in &scenario.environment.networks {
        println!(
            "\nnetwork    {} (operated by {})",
            network.name, network.operator
        );
        for line in &network.lines {
            let guarantee = match &line.departure_guarantee {
                Some(g) => format!(
                    "  departure guarantee after {:.0} s, {} seats, {:.0} s cooldown",
                    g.trigger_after_wait_s, g.capacity, g.cooldown_s
                ),
                None => String::new(),
            };
            println!("  {} -> {}{}", line.origin, line.destination, guarantee);
        }
    }

    println!("\ncohorts    {}", scenario.cohorts.len());
    let name_width = scenario
        .cohorts
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);
    for cohort in &scenario.cohorts {
        let declaration = if cohort.advanced_declaration_lead_s > 0.0 {
            format!(
                ", declares {:.0} s ahead (-{:.0}/+{:.0} s)",
                cohort.advanced_declaration_lead_s,
                cohort.earliness_margin_s,
                cohort.lateness_margin_s
            )
        } else {
            String::new()
        };
        let (spawn_start_s, spawn_end_s) = cohort.spawn_window.bounds_s();
        println!(
            "  {:<name_width$} {:>4} x {:<20} spawn {:.0}-{:.0} s{}",
            cohort.name,
            cohort.count,
            cohort.kind.to_string(),
            spawn_start_s,
            spawn_end_s,
            declaration
        );
    }

    println!("\ntotal      {} agents", scenario.total_agents());

    for kind in scenario.unimplemented_kinds() {
        eprintln!("warning: {kind} behaviour is not implemented; those agents will do nothing");
    }
}
