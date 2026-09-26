#!/usr/bin/env bash
# Regenerate every committed CSV and chart that needs nothing but this repository, in place.
#
#     scripts/regenerate.sh && git status --short    # empty: every committed number reproduces
#
# Offline and deterministic: every sweep but the model's, every chart, the time series and the
# agreement table. Left out, because each needs something the repository does not hold: the model
# sweep and the network design (an API key), `fetch-data.py`, `fetch-lyon.py` and `build-cache`
# (the network), and `scripts/reference/` (a checkout of the original simulator), whose committed
# output the agreement table reads. About 25 minutes on 16 cores, most of it the Lyon sweeps at the 10 sample.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --quiet
mas=target/release/mas

for config in sweeps/*.json; do
    name=$(basename "$config" .json)
    case "$name" in
        model-policies) continue ;;
        lowflow) out=analysis/results.csv ;;
        *) out=analysis/$name.csv ;;
    esac
    echo "sweep $name"
    "$mas" sweep --config "$config" --out "$out" 2>/dev/null
done

# The corridor's charts; the Lyon sweeps are read through the agreement table instead.
for csv in results policies networks walking fleet declaration adoption approach; do
    uv run analysis/charts.py "analysis/$csv.csv" >/dev/null
done

# One day of each service on the second study area, seed 1, as ran.
day=$(mktemp -d)
trap 'rm -rf "$day"' EXIT
for scenario in private-cars-100 fleet-door-to-door-100 fleet-network-100; do
    "$mas" run --scenario "scenarios/lyon/$scenario.json" --seed 1 --out "$day/$scenario" >/dev/null 2>&1
done
uv run analysis/timeseries.py lyon-100 \
    "private cars=$day/private-cars-100/events.csv" \
    "door to door=$day/fleet-door-to-door-100/events.csv" \
    "network=$day/fleet-network-100/events.csv" >/dev/null

uv run analysis/agreement.py >/dev/null
echo "done: git status --short should be empty"
