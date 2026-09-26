#!/usr/bin/env python3
"""Charts from a sweep's results.csv, as committed SVG.

Run after `cargo run --release -- sweep --config sweeps/lowflow.json`:

    uv run analysis/charts.py analysis/results.csv

One line per series, the mean over seeds, with the seed-to-seed spread as a band. A single seed
is one run of a stochastic model and not a finding; the band is what says how much of the shape
is the parameter and how much is the draw.

Any sweep CSV, not just one: the axis columns are whatever the sweep wrote between `scenario` and
`seed`, the last of them is the x axis and the first is the series, and the series sort falls back
to text so a policy or a construction charts as readily as a fleet size. Output is named after the
input, so two sweeps do not overwrite each other's figures.

The files are committed, so the output has to be reproducible: matplotlib stamps the SVG with the
time it was written and salts its element ids from the file path unless it is told not to, and
either one makes a regenerated chart a diff with no change in it.
"""

import csv
import statistics
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib

matplotlib.use("svg")
# A fixed salt, not the package name: matplotlib derives every SVG element id from it, so
# changing this string rewrites all 28 committed figures for nothing. It stays as it is.
matplotlib.rcParams["svg.hashsalt"] = "mas-new"

import matplotlib.pyplot as plt  # noqa: E402 - after the backend is chosen

# One chart per metric. The axes come from the file's own header.
#
# The last two are the cost of a model in the loop, and they are here rather than in a chart script
# of their own because what a decision bought and what it cost belong on the same page. A sweep
# under a fixed rule leaves them zero on every row and they are skipped, so `results.csv`,
# `networks.csv` and `policies.csv` still get exactly the four figures they always did.
CHARTS = [
    ("service_rate", "service rate", "riders carried, of riders that asked"),
    (
        "empty_distance_share",
        "empty distance share",
        "kilometres driven with nobody aboard",
    ),
    (
        "mean_wait_s",
        "mean wait (s)",
        "time at the departure station, over riders picked up",
    ),
    ("mean_occupancy", "mean occupancy", "passenger-kilometres per vehicle-kilometre"),
    (
        "usd_per_decision",
        "cost per decision (USD)",
        "what the model was paid to decide",
    ),
    (
        "invalid_action_rate",
        "invalid action rate",
        "decisions the model could not answer usably",
    ),
]


def axes_of(header):
    """The sweep's axis columns: everything it wrote between `scenario` and `seed`.

    The last is the x axis, because that is the one a sweep varies most finely, and the first is
    the series. One axis charts as a single line, and then there is no series at all: `None`
    rather than the x axis a second time, which would key every group by a pair that only agrees
    on the diagonal and leave the rest empty.
    """
    columns = header[header.index("scenario") + 1 : header.index("seed")]
    if not columns:
        raise SystemExit("no axis columns: a sweep with no axes has nothing to chart")
    return (columns[0] if len(columns) > 1 else None), columns[-1]


def read(path):
    """(series, x) -> list of metric dicts, one per seed, plus the two axis names."""
    grouped = defaultdict(list)
    with open(path, newline="") as handle:
        reader = csv.DictReader(handle)
        series_axis, x_axis = axes_of(reader.fieldnames)
        for row in reader:
            series = row[series_axis] if series_axis else ""
            grouped[(series, float(row[x_axis]))].append(row)
    return grouped, series_axis, x_axis


def sort_key(name):
    """Numeric where the axis is a number, alphabetical where it is a label."""
    try:
        return (0, float(name), "")
    except ValueError:
        return (1, 0.0, name)


def chart(grouped, series_axis, x_axis, metric, title, subtitle, out):
    series = sorted({key[0] for key in grouped}, key=sort_key)
    xs = sorted({key[1] for key in grouped})

    figure, axes = plt.subplots(figsize=(7.2, 4.2), layout="constrained")
    for name in series:
        values = [[float(row[metric]) for row in grouped[(name, x)]] for x in xs]
        axes.fill_between(
            xs,
            [min(v) for v in values],
            [max(v) for v in values],
            alpha=0.12,
            linewidth=0,
        )
        axes.plot(
            xs,
            [statistics.fmean(v) for v in values],
            marker="o",
            markersize=4,
            label=f"{series_axis} {name}" if series_axis else None,
        )

    axes.set_xlabel(x_axis)
    axes.set_xticks(xs)
    axes.set_ylim(bottom=0)
    axes.set_title(title, loc="left", fontweight="bold")
    axes.set_title(subtitle, loc="right", fontsize="small", color="0.4")
    axes.grid(axis="y", color="0.9")
    axes.set_axisbelow(True)
    for side in ("top", "right"):
        axes.spines[side].set_visible(False)
    if series_axis:
        axes.legend(frameon=False, fontsize="small")

    # No timestamp: the charts are committed, and a date turns every regeneration into a diff.
    figure.savefig(out, format="svg", metadata={"Date": None})
    plt.close(figure)
    return out


def main():
    results = Path(sys.argv[1] if len(sys.argv) > 1 else "results.csv")
    images = Path(__file__).parent / "images"
    images.mkdir(exist_ok=True)
    grouped, series_axis, x_axis = read(results)
    seeds = len(next(iter(grouped.values())))
    for metric, title, subtitle in CHARTS:
        if metric not in next(iter(grouped.values()))[0]:
            continue
        # A column that is zero on every row is a column this sweep did not exercise - the cost of
        # a model nobody called. A committed chart of nothing is a file to keep in step for no
        # reason.
        if all(float(row[metric]) == 0.0 for rows in grouped.values() for row in rows):
            continue
        written = chart(
            grouped,
            series_axis,
            x_axis,
            metric,
            title,
            f"{subtitle} - mean and range over {seeds} seeds",
            images / f"{results.stem}-{metric}.svg",
        )
        print(f"wrote {written}")


if __name__ == "__main__":
    main()
