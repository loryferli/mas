#!/usr/bin/env python3
"""Time series from event logs, as committed SVG.

    uv run analysis/timeseries.py NAME LABEL=path/to/events.csv [LABEL=...]

Writes `analysis/images/NAME-occupancy.svg` (distance-weighted occupancy in 15-minute intervals),
`NAME-distance.svg` (cumulative kilometres over the day, loaded and empty, one panel per run) and
`NAME-times.svg` (the distribution of riders' journey and waiting times). One line per run, each a
single seed: these are the shape of one day, not a finding about many, and a caption says so.

Everything is read off the log's own columns, never accumulated in the tick loop: between two rows
of the same vehicle, the distance it covered is attributed to the seats it had in use at the first
row and to the interval that row falls in. A vehicle counts as empty when it has no seat in use,
its own party included - the reading the private-car baseline needs, where a car with only its
driver aboard is a car in use, and exactly what a fleet taxi with nobody aboard reads as.

Same reproducibility rule as `charts.py`: a pinned hash salt and no date stamp, so a regenerated
figure is not a diff.
"""

import csv
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib

matplotlib.use("svg")
matplotlib.rcParams["svg.hashsalt"] = "mas-new"

import matplotlib.pyplot as plt

IMAGES = Path(__file__).resolve().parent / "images"
INTERVAL_S = 900


def read(path):
    """Per vehicle, its rows as (time, seats in use, kilometres so far); per rider, its rows as
    (time, state, reason)."""
    vehicles, riders = defaultdict(list), defaultdict(list)
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            t = float(row["t_s"])
            if row["seats_used"] != "":
                vehicles[row["agent_id"]].append((t, int(row["seats_used"]), float(row["distance_km"])))
            else:
                riders[row["agent_id"]].append((t, row["state"], row["reason"]))
    return vehicles, riders


def legs(vehicles):
    """Every stretch between two rows of one vehicle: (start time, seats in use, kilometres)."""
    for rows in vehicles.values():
        for (t, seats, km), (_, _, next_km) in zip(rows, rows[1:]):
            if next_km > km:
                yield t, seats, next_km - km


def rider_times(riders):
    """Journey times of riders that arrived, and every rider's total wait at stops, in minutes."""
    journeys, waits = [], []
    for rows in riders.values():
        spawned = rows[0][0]
        waited, since = 0.0, None
        for t, state, _ in rows:
            if since is not None and state != "WaitingDriver":
                waited += t - since
                since = None
            if state == "WaitingDriver" and since is None:
                since = t
        arrived = [t for t, state, reason in rows if state == "EndJourney" and reason == "ArrivedAtDestination"]
        if arrived:
            journeys.append((arrived[-1] - spawned) / 60)
            waits.append(waited / 60)
    return journeys, waits


def save(figure, path, caption):
    figure.text(0.01, 0.005, caption, fontsize=7, color="0.35")
    figure.savefig(path, metadata={"Date": None})
    plt.close(figure)
    print(f"wrote {path}")


def main():
    if len(sys.argv) < 3 or not all("=" in argument for argument in sys.argv[2:]):
        sys.exit(__doc__)
    name = sys.argv[1]
    runs = [argument.split("=", 1) for argument in sys.argv[2:]]
    data = [(label, read(path)) for label, path in runs]
    caption = "one run each, a single seed: the shape of one day, not a finding about many"

    figure, axis = plt.subplots(figsize=(8, 4.5))
    for label, (vehicles, _) in data:
        seat_km, km = defaultdict(float), defaultdict(float)
        for t, seats, distance in legs(vehicles):
            bucket = int(t // INTERVAL_S) * INTERVAL_S
            seat_km[bucket] += seats * distance
            km[bucket] += distance
        buckets = sorted(km)
        axis.plot([b / 3600 for b in buckets], [seat_km[b] / km[b] for b in buckets], marker="o",
                  markersize=2, linestyle="", label=label)
    axis.set_xlabel("time of day (h)")
    axis.set_ylabel("occupancy (seats in use per km)")
    axis.set_title("Vehicle occupancy by 15-minute intervals, weighted by distance")
    axis.legend()
    axis.grid(linestyle="--", alpha=0.5)
    save(figure, IMAGES / f"{name}-occupancy.svg", caption)

    figure, axes = plt.subplots(1, len(data), figsize=(5 * len(data), 4.5), sharey=True, squeeze=False)
    for axis, (label, (vehicles, _)) in zip(axes[0], data):
        # Summed by the minute: a point per leg would make a figure of megabytes.
        by_minute_total, by_minute_loaded = defaultdict(float), defaultdict(float)
        for t, seats, distance in legs(vehicles):
            by_minute_total[int(t // 60)] += distance
            by_minute_loaded[int(t // 60)] += distance if seats > 0 else 0.0
        times, total, loaded = [], [], []
        running_total = running_loaded = 0.0
        for minute in sorted(by_minute_total):
            running_total += by_minute_total[minute]
            running_loaded += by_minute_loaded[minute]
            times.append(minute / 60)
            total.append(running_total)
            loaded.append(running_loaded)
        axis.fill_between(times, loaded, alpha=0.7, hatch="///", label="with somebody aboard")
        axis.fill_between(times, total, loaded, alpha=0.3, label="empty")
        axis.plot(times, total, linewidth=1.5, label="total")
        axis.set_title(label)
        axis.set_xlabel("time of day (h)")
        axis.grid(axis="y", linestyle="--", alpha=0.5)
    axes[0][0].set_ylabel("cumulative vehicle-kilometres")
    axes[0][0].legend()
    save(figure, IMAGES / f"{name}-distance.svg", caption)

    figure, (journey_axis, wait_axis) = plt.subplots(1, 2, figsize=(10, 4.5))
    for label, (_, riders) in data:
        journeys, waits = rider_times(riders)
        if not journeys:
            continue
        journey_axis.hist(journeys, bins=30, histtype="step", label=label)
        wait_axis.hist(waits, bins=30, histtype="step", label=label)
    journey_axis.set_title("Riders' journey time")
    journey_axis.set_xlabel("minutes, spawn to arrival")
    wait_axis.set_title("Riders' waiting time")
    wait_axis.set_xlabel("minutes waiting at stops, summed over legs")
    for axis in (journey_axis, wait_axis):
        axis.set_ylabel("riders")
        axis.grid(axis="y", linestyle="--", alpha=0.5)
        axis.legend()
    save(figure, IMAGES / f"{name}-times.svg", caption)


if __name__ == "__main__":
    main()
