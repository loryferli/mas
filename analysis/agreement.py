#!/usr/bin/env python3
"""The agreement table: this engine's runs of the Lyon scenarios against the reference engine's.

    uv run analysis/agreement.py

Reads `analysis/reference/*.csv` (the reference engine, run by `scripts/reference/run.py`) and
`analysis/lyon-*.csv` (this engine, the `as-ran` and `corrected` variants), and writes
`analysis/agreement.csv`. Standard library only.

Two tests, both fixed before any run was looked at:

  * **level** - per scenario and metric, the mean of this engine's `as-ran` runs over its ten seeds
    lies inside the reference engine's range over its ten seeds;
  * **sign** - every between-scenario difference points the same way in both engines: the
    structured network against door to door at each sample, and on the lane, full advance
    declaration on both sides against none, cell by cell.

A disagreement is listed, never tuned away. The `corrected` column is beside every row so the
distance between the published model and the corrected one is read off the same table.
"""

import csv
import statistics
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
METRICS = [
    "service_rate",
    "mean_wait_s",
    "mean_journey_time_s",
    "mean_occupancy",
    "empty_distance_share",
    "vehicle_km_total",
    "passenger_km",
]


def rows(path):
    with open(path, newline="") as handle:
        return list(csv.DictReader(handle))


def by(rows_, *keys):
    grouped = defaultdict(list)
    for row in rows_:
        grouped[tuple(row[key] for key in keys)].append(row)
    return grouped


def values(rows_, metric):
    return [float(row[metric]) for row in rows_]


def sign(value, tolerance):
    return 0 if abs(value) <= tolerance else (1 if value > 0 else -1)


def main():
    out = []

    # --- the network samples -----------------------------------------------------------------
    reference = by(rows(HERE / "reference" / "lyon.csv"), "scenario")
    ours = {}
    for path in sorted(HERE.glob("lyon-*.csv")):
        if "lane" in path.name:
            continue
        grouped = by(rows(path), "variant")
        ours[path.stem.removeprefix("lyon-")] = grouped
    for scenario, variants in sorted(ours.items()):
        if (scenario,) not in reference:
            continue
        for metric in METRICS:
            theirs = values(reference[(scenario,)], metric)
            as_ran = statistics.mean(values(variants[("as-ran",)], metric))
            corrected = statistics.mean(values(variants[("corrected",)], metric))
            out.append({
                "test": "level", "scenario": scenario, "metric": metric,
                "reference_min": min(theirs), "reference_mean": statistics.mean(theirs),
                "reference_max": max(theirs), "as_ran_mean": as_ran, "corrected_mean": corrected,
                "agrees": min(theirs) <= as_ran <= max(theirs),
            })

    # The structured network against door to door, sample by sample.
    for sample in ["10", "100"]:
        network, door = f"fleet-network-{sample}", f"fleet-door-to-door-{sample}"
        if (network,) not in reference or (door,) not in reference or network not in ours:
            continue
        for metric in METRICS:
            theirs = statistics.mean(values(reference[(network,)], metric)) - statistics.mean(
                values(reference[(door,)], metric))
            as_ran = statistics.mean(values(ours[network][("as-ran",)], metric)) - statistics.mean(
                values(ours[door][("as-ran",)], metric))
            corrected = statistics.mean(values(ours[network][("corrected",)], metric)) - statistics.mean(
                values(ours[door][("corrected",)], metric))
            tolerance = 1e-9
            out.append({
                "test": "sign", "scenario": f"network - door-to-door at {sample}", "metric": metric,
                "reference_mean": theirs, "as_ran_mean": as_ran, "corrected_mean": corrected,
                "agrees": sign(theirs, tolerance) == sign(as_ran, tolerance),
            })

    # --- the lane ----------------------------------------------------------------------------
    grid_path = HERE / "reference" / "lyon-lane-grid.csv"
    if grid_path.exists():
        theirs = by(rows(grid_path), "setting", "scenario")
        as_ran = by(rows(HERE / "lyon-lane-as-ran.csv"), "setting", "cell")
        corrected = by(rows(HERE / "lyon-lane-corrected.csv"), "setting", "cell")
        for metric in ["service_rate", "mean_wait_s"]:
            agree = total = inside = cells = separable = separable_agree = 0
            for (setting, cell), reference_rows in theirs.items():
                ranged = values(reference_rows, metric)
                mean_ours = statistics.mean(values(as_ran[(setting, cell)], metric))
                cells += 1
                inside += min(ranged) <= mean_ours <= max(ranged)
            for setting in sorted({setting for setting, _ in theirs}):
                for drivers in [2, 6, 10, 20]:
                    for riders in [2, 6, 10, 20]:
                        full = f"drivers-{drivers}-declaring-100-riders-{riders}-declaring-100"
                        none = f"drivers-{drivers}-declaring-0-riders-{riders}-declaring-0"
                        if (setting, full) not in theirs:
                            continue
                        delta = lambda group, key: statistics.mean(values(group[(setting, full)], key)) - statistics.mean(values(group[(setting, none)], key))
                        tolerance = 1e-9
                        total += 1
                        agree += sign(delta(theirs, metric), tolerance) == sign(delta(as_ran, metric), tolerance)
                        # The same seeds on both sides, so the reference difference is paired: it
                        # separates when its mean is more than twice its standard error.
                        paired = [a - b for a, b in zip(values(theirs[(setting, full)], metric),
                                                        values(theirs[(setting, none)], metric))]
                        spread = statistics.stdev(paired) / len(paired) ** 0.5 if len(paired) > 1 else 0.0
                        if abs(statistics.mean(paired)) > 2 * spread and spread > 0:
                            separable += 1
                            separable_agree += sign(delta(theirs, metric), tolerance) == sign(delta(as_ran, metric), tolerance)
            out.append({
                "test": "level", "scenario": "lane grid, every cell", "metric": metric,
                "agrees": f"{inside} of {cells}",
            })
            out.append({
                "test": "sign", "scenario": "lane grid, full declaration - none", "metric": metric,
                "agrees": f"{agree} of {total}",
            })
            out.append({
                "test": "sign", "scenario": "lane grid, full declaration - none, where the reference separates",
                "metric": metric, "agrees": f"{separable_agree} of {separable}",
            })

    columns = ["test", "scenario", "metric", "reference_min", "reference_mean", "reference_max",
               "as_ran_mean", "corrected_mean", "agrees"]
    with open(HERE / "agreement.csv", "w", newline="") as handle:
        writer = csv.DictWriter(handle, columns, lineterminator="\n")
        writer.writeheader()
        for row in out:
            writer.writerow({key: (f"{value:.4f}" if isinstance(value, float) else value)
                             for key, value in row.items()})
    for row in out:
        print({key: (round(value, 3) if isinstance(value, float) else value) for key, value in row.items()})


if __name__ == "__main__":
    main()
