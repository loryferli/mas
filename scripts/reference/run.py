#!/usr/bin/env python3
"""The reference harness: run the reference engine over scenarios and seeds, one row per run.

    uv run --group reference scripts/reference/run.py SCENARIO.json [...] \\
        --lane-engine DIR --fleet-engine DIR --seeds 1-10 --out analysis/reference/NAME.csv

The reference engine is the Python simulator the earlier published runs were made with, in two
versions (see `harness.py`); neither is part of this repository, so each is passed as a checkout
(or `MAS_LANE_ENGINE` / `MAS_FLEET_ENGINE`). Every scenario is an agent-list file, as
`scripts/build-lyon-scenarios.py` writes them, and runs on the version it belongs to - carpool
scenarios on the lane engine, fleet and private-car scenarios on the fleet engine - headless and
offline. The output is this repository's sweep columns: `scenario`, `seed`, every field of
`Metrics`, then the notebooks' extras (`harness.EXTRA_COLUMNS`). Rows come out in (scenario, seed)
order whatever order the processes finish in.

The engine keeps its state in modules and in `random`, so every run is its own process. A run is a
pure function of scenario, seed, detour factor and tick; the engine's uuids are random but no metric
reads one.

    --naive-perception   the original all-pairs perception; with --events, the logs must match
    --events DIR         write each run's transitions to DIR/<scenario>-<seed>.csv
    --road-detour-factor the scenario's measured factor where there is one (default 1.3)
    --parent-column NAME a column NAME holding each scenario's directory name, for a grid whose
                         cells share file names across settings

What it patches and why is `harness.PATCHES`, printed by --patches.
"""

import argparse
import concurrent.futures
import csv
import multiprocessing
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import harness  # noqa: E402

METRIC_COLUMNS = [
    "agents_spawned", "riders_total", "drivers_total", "passengers_served", "passengers_unserved",
    "service_rate", "drivers_active", "vehicle_km_total", "vehicle_km_loaded", "vehicle_km_empty",
    "empty_distance_share", "passenger_km", "mean_occupancy", "mean_wait_s", "mean_journey_time_s",
    "taxi_km_empty_to_pickup", "mean_time_to_pickup_s", "fleet_utilisation", "incentive_paid",
    "cost_per_passenger_served", "model_decisions", "model_calls", "usd_per_decision",
    "tokens_per_decision", "decision_latency_ms", "invalid_action_rate", "stranded", "stopped_by_clock",
]

# Only the fleet engine has a fleet, and its base `Driver` is the private car. Everything else is the
# carpool model, which the lane engine ran.
FLEET_CLASSES = {"AutonomousTaxi", "AutonomousTaxiRider", "AutonomousTaxiOrchestrator", "Driver"}


def tree_for(scenario):
    import json

    agents = json.loads(Path(scenario).read_text())["simulation"]["start"].get("agents", [])
    return "fleet" if any(agent["class"] in FLEET_CLASSES for agent in agents) else "lane"


def seeds(text):
    first, _, last = text.partition("-")
    return list(range(int(first), int(last or first) + 1))


def one(job):
    scenario, seed, args = job
    events = None
    if args.events:
        events = Path(args.events) / f"{Path(scenario).stem}-{seed}.csv"
    tree = tree_for(scenario)
    row = harness.run(
        tree,
        args.engines[tree],
        scenario,
        seed,
        detour_factor=args.road_detour_factor,
        clock_cap_s=args.clock_cap_s,
        naive_perception=args.naive_perception,
        events=events,
    )
    parent = {args.parent_column: Path(scenario).parent.name} if args.parent_column else {}
    return {"scenario": Path(scenario).stem, **parent, "seed": seed, **row}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("scenarios", nargs="*")
    parser.add_argument("--seeds", type=seeds, default=seeds("1-10"))
    parser.add_argument("--out")
    parser.add_argument("--events")
    parser.add_argument("--road-detour-factor", type=float, default=harness.DEFAULT_ROAD_DETOUR_FACTOR)
    parser.add_argument("--clock-cap-s", type=float, default=harness.DEFAULT_CLOCK_CAP_S)
    parser.add_argument("--naive-perception", action="store_true")
    parser.add_argument("--jobs", type=int, default=None)
    parser.add_argument("--parent-column")
    parser.add_argument("--lane-engine", default=os.environ.get(harness.ENGINE_VARIABLES["lane"]))
    parser.add_argument("--fleet-engine", default=os.environ.get(harness.ENGINE_VARIABLES["fleet"]))
    parser.add_argument("--patches", action="store_true")
    args = parser.parse_args()

    if args.patches:
        print(*(f"- {patch}" for patch in harness.PATCHES), sep="\n")
        return
    if not args.scenarios or not args.out:
        parser.error("scenarios and --out are required")
    args.engines = {"lane": args.lane_engine, "fleet": args.fleet_engine}
    for tree in sorted({tree_for(scenario) for scenario in args.scenarios}):
        engine = args.engines[tree]
        if not engine or not Path(engine).is_dir():
            sys.exit(f"the {tree} engine is not checked out: pass --{tree}-engine or set "
                     f"{harness.ENGINE_VARIABLES[tree]}")
    if args.events:
        Path(args.events).mkdir(parents=True, exist_ok=True)

    jobs = [(scenario, seed, args) for scenario in args.scenarios for seed in args.seeds]
    context = multiprocessing.get_context("spawn")
    with concurrent.futures.ProcessPoolExecutor(args.jobs, mp_context=context, max_tasks_per_child=1) as pool:
        rows = list(pool.map(one, jobs))

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", newline="") as handle:
        axes = [args.parent_column] if args.parent_column else []
        writer = csv.DictWriter(handle, ["scenario", *axes, "seed", *METRIC_COLUMNS, *harness.EXTRA_COLUMNS])
        writer.writeheader()
        for row in rows:
            writer.writerow({key: format_value(value) for key, value in row.items()})
    print(f"{len(rows)} runs written to {args.out}")


def format_value(value):
    # Fixed precision, so a rerun is a byte-identical file, like the engine's sweep.
    return f"{value:.6f}" if isinstance(value, float) else value


if __name__ == "__main__":
    main()
