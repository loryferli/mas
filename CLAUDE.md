# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A multi-agent simulator for on-demand shared mobility on low-flow networks, in Rust. The research
question it exists to answer: can a ridesharing service survive on a network carrying very little
traffic, and what does it cost to run one?

Two earlier studies stand behind it (the README names them), and both can be re-run here: their
scenarios rebuilt from open data on a second study area, translated by `cargo run -- import`, and
run beside the simulator that produced the published figures - see *The second study area, and the
re-runs* below.

**`README.md` and `FINDINGS.md` are the reader-facing surface, and they are the contract.** They
divide cleanly: the README is what the thing is, where it came from and how to run it, and it
deliberately **carries no results at all**; `FINDINGS.md` is the argument the numbers support and is
the only place a figure belongs. Two consequences. A new finding is a `FINDINGS.md` edit and not a
README edit, so do not answer the research question in the README again. And the README documents no
model policy: the seam is built and tested but nothing has been run through it, so there is nothing
to report there and the README says exactly that, once, at the end. The rule for both files
is that **every figure traces to a committed run** - `analysis/results.csv`, `policies.csv`,
`networks.csv`, `walking.csv`, or a single run at a named seed that is labelled as one. If a number
cannot be traced, it does not go in. Changing a metric, a scenario or a sweep means regenerating the
CSVs and the charts and re-checking both documents in the same commit; a figure in prose is the
easiest thing in this repository to leave stale, and it has already happened twice. The public
write-up at `loryferli.github.io/mas/` (and `it/mas/` beside it) is derived from `FINDINGS.md`, and
it carries the committed SVGs as static files - no JavaScript, no plotting in the browser.

## Commands

```bash
cargo test
cargo clippy --all-targets -- -D warnings     # warnings are errors
cargo fmt --check

cargo test --test distributions               # one integration test file
uvx ruff check analysis scripts               # the Python, with the rules pyproject.toml pins

# every committed CSV and chart that needs nothing outside the repository, regenerated in place:
# afterwards `git status --short` is empty, or a committed number has moved
scripts/regenerate.sh
cargo test the_same_seed_produces_the_same_run -- --exact --nocapture

cargo run -- run --scenario scenarios/minimal.json --seed 42 --out /tmp/a
cargo run -- run --scenario scenarios/baseline.json --seed 42 --out /tmp/a   # the real corridor

# determinism check: same scenario + seed + cache must be byte-identical
cargo run -- run --scenario scenarios/baseline.json --seed 42 --out /tmp/b
diff /tmp/a/events.csv /tmp/b/events.csv

# the committed sweep and the committed charts, both regenerated in place
cargo run --release -- sweep --config sweeps/lowflow.json --out analysis/results.csv
uv run analysis/charts.py analysis/results.csv

# the three fixed rules, side by side in one table
cargo run -- run --scenario scenarios/baseline.json --seed 42 --policy greedy
cargo run -- run --scenario scenarios/baseline.json --seed 42 --policy pairs
cargo run --release -- sweep --config sweeps/policies.json --out analysis/policies.csv

# a model in the loop, with no model behind it - offline, and what tests/llm.rs does
cargo run -- run --scenario scenarios/baseline.json --seed 42 --policy hybrid \
  --sidecar "python3 tests/fixtures/stub-sidecar.py second-line"

# the other two services the corridor can be asked about
cargo run -- run --scenario scenarios/fleet.json --seed 42       # door to door, no lines at all
cargo run -- run --scenario scenarios/network.json --seed 42     # a constructed line set
cargo run --release -- sweep --config sweeps/networks.json --out analysis/networks.csv
uv run analysis/charts.py analysis/networks.csv                  # any sweep CSV, not just results

# the walking threshold, which is what actually caps the service
cargo run --release -- sweep --config sweeps/walking.json --out analysis/walking.csv

# a registered driver that stops only when asked, against one that waits at the station
cargo run --release -- sweep --config sweeps/approach.json --out analysis/approach.csv

# the re-runs on the second study area: every lyon-* sweep, then the agreement table
python3 scripts/build-lyon-scenarios.py /tmp/lyon --grid          # the agent-list scenarios
cargo run -- import /tmp/lyon/fleet-network-100.json --out /tmp/x.json --variant as-ran \
  --road-detour-factor 1.37                                        # one translated
cargo run --release -- sweep --config sweeps/lyon-fleet-network-100.json \
  --out analysis/lyon-fleet-network-100.csv
uv run analysis/agreement.py                                       # analysis/agreement.csv
uv run analysis/timeseries.py lyon-100 "network=/tmp/n/events.csv" # time series off a log
```

```bash
# the commands that touch the network, all run by hand, all with committed output
uv run scripts/fetch-data.py                  # data/flows.csv and data/stations.json
cargo run -- build-cache                      # data/routes.json, over the stations scenarios name
uv run scripts/fetch-lyon.py                  # data/lyon/flows.csv and data/lyon/stations.json

# the one that needs the original simulator checked out, outside this repository
uv run --group reference scripts/reference/run.py /tmp/lyon/fleet-network-100.json \
  --lane-engine DIR --fleet-engine DIR --seeds 1-10 --road-detour-factor 1.37 \
  --out analysis/reference/lyon.csv

# and the two that need an API key. Budget before running: ~1,300 calls, ~$11 on claude-opus-5
export ANTHROPIC_API_KEY=...
cargo run --release -- sweep --config sweeps/model-policies.json --out analysis/model-policies.csv
uv run analysis/charts.py analysis/model-policies.csv    # six figures: four mobility, two cost
uv run scripts/design-network.py              # scenarios/network-designed.json + a sweep point
```

**Everything else in this file is offline and needs no key**, which is deliberate: a reader who has
cloned the repository can check every committed number without one. `tests/study_area.rs` asserts
the Rust half of that - no `.rs` file outside `routing.rs` may name a fetch host, and only
`routing.rs` (whose `build-cache` shells out to `curl`) and `llm.rs` (which spawns the sidecar) may
spawn a process at all. The sidecar is the one place a run can reach a network, it only does so
under `--policy llm|hybrid`, and the suite always points it at a stub.

Tests live in `tests/` as integration tests against the public API - there are no `#[cfg(test)]`
modules in `src/`. Debug builds assert the physical invariants every tick, so a logic regression
fails a plain `cargo run` rather than surfacing as a quietly wrong number several steps later.

## The study area

Every committed scenario but `minimal.json` and `scenarios/lyon/` stands on **north Ceredigion into
Aberystwyth**: five
middle-layer super output areas, 44,485 residents, one town that takes the work, seven boarding
points across 23 km of the A487 and A44. `data/README.md` is the authority on where the data came
from, under what licence, and by which rule each station and each corridor area was selected -
**read it before quoting any figure from a run**, and change it in the same commit as
`scripts/fetch-data.py`, which is its executable half.

Three things about it that are easy to get wrong later:

- **The demand table is 2011, not 2021, for a licensing reason and not a preference.** The 2021
  origin-destination tables are safeguarded at this resolution and cannot be committed; the open
  2021 release is one cell per local authority. WU03EW is the most recent openly licensed
  area-to-area table with a **mode breakdown**, which is the whole point of the source: it separates
  *driving a car or van* from *passenger in a car or van*, so the corridor arrives with an observed
  car-passenger share of 9.95% of car commutes. That number is the one external check on whether a
  modelled `mean_occupancy` is plausible - **never a target to tune against**, which would make the
  model circular.
- **Two numbers in the corridor are invented, and both are stated out loud.** A scenario samples
  **one in twenty** of the observed flow on both sides, and enrols **one car driver in ten** - the
  corridor's own ratio is nine solo drivers to one car passenger, so enrolling every driver is a
  service with no scarcity in it and nothing to study. `sweeps/*.json` move that adoption from a
  twentieth to all of it, which is 6 to 132 vehicles. Everything downstream is a ratio against those
  two numbers.
- **`max_walk_s` is 2400 s on the corridor's rider cohorts, and that is a finding with a sweep behind
  it.** It prices both flanking legs - twenty minutes at each end - and the boarding points sit up to
  two kilometres from where the population actually is. `sweeps/walking.json` moves it against driver
  adoption and `analysis/walking.csv` is its 150 rows: 0.744 at the engine's 1800 s default against
  0.853 at 2400 s at thirteen drivers (ten seeds out of ten), and at **1200 s the service stops
  responding to fleet size at all** - 0.278, 0.381, 0.375 across 6, 13 and 32 vehicles, because a
  rider with no line inside its own threshold never registers and no amount of supply reaches it. The
  constraint saturates by 3000 s. An earlier version of this file claimed 1800 s cost 26 of 32 riders
  and a 19% service rate; that was stale long before anyone noticed, and the sweep replaces it.

`src/data.rs` reads both files (`Flows`, `Stations`) so a test and `network.rs`'s flow-ranked
construction parse them one way. Cohort origins are the areas' population-weighted centroids with a jitter radius -
3 km for drivers, 1.5 km for riders, because a driver's spread is the whole area and a rider's is
the village it walks from. The corridor scenarios set `road_detour_factor` to **1.19**, measured on
their own cache (626.0 km of road over 527.8 km of straight line); the 1.3 default was measured on
the synthetic corridor and stays, so `minimal.json` is untouched.

## The second study area, and the re-runs

**The Rhône, and the lane from Bourgoin-Jallieu into Lyon**, where both earlier studies were run.
`data/lyon/README.md` is its authority: INSEE MOBPRO 2021 for the demand (the thousand largest
motorised commuting pairs between Rhône places, 5 to 50 km), the 2021 administrative contours for
the centroids, two OpenStreetMap carpool stops, and a by-hand comparison against the scenarios the
published runs used. **The demand is the published method on the published source, not the
published numbers** - the published pairs do not reproduce, and the README says by how much. Its
road detour factor is **1.37**, measured on `data/lyon/routes.json`.

`scripts/build-lyon-scenarios.py` writes every scenario the studies ran in the *agent-list format*
their simulator reads; `cargo run -- import` translates one into this engine's own, in two variants.
**`as-ran`** reproduces the original simulator: a 30 s tick, walking at 4 m/s, a declaring rider that
never times itself to its driver, no departure guarantee (the original created the vehicle and never
put it in the world), a fleet vehicle one seat short, a lane rider with no patience, and the fleet
assigned by `urgency-as-ran`, which loses assignments exactly as the original did. **`corrected`**
keeps the model and drops the bugs. `scenarios/lyon/` and the `lyon-*` sweeps carry both, the
variant as an axis wherever the two differ only in patchable fields.

The original simulator itself is run by `scripts/reference/`, headless and offline, with every
change to it a patch listed in `harness.PATCHES` and the one speed-up checked byte for byte against
the original. It needs a checkout of each version of that simulator and is never run by the suite;
its output is committed under `analysis/reference/`. `analysis/agreement.py` holds the two engines
against each other by rules fixed before either was run: this engine's `as-ran` mean over ten seeds
inside the original's seed range, and every between-scenario difference with the same sign.
**Disagreements are listed, not tuned away** - but a disagreement that turns out to be a behaviour
this engine did not have is a missing knob, and several of the knobs the importer sets
(`takes_unregistered_riders`, `walks_unclaimed`, `urgency-as-ran`) exist because of exactly that.
Every such knob is off by default and moves nothing committed.

## The policy seam

`policy.rs` holds **four decisions**, all of them the operator's rather than an agent's: which line
a driver takes, whether to spend a guarantee vehicle on a rider that has waited too long, which idle
taxi collects which waiting rider, and where an idle taxi waits. Seven implementations -
`heuristic`, `greedy`, `pairs`, `urgency`, `urgency-as-ran`, `llm`, `hybrid`; the two urgency rules
are `heuristic` everywhere but the fleet assignment, `urgency-as-ran` only for reproducing the
original simulator's fleet runs. `Sim` owns one for the whole run, built from the scenario's
`"policy"` field; `agents::decide` threads it to the driver machine for the line choice, and the
other three are made in `sim.rs` because they never pass through an agent at all.

**Three of the four have a trait default, and each default is what the engine did before the seam
widened** - dispatch the guarantee, assign greedily, never reposition. That is what keeps
`heuristic` and `greedy` reproducing every committed figure byte for byte while `llm` and `hybrid`
override all four, and it is why adding a fifth decision means adding a default too.

`decide` returns `Result` and takes `&mut self` for the same reason: a policy may hold a pipe to a
model. Neither fixed rule keeps state or draws randomness, so under those a run stays a pure
function of its scenario and seed; under a model it is not, and that is what k seeds and a reported
band are for.

**Every context is audited for leakage, and the audit is a committed list** - `policy::LEAKAGE_AUDIT`,
asserted field by field by `tests/llm.rs`, so a field added to any context fails the suite until
somebody has written it down and looked at it. `options[].best_walk_share` is the most recent
addition and shows the mechanism working: it arrived with `pairs`, and the suite stayed red until it
was in the list. Nothing a context carries is unknowable to the
deciding party at decision time: no future spawn, no undeclared request, no realised demand, no
metric value, no objective function. A policy that can see any of those beats every real dispatcher
and the comparison is worth nothing. A policy chooses between options the agent would accept; it
never overrules the agent's own refusal, which is what `DecisionContext::accepts` is for.

**The wall clock is in `DecisionContext` and never on the wire.** It is knowable, so it belongs in
the audited context - but a question carrying it is a different question every tick and could never
be memoised, and a decision that cannot be memoised is a call per agent per tick. Nothing in these
decisions needs it: the waits and the demand are already relative.

`heuristic` is what every driver did before the seam: the first line the operator's ranking offers
that the detour bound allows. **It is the rule every committed figure and every row of
`analysis/results.csv` was produced under, so it has to keep reproducing them byte for byte** -
`tests/policy.rs` pins `baseline.json` at seed 42 with the cache loaded: 25 of 32 carried in 9 of 13
vehicles over 187.8378 km. `tests/declaration.rs` pins the same run's tick count and kilometres, and
`minimal.json` alongside it. `greedy` is the second fixed rule
and the one that removes the shared yardstick: it never reads `LineMatch::cost_s`, and takes the
least-detour line with riders on it, falling back to the least-detour line of all.

**`pairs` is the third, and the only rule here that drops the line as the match key.** A rider still
registers on the line it ranked first and still starts walking there; it simply stops being that
line's property. Any driver may take it with `Influence::Claim` - the mechanism advance declaration
already had - provided the walk it would *still* have to make, from wherever it has got to, is inside
the `max_walk_s` it declared, and `rider::decide_carpool` re-targets the walk when the answer moves.
`Operator::divertible_to` is the whole query, and it is the one place in that file that scans the
arena rather than the registry, because the line a rider registered on is the thing it is not
interested in. The score is a pair score literally: the driver's detour as a fraction of driving
straight there, plus the rider's remaining walk as a fraction of its own threshold, added one to one,
so neither party reads the other's unit and neither threshold is overruled. It cost one new field on
`LineOption` (`best_walk_share`, `None` under every rule that keys on the line) and therefore one new
line in `LEAKAGE_AUDIT`.

**Three ceilings, all deliberate and each marked `ponytail:` in the source.** It is greedy per driver with no
view of the fleet, so two can reach for the same rider. A rider is diverted **at most once and only
while it is still walking** - `Agent::awaits_pair_match` is what says so, and the reason is
`state_since_s`: that field is what `max_wait_s` is measured against, so a claim that re-enters
`RideToDepartureStation` from a station hands the rider a fresh half hour and quietly inflates every
service figure. Do not widen that predicate without fixing the wait first. And the two halves of the
score are weighed one to one, which is a rate of exchange nothing here fixes.

The choice is re-asked **every tick a driver spends at home**, because demand moves while it waits.
`heuristic` reads nothing that moves, answers the same line every tick and so registers exactly
once; `greedy` switches, and the apply pass unregisters the old line before registering the new one.
A driver that has claimed a rider stops reconsidering - the claim told that rider which station to
walk to - and a debug invariant asserts a claimed rider is on its driver's line. A guarantee
vehicle is handed `None` instead of a policy for *this* decision: it was called for one line and has
no alternative to weigh. Whether it was called at all is the policy's own separate decision.

`Operator::line_options` is the one query behind all of it, and the demand it reports is riders
standing at a line's origin station **plus** riders that declared ahead and are still in its pool
and that this driver could claim. Both halves, or a declaring cohort reads as no demand at all and a
demand-following driver drives away from it. It carries no direction filter, unlike
`Operator::riders_for`: that filter asks whether a rider lies ahead of where the driver is *now*,
which is the wrong question for a driver at home.

**The first two rules look like they cross over and they do not - that reading was wrong and is corrected
in `FINDINGS.md`.** Compared as means, greedy is ahead where vehicles are scarce (0.578 against
0.550 at six drivers, 0.859 against 0.853 at thirteen) and behind above that (0.912 against 0.950 at
thirty-two). But `sweeps/policies.json` runs the same ten seeds at every point, so the two can be
**differenced per seed**, and paired that way the low-end lead is noise: +0.028 with a standard
deviation of 0.132 at six drivers, 5 seeds up and 3 down, and +0.006 with sd 0.069 at thirteen, 4 up
and 5 down. What survives is the cost. Greedy's empty distance share is worse **ten seeds out of ten
at every driver count**, and at thirty-two drivers it drives 234 extra kilometres - every seed - for
0.037 *less* service.

Two consequences, and both matter when writing anything up. Greedy is a **baseline that loses**, not
half of a trade-off: keep it, because it removes the shared yardstick and that is what it is for, but
do not describe it as winning anywhere, and `pairs` dominates it - more service at 6, 13 and 32
drivers, shorter waits at every driver count, and from thirty-two vehicles up at the same distance or
less. And **`hybrid`'s exception is a choice, not a measured crossover** - "ask the model where the two fixed rules disagree" is still a defensible place to spend
a call, but the evidence that used to justify it does not exist. Say that wherever a `hybrid` figure
is quoted. The honest reading of both rules is unchanged and is the one to keep: neither is short of
a mechanism the other has, both are short of pair scoring - which `pairs` now supplies, and which
beats `heuristic` on service **+0.075 at six vehicles (7 seeds up of 10) and +0.053 at thirteen (8 of
10)** while losing to it on empty distance ten seeds out of ten. The bar a model's line choice has to
clear is therefore **0.906 at thirteen drivers and 0.950 at thirty-two**, not `heuristic`'s 0.853;
quoting the weaker opponent is how a model result stops being one.

**Prefer paired differences to overlapping bands.** Every sweep runs the same seed range at every
axis point, so any two points can be differenced per seed and reported as a mean, a standard
deviation and a win/tie/loss count. That is not a significance test, but it separates "the means
differ and the seeds disagree about the sign" from "every seed moved the same way", and those two are
the whole difference between a finding and a coincidence. Two conclusions in this repository already
changed direction under it.

## The model in the loop

`llm.rs` spawns **one long-lived child process** and speaks line-delimited JSON over its stdin and
stdout, the way `build-cache` shells out to `curl`. The Anthropic SDK stays in Python
(`scripts/sidecar.py`), so the engine has no HTTP client, no async runtime and no TLS stack, and the
sidecar is a command in the scenario (`"sidecar"`, or `--sidecar`) so a test points it at
`tests/fixtures/stub-sidecar.py` instead. The process is spawned **lazily, on the first decision** -
that is what makes "`heuristic` makes zero calls" a fact about the process table rather than a claim
about a counter, and `tests/llm.rs` gives the fixed rules a sidecar command of `exit 1` to prove it.
**The action space and its JSON Schema live in `policy.rs` and the schema rides every request**, so
the shape the model may answer in is defined once, beside the code that validates it; do not copy a
schema into the sidecar.

**Nothing costs a call per tick and nothing costs a call per agent per tick.** This is the
constraint the whole design serves, and it is not aspirational: on `baseline.json` at thirteen
drivers, `llm` answers **3,913 decisions in 44 calls** and `hybrid` **2,239 in 22**. Two mechanisms
do it, and breaking either turns a policy into a bill.

- **The memo.** Every answer is cached on the exact bytes of the request that produced it. It is
  only sound because no context carries the clock, and because floats are rounded to four decimals
  (about a metre of latitude) so "the same situation" means the same situation.
- **Event-driven triggers.** `taxi::assign_context` returns `None` unless there is a pair to make;
  repositioning is skipped entirely unless `Policy::repositions()` and only asks about **parked**
  taxis, so a vehicle on its way somewhere is never re-asked; and a *declined* guarantee starts the
  cooldown exactly as a dispatch does, so a standing trigger is not a question every tick.

`llm` asks about every line choice. **`hybrid` asks only where `heuristic` and `greedy` disagree** -
the exception architecture, and the exception is grounded in this repository's own measurement rather
than invented. Where the two rules agree there is nothing for a model to add and it is not asked; on
`minimal.json` that is every decision, so `hybrid` there makes no calls and starts no process. If
you change what counts as the exception, say so wherever a `hybrid` figure is quoted: a different
exception asks about different situations and can come back differently.

**An invalid action is counted and refused, never retried.** Refused means the decision falls through
to the rule that would have made it - the heuristic's line, the guarantee's yes, the greedy pairing,
no movement. `invalid_action_rate` is **per decision, not per call**: a refused answer is memoised
like any other (re-asking *is* the retry this refuses to do), so every later decision that reads it
really was made by the fallback, and the column is how many of the operator's decisions that was. A
high rate means much of a run was the fallback wearing the model's name - which is the number worth
having, and precisely what a retry loop would hide. The refusal warns once per run and counts every
time.

**A dead sidecar fails the run.** A process that will not spawn, a closed pipe, a line that is not
JSON, an `{"error": …}` from the sidecar - every one ends the run with the sidecar named. Never add a
fallback here: finishing under a fixed rule would publish that rule's numbers under the model's name,
and it is the one failure a reader of the results could never detect.

**Cost is a column.** `model_decisions`, `model_calls`, `usd_per_decision`, `tokens_per_decision`,
`decision_latency_ms`, `invalid_action_rate` - in the same row as `service_rate`, so a sweep produces
the mobility delta and its price in one table. Latency is measured on the engine's side of the pipe,
so it includes the round trip. Prices are a table in the sidecar, honest about being a table, and an
unknown model reports zero rather than a guess. `charts.py` charts `usd_per_decision` and
`invalid_action_rate` beside the four mobility figures and **skips a column that is zero on every
row**, so the four fixed-rule sweeps still write exactly the four SVGs they always did. Scoring
stays with the engine: `metrics.rs` computes every number, and **there is no model judging its own
work anywhere in this repository** - there is a deterministic ground truth, so it gets used.

**The Batch API does not apply in the loop and the write-up must not pretend it does.** In-loop
decisions are sequential by construction: each action changes the trajectory the next is made from.
Batch covers the offline network design and nothing else; throughput across a sweep comes from the
sweep's own `rayon` runs each holding their own sidecar, which is concurrency, not batching.

**The network design is the one call not in the loop.** `scripts/design-network.py` hands the model
the corridor's directed area-to-area car commutes and its seven boarding points, asks for a line set,
validates it against the rules the engine would apply at load, and writes
`scenarios/network-designed.json` plus a fifth `designed` point on the axis `sweeps/networks.json`
already has. That point sets `/environment/construction` to `null` and writes
`/environment/networks`, which needed no engine change: `construction` is an `Option` and declaring
both is already a load error. The model is told nothing about the four constructions or their
scores - being told the answer is how this stops being a result. **The bar is 0.853, the
hand-declared radial network, not flow-ranked's 0.750.**

**No model figure is committed yet, and none is quoted anywhere.** Every call count above came from
the stub, which answers from a rule: it measures the machinery, not a model.
`analysis/model-policies.csv`, `scenarios/network-designed.json` and the `designed` sweep point each
need one command with a key - see *Commands*. Budget before running: 60 runs,
30 of which reach the model (`pairs` is in that sweep too and runs offline, like `heuristic`),
~1,300 calls, ~$11 on `claude-opus-5` at `--effort low`. Five seeds
rather than ten because the model is not deterministic and each seed is money.

## The fleet, and the network constructions

Two services stand beside carpooling, and **both came back worse than it on this corridor**. Say
that plainly wherever either is written up: the interesting result is not that the new thing won.

**The fleet.** An `AutonomousTaxi` has no trip of its own, so every empty kilometre is a cost the
service pays rather than a trip somebody was making anyway - that one difference is what makes the
fleet case read differently in every metric. Service is door to door: an `AutonomousTaxiRider` is
collected where it is standing and set down at its destination, so `fleet.json` declares **no lines
at all** and validation only requires lines of scenarios with a non-fleet cohort in them. Eight
four-seat vehicles serve 24 of the corridor's 32 riders over 335.0 km, 56% empty, occupancy **0.44**;
`baseline.json`'s thirteen carpool drivers serve 25 of the same 32 over **183.1** km at occupancy
**1.35**. Adding vehicles saturates at 0.875 - the riders it never reaches are 23 km out and give up
first. **Every fleet leg is a straight-line fallback** (door-to-door coordinates never hit the
station-pair cache), so fleet distance is understated by the detour factor; that cuts in the
direction that strengthens the conclusion, which is why it is a caveat and not a blocker.

`agents/taxi.rs` holds both machines and builds the operator's view. **The operator is
`taxi::assign_context` plus `Policy::assign_fleet`, and it holds no registry** - the arena already
says which taxis are idle and which riders are waiting, and the pairing goes on `Agent::claimed_by`,
the field a carpool advance claim already uses. It writes straight into the arena rather than
emitting an influence, for the same reason `Sim::dispatch_guarantees` does: the operator *is* the
authority, so there is nothing to arbitrate. A policy names a pair by its **position in the two
lists it was handed**, never by an arena slot, so validation happens against a list rather than
against the world.

**Assignment is event-driven and that is what keeps a model in the loop affordable**: two filters, an
early return if either side is empty, and if neither is there is at least one pair to make - so the
O(riders × taxis) pricing, or a model call, runs exactly on the ticks where demand or idleness
changed. `Sim::fleet_assignments` counts those and a test pins it at three across thousands of ticks.
The greedy pricing itself now lives in `policy::greedy_pairs`, with the other baselines, and is the
trait's default.

**There is no repositioning heuristic**, deliberately: under either fixed rule an idle taxi stands
where it was released, so a model asked where one should wait competes against doing nothing, and any
write-up has to say so first. `Sim::reposition_fleet` writes the target into `Agent::station` - a
field a fleet taxi never otherwise uses - and the taxi's own machine drives there **while staying
idle**, so an assignment can still interrupt it and `station: None` on that assignment's `Travel` is
what clears the target. **One drop-off per trip** unless the taxi sets `chained_drop_offs` (pooling is
fifty metres at the pickup and a kilometre at the drop either way). `Sim::stand_down_fleet` releases
idle taxis once no fleet rider is left, in the arena or in the spawn queue - without it a taxi is
active for ever and every fleet run is a cut-off run.

**The constructions.** `network.rs` turns a station set into undirected pairs, each becoming two
directed lines, and the derivation happens in `Scenario::from_json` so that **nothing downstream
knows which construction it was given** - by the time the world is built, a derived line set and a
hand-declared one are the same lines, `build-cache` is unchanged, and a construction is a one-pointer
sweep axis (`/environment/construction`). Free-form is all 21 corridor pairs, Delaunay 14,
flow-ranked 13, the minimum spanning tree 6. Only **flow-ranked** reads the demand - `environment.flows`
and the stations' `area_code` - and that asymmetry is the point of the comparison.

`analysis/networks.csv` is the answer and it is negative. Sparser is worse everywhere above six
vehicles: at thirteen drivers free-form 0.744 and flow-ranked 0.750 against Delaunay 0.619 and the
tree 0.631, and the gap *widens* to 0.950 against 0.725 at 132. Two sharper things: reading the
demand's **size** buys almost nothing over connecting everything (flow-ranked and free-form agree
within 0.006 everywhere, so the network halves for free but does not quarter), and **the
hand-declared radial network beats all four** wherever vehicles are scarce - 0.853 against 0.750 at
thirteen drivers. The mechanism is **direction**: every construction is symmetric, so half its lines
run away from a radial morning commute. That is the bar `scripts/design-network.py` puts to the
model, and it is 0.853, not flow-ranked's 0.750.

## The four decisions that shape everything

**Determinism is the point.** A run is a pure function of its scenario, its seed and the committed
route cache. Concretely, and each of these is easy to break by accident:

- One `ChaCha8Rng` owned by `Sim`, seeded from `--seed`. Never a thread-local generator, never
  `StdRng` (its output is not stable across releases).
- Every draw happens for every agent in a fixed order at construction, in `sim::new_agent`.
  `Value::Fixed` deliberately consumes **no** randomness, which is what keeps a bare-number
  scenario byte-identical to the run it produced before distributions existed. Adding a draw in
  the middle of that order shifts every later draw.
- Registries are `Vec`s in arrival order, not hash maps, so matching never depends on hash
  iteration order. Sorts are stable, on `total_cmp`.
- The event log writes at fixed precision; the cache file is written sorted.

**Arena with typed indices, never `Rc<RefCell<_>>`.** Agents live in `Vec<Agent>` on `Sim` and
refer to each other as `AgentId(u32)`, which is the slot. Nothing is ever removed, so a slot stays
valid for the whole run. Same for `StationId`, `LineId`.

**Influence–reaction cycle.** Each tick is `perceive → decide → apply`. `agents::decide` is a pure
function of an agent, the world and a `Context`; it mutates nothing and returns an `Influence`.
The apply pass in `sim.rs` resolves it. The one thing a decision may mutate is the `Policy`, which
is why that is a separate `&mut` parameter rather than a field of `Context` - and why `decide`
returns `Result`: a policy may hold a pipe, and a pipe that has closed must end the run rather than
quietly becoming a different policy. **No agent ever writes into another** - a driver cannot
seat a rider, it emits `Influence::Pickup` and the apply pass re-checks that the rider is still
waiting and the seats still fit, so two drivers reaching for the same rider on one tick means the
second gets nothing and tries again. This is what let the line choice move behind `policy::Policy`
without touching a state machine, so keep new behaviour inside `decide` and new mutation inside
`apply`.

**`State` is separate from `TransitionReason`.** `State` is the finite-state machine; the reason
records *why* a transition fired and only the analysis depends on it. Every `match` on `State` is
exhaustive - no `_` arm - so a new variant is a compile error in every machine that has not been
taught about it. Rider and driver states share one enum, and reaching the other family's state is
`unreachable!`. The one `_` arm allowed is a ghost handing its shared tail to the registered
machine (`_ => decide_carpool(..)`, `_ => registered(..)`): that is a delegation, not a default,
and the machine it delegates to is still exhaustive.

## How a match actually happens

`Operator` is a registry, not an agent: no state machine, no position. `ranked_lines` returns
every line quickest first - the walk to its origin station, the line itself at the routing
server's own duration, and the leg from its destination station onwards. It **ranks and never
refuses**; the refusal is the agent's own (`max_walk_s`), which is why one rider walks twenty
minutes and another five.

Both flanking legs are priced on foot **for drivers as well as riders**, and the two speeds are
drawn once per run rather than per agent. That is a shared yardstick, not an estimate: a pickup
needs both parties registered on the same `LineId`, so the cost function is the only thing making
the two sides name the same station. That is now a property of the `heuristic` policy alone -
`greedy` never reads the ranking's `cost_s` - but `heuristic` is the rule the committed sweep was
run under, so making the driver's number more accurate *changes* its results rather than fixing
them. Per-agent speeds and a driver-priced approach belong in a policy, not in the ranking. A
driver's `cost_s` is not a travel time and nothing may read it as one - the drive itself is routed
at `Profile::Road` in the apply pass, so kilometres and clock time stay honest.

A cohort that declares ahead is out of that arrangement already. With
`advanced_declaration_lead_s` above zero a rider announces its trip before setting off, sits in the
pool of **every** line inside its `max_walk_s`, and walks only once a driver has taken it with
`Influence::Claim` - to that driver's station, first choice or not. The rendezvous is an
intersection rather than a number both sides must compute the same way. The claim is mutual (the
driver needs a lead of its own) and bounded by the two `DepartureWindow`s overlapping; it reserves
no seat, so a driver out of patience leaves without a rider still walking in. A driver's own lead
never moves when it registers - a seat is visible as soon as the vehicle exists.

One machine in `driver.rs` serves nearly every driver, and the kinds differ by configuration rather
than by a branch. An `OpportunisticDriver` cannot have a declaration lead, so the advance claim
disables itself. **What a ghost withholds is the registration, not the ranking** - the operator's
ranking is public, so a `GhostDriver` and a `GhostRider` read it and take a line like anyone else,
and `AgentKind::is_registered` is what makes the apply pass skip the registry for them. The cost is
that no query the operator answers can offer them anything: a ghost driver finds riders by scanning
its own station (so it picks up registered riders too), and a ghost rider can only be found by a
driver standing there. That is why `GhostRider` is the same code as `CarpoolRider`. A
`NeglectfulDriver` is the one with its own machine: it registers, so the operator counts a seat, and
then drives the trip it was making anyway without going near a station.

`max_detour_pct` is **the driver's refusal**, the counterpart of a rider's `max_walk_s`, and it is
measured in kilometres - never on `LineMatch::cost_s`, which prices both flanking legs on foot for
both parties on purpose and is not a travel time. It reaches the policy in the `DecisionContext`
and no policy may overrule it. A driver that refuses every line drives its own trip and carries
nobody. There is no default, so scenarios written before it are untouched; on
`minimal.json` the dogleg is about 150% of driving direct with the cache loaded, so a fixed 110% would
refuse the toy corridor outright. No corridor scenario sets it - the stations
sit on the roads people already drive, so the doglegs there are milder.

The departure guarantee is the operator's vehicle, not a cohort: `Sim::dispatch_guarantees` puts a
`TaxiDriver` in the arena, already registered on the line, when a rider on that line has waited past
`trigger_after_wait_s`. It takes no random draw, which is what keeps every scenario without a
guarantee byte-identical. One vehicle at a time per line and then `cooldown_s` - the first of those
is why a zero cooldown does not dispatch one every tick. It has no trip of its own, so a vehicle
that reaches the station and finds nobody stands down there rather than driving the line empty; that
is `decide_guarantee`, which reads the shared machine's decision instead of duplicating it.

`Router::route` is the only way to get geometry. A hit on the committed cache returns real road
geometry and the server's own duration; a miss returns the straight line, which gets the *time*
right and **understates the distance** by the detour factor. The cache covers station-to-station
pairs only - agent origins are jittered per seed, so approach legs would never hit an entry
however large the file grew. The legs that carry passengers are the cached ones, so
`vehicle_km_loaded` is a road distance.

`metrics.rs` derives every number from the agents at the end of the run, never accumulated by the
tick loop, so the metrics cannot drift from what the event log says happened. It is also
where the incentive is applied: `CompletedTrip::collect` prices a *finished* arena, one trip per
driver that carried somebody, and **nothing in a run ever reads a reward back**. A trip carries the
driver and its passenger-kilometres and nothing else - widen it with the scheme that prices the new
field, never ahead of it. Keep it that way until a behavioural response to the incentive is built on
purpose - the moment a driver responds to a payout, no sweep over any other
axis is comparable across schemes any more, and `tests/incentives.rs` says so. A run cut off by the
clock cap carries `stopped_by_clock` and a nonzero `stranded`, and both are fields rather than a
footnote because a cut-off run's averages are not a finding.

The fleet's three columns - `taxi_km_empty_to_pickup`, `mean_time_to_pickup_s`,
`fleet_utilisation` - are zero on every row of a scenario with no fleet in it, and they split the
two services by *who did the driving* (`carried_by`'s kind) rather than by the rider's own, so a
mixed scenario does not average them together. The first is deliberately not the
`taxi_km_empty_repositioning` one might expect: the deadhead to a pickup is the number that
exists whether or not anything repositions, and repositioning lands **in the same column** rather
than one of its own - it is the same empty kilometre however it was spent, and splitting it would let
a policy look cheap on one column while spending on the other. The column a repositioning policy has
to earn that distance back on is `mean_time_to_pickup_s`.

The model's six - `model_decisions`, `model_calls`, `usd_per_decision`, `tokens_per_decision`,
`decision_latency_ms`, `invalid_action_rate` - are zero on every row of a run under a fixed rule, and
reach `Metrics` through `RunSummary::decisions` off `Policy::cost()`. **Adding a field to `Metrics`
changes every committed sweep CSV's header**, so regenerate all four offline sweeps and their charts
in the same commit.

## The sweep

`sweep --config sweeps/lowflow.json` patches a base scenario per axis point, runs the cross product
of the axes against a seed range under `rayon`, and writes one tidy row per run: scenario, the axis
values, the seed, then every field of `Metrics`, whose names it reads off the serialised struct
rather than a second list to keep in step. An axis point is a **set** of JSON pointers into the
scenario and what to put there, not one field, because the interesting knobs rarely are one field:
moving declaration adoption means moving two cohort counts at once, and a lead without its margins
is a load error. **A pointer that does not resolve stops the sweep** - a patch that quietly does
nothing runs a different experiment than the config describes, and no column downstream could tell.

Runs are independent by construction, so `rayon` decides who runs where and never what comes out;
rows are collected in the cross product's own order and repeated sweeps are byte-identical. The
incentive scheme is fixed for the whole sweep and cannot be an axis: no behaviour responds to a
payout, so a scheme that moved between rows would move two columns and explain nothing. The
**policy** is the opposite case - it is a scenario field precisely so `sweeps/policies.json` and
`sweeps/model-policies.json` can patch `/policy` and put every rule in one table. A base scenario a
config patches has to *name* the field: an absent key is a pointer that does not resolve, which is
why `baseline.json` says `"policy": "heuristic"` out loud even though that is the default. The
`sidecar` field is the same shape and for the same reason, though nothing sweeps it yet.

`sweeps/policies.json` and `sweeps/model-policies.json` are kept apart on purpose: the first is the
two fixed rules and regenerates offline, the second reaches a model and needs a key. Folding them
into one config would make `analysis/policies.csv` unreproducible without one.

The corridor makes the multi-pointer axis the normal case rather than the exception: `baseline.json`
has one driver cohort and one rider cohort **per feeding area**, so the driver axis sets four counts
per point, each the same fraction of that area's observed car-driver flow. The axis `value` is their
sum - 6, 13, 32, 66, 132 - because that is what reads on a chart.

`analysis/charts.py` reads **any** sweep CSV and writes the committed SVGs: the mean over seeds
with the seed-to-seed range as a band, because one seed of a stochastic model is not a finding. It
takes its axes from the file's own header - the axis columns are whatever the sweep wrote between
`scenario` and `seed`, the last is the x axis and the first is the series - and the series sort
falls back to text, so a `construction` or a `policy` column charts as readily as a fleet size.
Figures are named `<csv stem>-<metric>.svg`, so `results.csv`, `networks.csv` and `policies.csv`
each get their own four and none overwrites another. Two axes is still the ceiling. It is
matplotlib in `uv`'s environment (`uv run analysis/charts.py`), and `pyproject.toml` and `uv.lock`
are committed with it. **The charts are committed, so writing them has to be reproducible**:
matplotlib stamps a write time into an SVG and salts its element ids from the output path unless it
is told not to, so `svg.hashsalt` is pinned and `metadata={"Date": None}` drops the stamp. Without
both, regenerating a chart is a diff with no change in it. Commit `results.csv` and the charts so
the repository is readable without running anything.

## Scenario configuration

JSON in `scenarios/`, loaded and validated by `scenario.rs` with `deny_unknown_fields` - a typo
fails at load rather than silently taking a default and quietly changing the experiment. Validation
names the offender.

Every cohort knob is a `Value`: a bare number parses as `Fixed`, and
`{"weibull": {"scale": 1800, "shape": 2}}` makes the cohort a population. Rider give-up comes out
of a Weibull `max_wait_s` with `shape > 1` rather than a per-tick hazard draw - no second path
through the state machine. Draws are clamped at the draw (durations and counts non-negative,
speeds to a floor), because a normal will eventually hand back a negative number and a zero speed
is a hang rather than an error.

`policy` and `sidecar` are the two scenario fields that are not about the world: the rule a run is
made under, and the command a model policy is reached through. Both are fields rather than only flags
so the sweep can patch them; `--policy` and `--sidecar` override them for one run, patched *into* the
scenario so there is one place a run reads either from.

`destination` is optional and absent means exactly one thing: an `AutonomousTaxi`, which has
nowhere to be. Absent on anything else is a load error, and present on a fleet taxi is too - and
its draw is skipped rather than reordered, so every existing cohort consumes the randomness it
always did. `environment.networks` is optional as well, because `environment.construction` fills it
in at load; declaring both is a load error, since one of the two would be ignored.

The declaration knobs - `advanced_declaration_lead_s`, `earliness_margin_s`, `lateness_margin_s` -
are the exception and stay plain numbers: a draw there would shift every later draw and break the
byte-identity of every scenario that declares nothing. Margins without a lead, or a lead on a kind
that does not use the app, are load errors rather than knobs quietly ignored.

## Conventions

**No acronyms, anywhere, and the unit goes in the name**: `_s` for seconds, `_km`, `_pct`, `_mps`.
A reader should never have to guess whether a duration is seconds or minutes. Agent kind names are
fixed vocabulary - Private, Carpool, Ghost, Opportunistic, Neglectful, Taxi, Polynomial,
AutonomousTaxi - separated on three axes: registered with the operator, uses the app, willing to
pick up. `PrivateDriver` is the base every driver machine is built on (`driver::decide_private`):
it drives its own trip with its own party and is on none of the axes. Spawned alone it is the
private car, and its companion is its party (`seats`), never a rider, so it adds nothing to
`passengers_served` and all its kilometres are empty; counting it as a user, as a private-car
baseline does, is analysis over `drivers_total` and the event log rather than a `Metrics` field.
`TaxiDriver` and the two `AutonomousTaxi` kinds are off those axes: they are the operator's own,
dispatched or assigned rather than matched, and neither declares ahead because advance declaration
pools an agent on a *line*. A scenario declaring a `TaxiDriver` cohort is a load error; an
`AutonomousTaxi` cohort declaring a destination or a party of its own is too, because a fleet
vehicle has no trip and nobody rides with a robot.

**The glossary**, to be used verbatim in code and in prose:

- **Station** - a boarding point. **Line** - a directed origin station to destination station pair.
  **Network** - a set of lines run by one **operator**, which is a registry, not an agent.
- **Cohort** - a group of agents spawned from one scenario entry. **Kind** - its behaviour type.
- **Private driver** - the base every driver is built on: drives its own trip, registers with
  nobody, carries only its own party. Spawnable on its own as the private-car baseline.
- **Pre-formed group** - a `PolynomialDriver` and the `PolynomialRider`s spawned with it, who travel
  together anyway. They register as a match to collect the incentive, and are matched at the station
  **if and only if they are together**.
- **Advance declaration** - announcing a trip `advanced_declaration_lead_s` ahead of departing.
  **Departure window** - the declared departure widened by `earliness_margin_s` and
  `lateness_margin_s`. **Claim** - a driver taking a rider that has not reached a station.
- **Departure guarantee** - the operator's vehicle (`TaxiDriver`), dispatched once a rider has
  waited past `trigger_after_wait_s`, then `cooldown_s` before the next on that line.
- **Fleet** - `AutonomousTaxi` vehicles, assigned by the operator, door to door or, for a
  `service: network` rider, one hop of the lines at a time.
- **Construction** - a rule deriving lines from stations: `free-form`, `delaunay`,
  `minimum-spanning-tree`, `flow-ranked`, `flow-directed`.
- **Policy** - the operator's four decisions: line choice, guarantee dispatch, fleet assignment,
  fleet repositioning.

**Metric columns**, exactly as `Metrics` serialises them: `agents_spawned`, `riders_total`,
`drivers_total`, `passengers_served`, `passengers_unserved`, `service_rate`, `drivers_active`,
`vehicle_km_total`, `vehicle_km_loaded`, `vehicle_km_empty`, `empty_distance_share`,
`passenger_km`, `mean_occupancy`, `mean_wait_s`, `mean_journey_time_s`, `taxi_km_empty_to_pickup`,
`mean_time_to_pickup_s`, `fleet_utilisation`, `incentive_paid`, `cost_per_passenger_served`,
`model_decisions`, `model_calls`, `usd_per_decision`, `tokens_per_decision`,
`decision_latency_ms`, `invalid_action_rate`, `stranded`, `stopped_by_clock`.

**Dependencies are added when they are first needed, and only then.** The Rust set is final:
`anyhow`, `clap`, `serde`, `serde_json`, `rand`, `rand_chacha`, `csv`, `rayon`.
`scripts/fetch-data.py` is standard-library Python - `urllib`, `csv`, `json`, `math` - including the
point-in-polygon test that attributes a stop to an area, so fetching the study area adds no
dependency either. The sweep config is JSON rather than TOML so its axis values are the same literals
as the scenario fields they patch, and so no parser crate is added for one file. Python's
dependencies are `matplotlib` for the charts and `anthropic` for the sidecar and the one design call,
both in `pyproject.toml`, and `uv run` is how they are invoked. Haversine is five lines, so no `geo`
crate; the distributions are three closed-form inverse-CDF expressions, so no `rand_distr`;
`build-cache` shells out to `curl` and the model lives behind a pipe, so **no HTTP client and no
async runtime in the engine at all**.

**Comments explain the decision, not the mechanics.** A deliberate simplification that cuts a real
corner is marked `ponytail:` and names both the ceiling and the upgrade path - grep for them to
find what was knowingly deferred. Prefer the shortest thing that works, and say what was skipped
and when to add it rather than building it speculatively.

A kind whose behaviour is not written yet still spawns, does nothing, and makes the run warn at
startup, so a cut-off or empty result is never mistaken for a finding.
