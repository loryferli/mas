# mas

A multi-agent simulator for on-demand shared mobility on low-flow networks, in Rust.

The question it exists to answer: **can a ridesharing service survive on a network carrying very
little traffic, and what does it cost to run one?** It answers it on real geography, north Ceredigion
into Aberystwyth in Mid Wales, with demand from a census travel-to-work table and boarding points
from OpenStreetMap. A run is a pure function of its scenario, its seed and the committed route
cache: the same three inputs always write byte-identical output, and every committed number can be
regenerated offline, with no network access at all.

**The answers live in [`FINDINGS.md`](FINDINGS.md).** That file is the argument: what was swept, what
moved, what did not, and at what cost, with every figure traced to a committed run. This file is how
to run the thing.

## Where it comes from

Two pieces of earlier work, and this repository is the attempt to answer what neither of them could.

**MSc thesis, Politecnico di Torino, 2025: *Ridesharing: an analysis of performance in low-flow
networks*.** Three techniques aimed at one question, and the honest answer was no, or at least not
provably: none of the behavioural levers moved participation enough to turn an unstable network into
a working one. Anticipated departure declaration showed a path only under forced full adoption, and
even there the simulation gave no clear statistical separation between scenarios. What did come out
were cost figures, which are enough to make a strategic decision with and not at all the same thing
as a solution.

**PFIA 2025, Dijon, pp. 86–94: *Véhicules autonomes : simulation multi-agents pour explorer
l'importance de la structuration de réseaux*.** A multi-agent simulation of autonomous on-demand
mobility, asking whether the network a service runs on should be structured or left free-form.
Structured won, and not marginally: better route optimisation, fewer kilometres driven with
passengers aboard, and a service that starts to behave like collective transport rather than a fleet
of taxis. The same structuring looked applicable to carpooling in peri-urban areas: an on-demand
service built around a few trunk lines, which we called *Mobility as a Network*.

What this repository adds is the part both were short of: **a cost column beside every mobility column,
real geography instead of a synthetic grid, and comparisons that are differenced seed by seed rather
than read off two means with overlapping bands.** Some of it confirms the earlier work and some of it
does not; `FINDINGS.md` says which, in place, rather than quietly dropping what changed direction.

Both earlier studies can be re-run here - `FINDINGS.md` calls them the *carpool study* and the
*fleet study*. Their scenarios are rebuilt from open data on a second study
area, the Rhône and the lane from Bourgoin-Jallieu into Lyon (`data/lyon/`), translated into this
engine's scenarios by `cargo run -- import`, and run beside the simulator that produced the published
figures by a harness (`scripts/reference/`); `FINDINGS.md` says where the two agree and where the
published figures do not survive.

## Running it

```bash
cargo test                                     # 118 tests
cargo run -- run --scenario scenarios/baseline.json --seed 42 --out /tmp/a
```

That writes two files into `/tmp/a`: `events.csv`, one row per state transition with the reason for
it, and `metrics.json`. The same seed and the same scenario always produce byte-identical output, so
determinism is a `diff` rather than a claim:

```bash
cargo run -- run --scenario scenarios/baseline.json --seed 42 --out /tmp/b
diff -r /tmp/a /tmp/b                          # empty
```

A single run is not a result. The sweep is what turns one into a result: a base scenario, a set of
axes, a seed range, and one tidy row per run.

```bash
cargo run --release -- sweep --config sweeps/lowflow.json     --out analysis/results.csv
cargo run --release -- sweep --config sweeps/policies.json    --out analysis/policies.csv
cargo run --release -- sweep --config sweeps/networks.json    --out analysis/networks.csv
cargo run --release -- sweep --config sweeps/walking.json     --out analysis/walking.csv
cargo run --release -- sweep --config sweeps/fleet.json       --out analysis/fleet.csv
cargo run --release -- sweep --config sweeps/declaration.json --out analysis/declaration.csv
cargo run --release -- sweep --config sweeps/adoption.json    --out analysis/adoption.csv
uv run analysis/charts.py analysis/results.csv  # regenerates the committed SVGs, byte-identical
```

Every sweep runs the *same* seed range at every point on its axes, which is what lets two points be
differenced per seed and reported with a win/tie/loss count instead of a band. `charts.py` takes any
sweep CSV, reads its axes off the header, and plots the mean over seeds with the seed-to-seed range
as a band, because one seed of a stochastic model is not a finding.

The other services the corridor can be asked about, and the other rules it can be run under:

```bash
cargo run -- run --scenario scenarios/fleet.json --seed 42     # door to door, no lines at all
cargo run -- run --scenario scenarios/network.json --seed 42   # a derived line set
cargo run -- run --scenario scenarios/baseline.json --seed 42 --policy greedy
cargo run -- run --scenario scenarios/baseline.json --seed 42 --policy pairs
cargo run -- run --scenario scenarios/baseline.json --seed 42 \
  --incentive per-passenger-km --rate 0.10
```

Two commands touch the network, both are run by hand, and both commit their output, so nothing in
the list above ever reaches it:

```bash
uv run scripts/fetch-data.py                   # once: data/flows.csv, data/stations.json
cargo run -- build-cache                       # once: data/routes.json, via curl
```

`tests/study_area.rs` asserts that mechanically rather than trusting it: no Rust source outside
`routing.rs` may name a fetch host, and process spawning is confined to two named modules.

Also worth having:

```bash
cargo clippy --all-targets -- -D warnings      # warnings are errors
cargo fmt --check
cargo test --test declaration                  # one integration test file
```

## The study area

North Ceredigion into Aberystwyth, Mid Wales: five middle-layer super output areas, 44,485
residents, seven boarding points across 23 km of the A487 and A44. 11,772 commutes run between the
five areas, and the largest single pair is 1,235 people a day with 62% of them driving alone. That is
the low-flow case the engine exists to model.

An area is in the corridor if at least a quarter of its working residents work in the two
Aberystwyth areas, a criterion computable from `data/flows.csv` alone: Ceredigion's other four areas
fall below it at 18%, 15%, 14% and 7%, and commute to Cardigan and Lampeter instead. Boarding points
come from OpenStreetMap and are selected by rule rather than by hand: every national-rail station,
bus station and named park-and-ride inside a corridor area, plus the bus stop nearest each area's
population-weighted centroid, with near-duplicates within 300 m collapsed.

**`data/README.md` is the authority** on the sources, the licences and the exact filters. Read it
before quoting any figure from a run. Two things are worth knowing here:

- The demand table is **2011** Census WU03EW rather than 2021, for a licensing reason and not a
  preference: the 2021 origin-destination tables are *safeguarded* at this resolution and cannot be
  committed, and the open 2021 release is one cell per local authority. WU03EW is the most recent
  openly licensed area-to-area table carrying a **mode breakdown**, which is the whole point of it:
  it separates *driving a car or van* from *passenger in a car or van*, so the corridor arrives with
  an observed car-passenger share of 9.95% of car commutes. That is the one outside number available
  for asking whether a modelled occupancy is plausible at all. It is **never a target to tune
  against**, which would make the model circular.
- **Two numbers in the corridor are invented, and both are stated out loud.** A scenario samples one
  in twenty of the observed flow on both sides and enrols one car driver in ten: the corridor's own
  ratio is nine solo drivers to one car passenger, so enrolling every driver is a service with no
  scarcity in it and nothing to study. The sweeps move that adoption from a twentieth to all of it,
  which is 6 to 132 vehicles. Everything downstream is a ratio against those two numbers.

## From the census table to a scenario

`data/README.md` is the authority on the sources and the licences. This is what the dataset
*decides*, which is a different question and the one a reader of a result actually needs answered.

**The census table decides three things and not a fourth.** It decides **which areas are in the
corridor** - the 25%-of-working-residents rule, computable from `flows.csv` alone. It decides
**where a cohort starts**: every cohort origin is its area's population-weighted centroid, with a
jitter radius around it, so agents come from where the people are rather than from the middle of a
polygon. And it decides **two of the five line constructions**: `flow-ranked` reads how big a flow
is, `flow-directed` reads which way it runs, and the other three never open the file. What it does
**not** decide is how many agents there are.

**How many agents there are is invented, and here is the correspondence.** Each of the four
origin areas becomes one driver cohort and one rider cohort in `scenarios/baseline.json`:

| cohort pair | area | inbound commutes | car drivers | car passengers | scenario drivers | scenario riders |
|---|---|---:|---:|---:|---:|---:|
| Borth and Bow Street | W02000116 | 1,563 | 1,246 | 130 | 4 | 10 |
| Llanbadarn | W02000118 | 1,135 | 478 | 85 | 2 | 9 |
| the coast road south | W02000120 | 773 | 628 | 44 | 2 | 4 |
| the Rheidol and Ystwyth valleys | W02000421 | 1,897 | 1,554 | 164 | 5 | 9 |
| **total** | | **5,368** | **3,906** | **423** | **13** | **32** |

(Inbound means into the two Aberystwyth areas, W02000117 and W02000118, summed over both. Aberystwyth
town is a destination and not a cohort origin.)

The scenario counts stand in for two rates the census cannot supply - what fraction of the flow a
morning's service sees, and how many solo drivers would enrol on a platform - and both are invented,
one in twenty of the flow and one car driver in ten. **They are hand-set
integers, not a formula evaluated over `flows.csv`, and no script derives them.** Read the columns
above as a correspondence rather than as a calculation, read every result downstream as a ratio
against those thirteen and thirty-two, and hold `mean_occupancy` next to the corridor's observed
9.95% car-passenger share, which is the one outside number that was never tuned against.

## How a run becomes a result

**One run per seed. There is no averaging inside a seed.** A run is a pure function of its scenario,
its seed and the committed route cache, so running the same seed twice produces byte-identical
output and would add nothing. **The ten seeds are the ten replications**, and every sweep runs the
*same* seed range at every point on its axes - which is what makes a paired difference possible, and
why comparisons in `FINDINGS.md` come with a win/tie/loss count over the ten seeds rather than two
means with overlapping error bands.

`charts.py` plots the mean over those ten seeds with the full seed-to-seed range as a band. One seed
of a stochastic model is not a finding, and the band is what separates the shape of the parameter
from the shape of the draw.

**One exception, and it is the model policies.** `llm` and `hybrid` call a model, and the model is
not deterministic, so under those two a seed no longer pins the run. `sweeps/model-policies.json` is
5 seeds x 1 run, and there seed variance and model variance are confounded: it reports a number, not
an interval, and it says so where it reports one.

**One thing a seed does not hold fixed: the demand, across a supply axis.** All draws are taken up
front, cohort by cohort in scenario order, and the four driver cohorts come *before* the four rider
cohorts - so changing a driver count changes how much randomness is consumed before the riders are
drawn, and the riders land somewhere else entirely. At seed 1, the thirty-two riders at 48 drivers
and the thirty-two at 96 share **none** of their origins. A sweep over a driver count therefore pairs
*runs* on a seed, not *demand*, and a small difference on such an axis is inside the draw-to-draw
spread. Fixing this means drawing the cohorts from separate streams in `Sim::new`; until that
happens `FINDINGS.md` says it where it matters.

## What a seed controls

Every random draw in a run comes from one `ChaCha8Rng` seeded from the `--seed` argument
(`src/sim.rs:86`) - never a thread-local generator, and never `StdRng`, whose output is not
guaranteed stable across releases. This is the complete list, in the order the draws happen:

| # | Draw | Scope | Where |
|---|---|---|---|
| 1 | `walk_speed_mps` | once per run | `sim.rs:95` |
| 2 | `drive_speed_mps` | once per run | `sim.rs:96` |
| 3 | spawn time inside `spawn_window` | per agent | `sim.rs:102` |
| 4 | origin: a bearing, then a radius on the jitter disc | per agent | `sim.rs:1122` |
| 5 | destination: the same, on its own disc | per agent, except a fleet taxi, which has nowhere to be and draws nothing here | `sim.rs:1122` |
| 6 | `departure_offset_s` | per agent | `sim.rs:982` |
| 7 | `max_wait_s` | per agent | `sim.rs:983` |
| 8 | `max_walk_s` | per agent | `sim.rs:984` |
| 9 | `additional_passengers` | per agent | `sim.rs:985` |
| 10 | `max_detour_pct` | per agent | `sim.rs:991` |

Three things about that table matter more than the table.

- **A bare number in a scenario draws nothing.** A knob written as `2400` parses as `Fixed` and
  consumes no randomness at all (`scenario.rs:71`); only `{"uniform": ...}`, `{"normal": ...}` and
  `{"weibull": ...}` reach the generator. A knob a cohort never mentions draws nothing either. On
  `baseline.json` rows 6 to 10 are bare numbers or absent, so
  **the only randomness in the baseline is the two run speeds, each agent's spawn time, and the two
  endpoint jitters.** Everything else is identical on every seed.
- **The order is fixed and it is all up front.** Every draw above happens in `Sim::new`, cohort by
  cohort and then agent by agent within a cohort, before the first tick. Nothing is drawn during the
  run. That is what makes the draw sequence a function of the scenario alone - and also why a change
  to an earlier cohort's *count* moves every later cohort's values, which is the caveat in the
  previous section.
- **Nothing else draws.** No dispatch policy takes randomness (`policy.rs:27`), no tie-break is
  broken randomly - `ranked_lines` is a stable total-order sort and `greedy_pairs` is cheapest-first
  over a deterministic order - and the tick loop draws nothing. A run under `heuristic`, `greedy` or
  `pairs` is deterministic end to end; `tests/simulation.rs` asserts both halves of that, that the
  same seed reproduces a run and that a different seed scatters the riders differently.

## Metric columns

Verbatim in `metrics.rs`, in `metrics.json`, and as CSV headers in every sweep.

| Column | Meaning |
|---|---|
| `passengers_served` / `passengers_unserved` | riders that completed a ride, and riders that did not |
| `riders_total` / `drivers_total` | how many of each the scenario spawned |
| `drivers_active` | drivers that carried at least one passenger |
| `service_rate` | `passengers_served / riders_total` |
| `vehicle_km_total` | every kilometre any vehicle drove |
| `vehicle_km_loaded` / `vehicle_km_empty` | with somebody aboard, and without |
| `empty_distance_share` | `vehicle_km_empty / vehicle_km_total` |
| `passenger_km` | kilometres people were carried, summed over people |
| `mean_occupancy` | `passenger_km / vehicle_km_total`; above 1 means the average kilometre carried more than one person |
| `mean_journey_time_s` | spawn to arrival, for riders that arrived: includes both walks and the wait |
| `mean_wait_s` | time a rider spent standing at its departure station |
| `incentive_paid` / `cost_per_passenger_served` | what the scheme billed, and that divided by `passengers_served` |
| `taxi_km_empty_to_pickup` | fleet deadhead: the approach to a pickup and any repositioning, in one column because it is the same empty kilometre however it was spent |
| `mean_time_to_pickup_s` / `fleet_utilisation` | the fleet's two other columns; zero on a run with no fleet |
| `stranded` / `stopped_by_clock` | agents still going when the clock cap hit, and whether it hit. **A metric from a run with either set is not a finding** |

Kilometres per passenger served is the cost figure to read, and it is deliberately not a column: it
is computed per run and then averaged, so it is never the ratio of two means.

## Glossary

| Term | Meaning |
|---|---|
| station | a boarding point; riders walk to one, drivers pick up at one |
| line | a directed origin–destination pair between two stations |
| network | a set of lines run by one operator |
| operator | runs a network, and is the matching authority for its lines |
| cohort | a group of identical agents spawned over a window |
| advanced time declaration | announcing a trip to the operator ahead of departure, so it can be matched in advance rather than only on the spot. `advanced_declaration_lead_s` is how far ahead; `earliness_margin_s` / `lateness_margin_s` bound what departure times the agent will accept |
| departure guarantee | the operator's promise that a rider who waits past `trigger_after_wait_s` still gets a vehicle, subject to `cooldown_s` between dispatches |
| incentive | what a driver is paid for carrying someone; the reason the service exists |
| construction | how a line set is derived from a station set: free-form, Delaunay, minimum spanning tree, flow-ranked, or flow-directed. Only the last two read the demand table - one for how big a flow is, one for which way it runs, and only flow-directed emits one-way lines |
| fleet | the operator's own vehicles, with no trips of their own, serving door to door. `AutonomousTaxi` and `AutonomousTaxiRider`; the operator assigns rather than ranking lines |

Agent kinds are fixed vocabulary: **Private**, **Carpool**, **Ghost**, **Opportunistic**,
**Neglectful**, **Taxi**, **Polynomial**, **AutonomousTaxi**, separated on three axes: registered
with the operator, uses the app, willing to pick up. `PrivateDriver`, the private car, is the base
every other driver is built on and is on none of them. No acronyms anywhere, and the unit goes in the name: `_s`, `_km`,
`_pct`, `_mps`.

## What this does not model

Each of these is a deliberate limit rather than an oversight, and `FINDINGS.md` says what it costs
the conclusions.

- **No behavioural response to the incentive.** `--incentive per-passenger-km --rate 0.10` moves
  `incentive_paid` and `cost_per_passenger_served` and nothing else: no driver participates, detours
  further or picks a different line because of a payout. A test pins that column by column. It is
  the research question rather than plumbing, so it stays out until the engine has measured
  everything that does not depend on it.
- **A pre-formed group is a windfall and nothing else.** A `PolynomialDriver` and its `group_size`
  `PolynomialRider`s ride together, only with each other, and collect the incentive for a trip they
  were making anyway; `run` prints the share of `incentive_paid` that went to groups. Because no
  behaviour responds to a payout, a group never forms *because* of one.
- **Pair scoring is greedy per driver, and half of it is still missing.** `--policy pairs` drops the
  line as the match key and scores `(driver, rider)` pairs, but a driver decides on its own with no
  view of the rest of the fleet, so two of them can reach for the same rider and the apply pass
  gives it to the first. Assigning across the whole fleet at once is the same computation as the
  fleet's own greedy pairing over a different pool. What the collisions cost is counted rather than assumed - `Sim::claim_collisions`, ten in a
  whole run at thirteen drivers, and the loser re-decides on the next tick - so what is actually
  unbuilt is the *ordering*: a contested rider goes to the earlier driver in the arena rather than to
  the cheapest pair. A rider is also diverted at most once, and only while it is still walking: one
  already standing at a station is left where it is.
- **The route cache covers station-to-station pairs only.** Agent origins are jittered per seed, so
  a walk to a station never hits the cache and takes a straight-line fallback: the time is right and
  the distance is understated by the detour factor. Passenger-carrying legs are cached, so
  `vehicle_km_loaded` is a road distance. Every fleet leg is door to door and therefore uncached,
  which means the fleet's kilometres are understated and a fleet-against-carpool distance comparison
  is not apples to apples.
- **`max_walk_s` is 2400 s on the corridor's rider cohorts, and that assumption carries every
  service figure.** It prices both flanking legs, twenty minutes at each end, and the boarding
  points sit up to two kilometres from where the population actually is. `sweeps/walking.json` is
  what moving it does, and the result is a statement about walking distance in rural Wales rather
  than about matching.
- **Two speeds are drawn once per run, not once per agent**, because under the ranking rule both
  sides of a match have to price a line's flanking legs identically to agree on a station. Per-agent
  speeds belong with a policy that no longer needs that agreement.
- **The fleet's vehicle capacity does nothing.** Capacity 2 and capacity 4 produce identical rows in
  `analysis/fleet.csv`, every column and every seed: riders are pooled only where they stand within
  fifty metres of each other, and on a 1.5 km jitter radius that never happens. Chaining drop-offs
  is the missing mechanism, and it is the same computation as the fleet-wide half of pair scoring.

## Layout

```
src/
  main.rs        CLI: run | sweep | build-cache
  scenario.rs    serde types, JSON loading, validation (deny_unknown_fields)
  world.rs       stations, lines, geometry, the arena of typed indices
  sim.rs         the tick loop: perceive -> decide -> apply
  agents/        Agent, Vehicle, State, TransitionReason, Influence; riders, drivers, the fleet
  operator.rs    the line ranking, the registry, the direction filter, the line options
  routing.rs     the committed route cache, the haversine fallback, build-cache
  network.rs     free-form / Delaunay / minimum-spanning-tree / flow-ranked /
                 flow-directed constructions
  policy.rs      the Policy trait, the four decision contexts, the leakage audit, the fixed rules
  incentives.rs  CompletedTrip, IncentiveScheme, NoIncentive, PerPassengerKm
  metrics.rs     end-of-run aggregation -> metrics.json
  sweep.rs       the axis cross product, the rayon runs, the tidy CSV
  events.rs      the event log
  import.rs      the agent-list scenario format -> this engine's scenarios
data/            flows.csv, stations.json, routes.json, and the README that is their authority
data/lyon/       the second study area, with its own README
scenarios/       minimal (synthetic), seven on the real corridor, lyon/ on the second study area
sweeps/          lowflow, policies, networks, walking, fleet, declaration, adoption, approach,
                 and lyon-* for the re-runs
scripts/         fetch-data.py, fetch-lyon.py, build-lyon-scenarios.py, the model sidecar and
                 network designer, and reference/ - the harness around the original simulator
analysis/        charts.py, timeseries.py, agreement.py, the committed CSVs (reference/ for the
                 original simulator's runs), and images/
tests/           integration tests only: no #[cfg(test)] modules in src/
```

Agents live in a `Vec<Agent>` and reference each other by `AgentId(u32)`, never `Rc<RefCell<_>>`.
`decide` is a pure function returning an `Influence` and mutates nothing; the apply pass resolves it,
which is what made the dispatch rule swappable without touching a state machine and removes a class
of ordering bugs where the result depended on which agent was updated first. Randomness is one seeded
`ChaCha8Rng` owned by the sim: never `StdRng`, whose output is not stable across releases, and never
a global. Debug builds assert the physical invariants every tick, so a logic regression fails a plain
`cargo run` rather than surfacing as a quietly wrong number several steps later.

Rust dependencies are `anyhow`, `clap`, `serde`, `serde_json`, `rand`, `rand_chacha`, `csv` and
`rayon`, and that is the final set: haversine is five lines, the distributions are four
closed-form inverse-CDF expressions, and `build-cache` shells out to `curl`. Python is `matplotlib`
for the charts and `anthropic` for the model sidecar, under `uv run`; the reference harness's own
dependencies are a separate group (`uv run --group reference`).

One thing this file deliberately leaves out: a fifth dispatch rule that puts a language model behind
the same seam as the fixed ones. The seam and its cost accounting are built and tested, but nothing
has been run through it, so there is no result to report and nothing here quotes one. `CLAUDE.md`
says what it would have to beat and what it would cost to find out.
