#!/usr/bin/env python3
"""Ask the model to design a line set, once, offline - and write it where the sweep can run it.

This is the one decision in the repository that is not in the loop. It costs a single call per
scenario, so it is the only place the Batch API would even apply, and the engine never sees it: the
output is a committed scenario with its lines *written* rather than constructed, which is a shape
`scenario.rs` already accepts.

    ANTHROPIC_API_KEY=... uv run scripts/design-network.py

Two files come out, from one answer, so there is one source of truth for what the model said:

    scenarios/network-designed.json   the corridor with the model's lines written out
    sweeps/networks.json              a sixth point on the `construction` axis

Both are committed. A reader without a key can then check every claim about this against
`analysis/networks.csv` without making a call, which is the whole reason the outputs are committed
rather than produced on demand.

**The bar is known and there are two of them.** The five constructions came back negative - see
`analysis/networks.csv` - and `baseline.json`'s six hand-declared one-way lines beat every one of
them wherever vehicles are scarce, 0.853 at thirteen drivers. Direction is why: a symmetric
construction runs half its lines away from a radial morning commute, and `flow-directed` - the same
pairs as `flow-ranked` taken one way each, oriented by the table - recovers 0.797 of that gap for
nothing. So the number a designed network has to beat is **0.797** to be worth having and **0.853**
to match what a person did by hand. Reporting only the win against `flow-ranked`'s 0.750 would be
reporting against the weakest opponent on the axis.

The model is given the corridor's own demand table and its boarding points and nothing else. It is
not told what the constructions produced, what any of them scored, or that asymmetry is the answer -
being told the answer is how this stops being a result.
"""

import argparse
import collections
import csv
import json
import pathlib
import sys

import anthropic

ROOT = pathlib.Path(__file__).resolve().parent.parent
FLOWS = ROOT / "data" / "flows.csv"
STATIONS = ROOT / "data" / "stations.json"
BASE = ROOT / "scenarios" / "network.json"
SCENARIO_OUT = ROOT / "scenarios" / "network-designed.json"
SWEEP = ROOT / "sweeps" / "networks.json"

# Matches `scripts/sidecar.py`; see the note there about why a table rather than a meter reading.
PRICES_PER_MTOK = {
    "claude-opus-5": (5.00, 25.00),
    "claude-sonnet-5": (2.00, 10.00),
    "claude-haiku-4-5": (1.00, 5.00),
}

BRIEF = """\
You are designing the route network for a shared-mobility service on a rural corridor in mid-Wales:
five census areas, seven boarding points, twenty-three kilometres end to end.

A **line** is a directed pair of boarding points. A driver serving a line drives to its origin,
picks up whoever is waiting there, and sets them down at its destination; a traveller walks to the
origin and walks on from the destination. Direction matters: a line from A to B carries nobody from
B to A.

You are given the corridor's commuting table - how many people travel between each pair of areas by
car, as a driver and as a passenger - and the boarding points with the area each one stands in.
Nothing else. There is no forecast and no score.

Design the line set. More lines means more places a traveller might be collected and more ways for
a handful of vehicles to be spread thin; fewer means a traveller whose journey no line covers.
Choose the set you would actually run, and say briefly why in `reasoning`."""


def flows_by_area() -> tuple[dict, dict]:
    """Directed area-to-area car commutes: the demand a shared service could actually carry."""
    counts = collections.OrderedDict()
    names = {}
    with FLOWS.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            names[row["origin_code"]] = row["origin_name"]
            names[row["destination_code"]] = row["destination_name"]
            key = (row["origin_code"], row["destination_code"])
            counts[key] = (
                counts.get(key, 0) + int(row["car_driver_count"]) + int(row["car_passenger_count"])
            )
    return counts, names


def brief_context() -> dict:
    counts, names = flows_by_area()
    stations = json.loads(STATIONS.read_text(encoding="utf-8"))
    corridor = set(stations["corridor"])
    return {
        "stations": [
            {
                "name": station["name"],
                "area_code": station["area_code"],
                "latitude": station["latitude"],
                "longitude": station["longitude"],
            }
            for station in stations["stations"]
        ],
        "areas": [{"code": code, "name": name} for code, name in names.items() if code in corridor],
        "car_commutes": [
            {"from_area": origin, "to_area": destination, "people": people}
            for (origin, destination), people in counts.items()
            if origin in corridor and destination in corridor and people > 0
        ],
    }


SCHEMA = {
    "type": "object",
    "properties": {
        "reasoning": {"type": "string"},
        "lines": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "origin": {"type": "string"},
                    "destination": {"type": "string"},
                },
                "required": ["origin", "destination"],
                "additionalProperties": False,
            },
        },
    },
    "required": ["reasoning", "lines"],
    "additionalProperties": False,
}


def validate(lines: list, station_names: set) -> list:
    """Every rule the engine would enforce at load, checked here so a bad answer is a message
    rather than a scenario that fails to load three commands later."""
    seen = set()
    checked = []
    for line in lines:
        origin, destination = line["origin"], line["destination"]
        for name in (origin, destination):
            if name not in station_names:
                raise ValueError(f"no such boarding point: {name!r}")
        if origin == destination:
            raise ValueError(f"a line from {origin!r} to itself")
        if (origin, destination) in seen:
            raise ValueError(f"{origin!r} to {destination!r} twice")
        seen.add((origin, destination))
        checked.append({"origin": origin, "destination": destination})
    if not checked:
        raise ValueError("a network with no lines in it")
    return checked


def write_scenario(lines: list) -> None:
    scenario = json.loads(BASE.read_text(encoding="utf-8"))
    scenario["name"] = "network-designed"
    # Written rather than constructed: `construction` and `networks` together is a load error,
    # because one of the two would be ignored.
    scenario["environment"].pop("construction", None)
    scenario["environment"]["networks"] = [
        {
            "name": "designed",
            "operator": "Ceredigion Shared Mobility",
            "lines": lines,
        }
    ]
    SCENARIO_OUT.write_text(json.dumps(scenario, indent=2) + "\n", encoding="utf-8")


def write_sweep_point(lines: list) -> None:
    """The sixth point on the axis the five constructions already share, so the comparison is one
    set of rows and nothing downstream changes. `construction: null` is what deserialises to "no
    construction" - the scenario declares its lines instead, and declaring both is a load error."""
    config = json.loads(SWEEP.read_text(encoding="utf-8"))
    axis = next(axis for axis in config["axes"] if axis["name"] == "construction")
    point = {
        "value": "designed",
        "set": {
            "/environment/construction": None,
            "/environment/networks": [
                {
                    "name": "designed",
                    "operator": "Ceredigion Shared Mobility",
                    "lines": lines,
                }
            ],
        },
    }
    axis["points"] = [p for p in axis["points"] if p["value"] != "designed"] + [point]
    SWEEP.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="claude-opus-5")
    parser.add_argument(
        "--effort", default="high", choices=["low", "medium", "high", "xhigh", "max"]
    )
    args = parser.parse_args()

    context = brief_context()
    station_names = {station["name"] for station in context["stations"]}

    client = anthropic.Anthropic()
    response = client.messages.create(
        model=args.model,
        max_tokens=16000,
        system=BRIEF,
        output_config={
            "effort": args.effort,
            "format": {"type": "json_schema", "schema": SCHEMA},
        },
        messages=[{"role": "user", "content": json.dumps(context, indent=2)}],
    )
    if response.stop_reason == "refusal":
        print("the model declined the design", file=sys.stderr)
        return 1
    answer = json.loads(next(b.text for b in response.content if b.type == "text"))

    lines = validate(answer["lines"], station_names)
    write_scenario(lines)
    write_sweep_point(lines)

    rates = PRICES_PER_MTOK.get(args.model, (0.0, 0.0))
    usd = (
        response.usage.input_tokens * rates[0] + response.usage.output_tokens * rates[1]
    ) / 1_000_000
    print(f"{len(lines)} directed lines, from one call: ${usd:.4f}", file=sys.stderr)
    print(
        f"  {response.usage.input_tokens} in, {response.usage.output_tokens} out",
        file=sys.stderr,
    )
    print(f"  {answer['reasoning']}", file=sys.stderr)
    print(
        f"wrote {SCENARIO_OUT.relative_to(ROOT)} and the `designed` point in "
        f"{SWEEP.relative_to(ROOT)}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
