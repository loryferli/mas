#!/usr/bin/env python3
"""Write the Lyon scenarios, in the agent-list format, from `data/lyon/`.

    python3 scripts/build-lyon-scenarios.py OUT_DIR [--grid]

Standard library only, offline, and deterministic: the files are a pure function of `data/lyon/`
and the tables below. Two families:

  * the lane      Bourgoin-Jallieu into Lyon on one line with a departure guarantee:
                  `lane-bourgoin.json`, `lane-mermoz.json` (the reverse direction), and with
                  --grid the 14 x 784 advance-declaration grid under `grid/<setting>/`;
  * the network   the Rhône's thousand largest commuting pairs, an outbound cohort and a return
                  cohort each, under three services - `private-cars-*` (everybody drives),
                  `fleet-door-to-door-*` and `fleet-network-*` (the seventeen-station network).

The tables are the specification of what each scenario holds - counts, windows, leads, margins,
capacities, fleet sizes - and `data/lyon/README.md` says where each came from. Geography and
demand are never written here: every coordinate is read from `data/lyon/`.
"""

import csv
import json
import math
import sys
from pathlib import Path

DATA = Path(__file__).resolve().parent.parent / "data" / "lyon"

# --- the lane --------------------------------------------------------------------------------

LANE_WINDOW_S = (0, 3600)
DRIVER_JITTER_KM = {"origin": 2, "destination": 5}
OPPORTUNISTIC_ORIGIN_JITTER_KM = 0.5
RIDER_JITTER_KM = {"origin": 0.1, "destination": 1}
CAR_SEATS = 5
GUARANTEE = {"threshold": 1200, "trigger": 1200, "minimum_between_two_start": 1200, "capacity": 8}

# The grid's settings: (driver lead, rider lead, earliness, lateness, driver noise, rider noise),
# the same margins on both sides. Seven with no departure noise at three leads, seven at a 600 s
# driver lead and a 300 s rider lead with noise, varying the margins.
GRID = {
    "lead-1800": (1800, 1800, 0, 0, (0, 0), (0, 0)),
    "lead-1800-margins-900": (1800, 1800, 900, 900, (0, 0), (0, 0)),
    "lead-3600": (3600, 3600, 0, 0, (0, 0), (0, 0)),
    "lead-3600-margins-900": (3600, 3600, 900, 900, (0, 0), (0, 0)),
    "lead-6000": (6000, 6000, 0, 0, (0, 0), (0, 0)),
    "lead-6000-margins-300": (6000, 6000, 300, 300, (0, 0), (0, 0)),
    "lead-600": (600, 300, 0, 0, (-60, 240), (-180, 120)),
    "lead-600-late-60": (600, 300, 0, 60, (-60, 240), (-180, 120)),
    "lead-600-early-120": (600, 300, 120, 0, (-60, 240), (-180, 120)),
    "lead-600-margins-120": (600, 300, 120, 120, (-60, 240), (-180, 120)),
    "lead-600-margins-240": (600, 300, 240, 240, (-60, 240), (-180, 120)),
    "lead-600-early-60": (600, 300, 60, 0, (-60, 240), (-180, 120)),
    "lead-600-margins-60": (600, 300, 60, 60, (-60, 240), (-180, 120)),
}
GRID_COUNTS = [2, 4, 6, 8, 10, 15, 20]
GRID_DECLARING_PCT = [0, 20, 50, 100]

# --- the network -----------------------------------------------------------------------------

OUTBOUND_WINDOW_S = (21600, 32400)
RETURN_WINDOW_S = (61200, 72000)
TRIP_JITTER_KM = 10
# Display fields the agent-list format's network graph builder requires although nothing it
# computes reads them: a station's catchment radius in metres, a line's geometry and colour.
STATION_RADIUS_M = 1000
LINE_DISPLAY = {"polyline": "", "color": [255, 0, 0]}
FLEET_SPAWN_S = 21600
# (share of each pair's flow, pairs kept). `ceil(flow x share)` per cohort, so no pair is empty.
SAMPLES = {
    "10": (0.075, 1000),
    "100": (0.0075, 1000),
    "200": (0.0075, 250),
    "complete": (1.0, 1000),
}
SERVICES = {
    # service: {sample: fleet size}, seats, fleet spawn jitter
    "fleet-door-to-door": ({"10": 9000, "100": 450, "complete": 90000}, 5, 20),
    "fleet-network": ({"10": 4500, "100": 450, "200": 450, "complete": 45000}, 9, 30),
}
PRIVATE_SAMPLES = ["10", "100", "complete"]

# The seventeen-station network as undirected pairs; each is two directed lines. The 100 sample
# ran a variant with Belleville and Saint-Priest in place of Bourgoin and Saint-Laurent-de-Mure.
NETWORK_PAIRS = [
    ("Anse", "Limonest"),
    ("Anse", "Villefranche-sur-Saone"),
    ("Brignais", "Francheville"),
    ("Brignais", "Mornant"),
    ("Brignais", "Oullins"),
    ("Caluire", "Ecully"),
    ("Caluire", "Villeurbanne"),
    ("Craponne", "Francheville"),
    ("Ecully", "Francheville"),
    ("Ecully", "Limonest"),
    ("Ecully", "Oullins"),
    ("Fleurieux", "Lentilly"),
    ("Lentilly", "Limonest"),
    ("Mermoz", "Oullins"),
    ("Mermoz", "Villeurbanne"),
    ("Meyzieu", "Villeurbanne"),
]
NETWORK_EAST = [("Bourgoin", "Saint-Laurent-de-Mure"), ("Mermoz", "Saint-Laurent-de-Mure")]
NETWORK_VARIANT = [("Belleville", "Villefranche-sur-Saone"), ("Mermoz", "Saint-Priest")]
NETWORK_ORDER = [
    "Villefranche-sur-Saone",
    "Anse",
    "Limonest",
    "Lentilly",
    "Fleurieux",
    "Ecully",
    "Caluire",
    "Villeurbanne",
    "Meyzieu",
    "Mermoz",
    "Saint-Laurent-de-Mure",
    "Bourgoin",
    "Oullins",
    "Brignais",
    "Mornant",
    "Francheville",
    "Craponne",
]


def point(place, jitter_km):
    return {
        "latitude": place["latitude"],
        "longitude": place["longitude"],
        "neighborhood": jitter_km,
    }


def coordinates(place):
    return {"latitude": place["latitude"], "longitude": place["longitude"]}


def distribution(count, window):
    return {"count": count, "start": window[0], "end": window[1]}


def scenario(agents, environment):
    return {"simulation": {"start": {"agents": agents}, "running": {}}, "environment": environment}


def load():
    document = json.loads((DATA / "stations.json").read_text())
    stations = {s["name"]: s for s in document["stations"]}
    places = {p["name"]: p for p in document["places"]}
    with open(DATA / "flows.csv", newline="") as handle:
        flows = list(csv.DictReader(handle))
    return stations, places, flows


# --- lane scenarios --------------------------------------------------------------------------


def lane_environment(stations, origin, destination, guarantee):
    line = {"origin": origin, "destination": destination}
    if guarantee:
        line["gd"] = {**GUARANTEE, "coordinates": coordinates(stations[origin])}
    return {
        "stations": [
            {
                "orchestrator": "operator",
                "coordinates": coordinates(stations[name]),
                "name": name,
                "radius": STATION_RADIUS_M,
            }
            for name in (origin, destination)
        ],
        "networks": [{"name": "lane", "orchestrator": "operator", "lines": [line]}],
    }


def lane_bourgoin(stations, places):
    """Opportunistic and declaring drivers, ghost and carpool riders, Bourgoin into Lyon."""
    home, work = places["Bourgoin-Jallieu"], places["Lyon 8e"]
    station, arrival = stations["Bourgoin"], stations["Mermoz"]

    def driver(kind, name, count, window):
        return {
            "class": kind,
            "name": name,
            "origin": point(home, DRIVER_JITTER_KM["origin"]),
            "destination": point(work, DRIVER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "vehicle": {"capacity": CAR_SEATS},
            "distribution": distribution(count, LANE_WINDOW_S),
        }

    def rider(kind, name, count, window):
        return {
            "class": kind,
            "name": name,
            "origin": point(station, RIDER_JITTER_KM["origin"]),
            "destination": point(arrival, RIDER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "distribution": distribution(count, LANE_WINDOW_S),
        }

    agents = [
        {
            "class": "CarpoolOrchestrator",
            "name": "operator",
            "distribution": distribution(1, (0, 0)),
        },
        driver("CarpoolDriver", "carpool driver", 0, {"shift": 0, "margin": 10}),
        driver("CarpoolDriver", "carpool driver atc", 15, {"shift": 0, "margin": 10}),
        driver("GhostDriver", "ghost", 0, {"shift": 0, "margin": 5}),
        driver("OpportunisticDriver", "opportunistic", 10, {"shift": 0, "margin": 5}),
        rider("GhostRider", "rider", 10, {"shift": 0, "margin": 0}),
        rider("CarpoolRider", "carpool rider", 10, {"shift": 0, "margin": 10}),
    ]
    return scenario(agents, lane_environment(stations, "Bourgoin", "Mermoz", guarantee=False))


def lane_mermoz(stations, places):
    """The evening direction: Lyon to Bourgoin, opportunistic drivers and carpool riders."""
    home, station, arrival = places["Lyon 8e"], stations["Mermoz"], stations["Bourgoin"]

    def driver(kind, name, count, window):
        return {
            "class": kind,
            "name": name,
            "origin": point(home, DRIVER_JITTER_KM["origin"]),
            "destination": point(arrival, DRIVER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "vehicle": {"capacity": CAR_SEATS},
            "distribution": distribution(count, LANE_WINDOW_S),
        }

    def rider(kind, name, count, window):
        return {
            "class": kind,
            "name": name,
            "origin": point(station, RIDER_JITTER_KM["origin"]),
            "destination": point(arrival, RIDER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "distribution": distribution(count, LANE_WINDOW_S),
        }

    agents = [
        {
            "class": "CarpoolOrchestrator",
            "name": "operator",
            "distribution": distribution(1, (0, 0)),
        },
        driver("CarpoolDriver", "carpool driver", 0, {"shift": 0, "margin": 10}),
        driver("CarpoolDriver", "carpool driver atc", 0, {"shift": 0, "margin": 10}),
        driver("GhostDriver", "ghost", 0, {"shift": 0, "margin": 5}),
        driver("OpportunisticDriver", "opportunistic", 10, {"shift": 0, "margin": 5}),
        rider("GhostRider", "rider", 0, {"shift": 0, "margin": 0}),
        rider("CarpoolRider", "carpool rider", 10, {"shift": 0, "margin": 10}),
    ]
    return scenario(agents, lane_environment(stations, "Mermoz", "Bourgoin", guarantee=False))


def grid_template(stations, places, setting):
    driver_lead, rider_lead, early, late, driver_noise, rider_noise = setting
    home, work = places["Bourgoin-Jallieu"], places["Lyon 8e"]
    station, arrival = stations["Bourgoin"], stations["Mermoz"]

    def driver(kind, name, window, origin_jitter=DRIVER_JITTER_KM["origin"]):
        return {
            "class": kind,
            "name": name,
            "origin": point(home, origin_jitter),
            "destination": point(work, DRIVER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "vehicle": {"capacity": CAR_SEATS},
            "distribution": distribution(0, LANE_WINDOW_S),
        }

    def rider(kind, name, window):
        return {
            "class": kind,
            "name": name,
            "origin": point(station, RIDER_JITTER_KM["origin"]),
            "destination": point(arrival, RIDER_JITTER_KM["destination"]),
            "departure_time_window": window,
            "distribution": distribution(0, LANE_WINDOW_S),
        }

    def declaring(key, lead, noise):
        return {
            key: lead,
            "incertitude": list(noise),
            "shift": 0,
            "earliness_margin": early,
            "lateness_margin": late,
        }

    agents = [
        {
            "class": "CarpoolOrchestrator",
            "name": "operator",
            "distribution": distribution(0, (0, 0)),
        },
        driver("CarpoolDriver", "carpool driver", {"shift": 0}),
        driver("CarpoolDriver", "carpool driver atc", declaring("atc", driver_lead, driver_noise)),
        driver("GhostDriver", "ghost", {"shift": 0}),
        driver(
            "OpportunisticDriver", "opportunistic", {"shift": 0}, OPPORTUNISTIC_ORIGIN_JITTER_KM
        ),
        rider("GhostRider", "rider", {"shift": 0}),
        rider("CarpoolRider", "carpool rider", {"shift": 0}),
        rider("CarpoolRider", "carpool rider atp", declaring("atp", rider_lead, rider_noise)),
    ]
    return scenario(agents, lane_environment(stations, "Bourgoin", "Mermoz", guarantee=True))


def grid(template):
    """Every cell of the grid: drivers x riders x declaring share on each side."""
    for drivers in GRID_COUNTS:
        for rider_pct in GRID_DECLARING_PCT:
            for driver_pct in GRID_DECLARING_PCT:
                for riders in GRID_COUNTS:
                    counts = {
                        "carpool driver": drivers - round(drivers * driver_pct / 100),
                        "carpool driver atc": round(drivers * driver_pct / 100),
                        "carpool rider": riders - round(riders * rider_pct / 100),
                        "carpool rider atp": round(riders * rider_pct / 100),
                    }
                    cell = json.loads(json.dumps(template))
                    for agent in cell["simulation"]["start"]["agents"]:
                        agent["distribution"]["count"] = counts.get(
                            agent["name"], 1 if agent["class"] == "CarpoolOrchestrator" else 0
                        )
                    name = (
                        f"drivers-{drivers}-declaring-{driver_pct}"
                        f"-riders-{riders}-declaring-{rider_pct}"
                    )
                    yield name, cell


# --- network scenarios -----------------------------------------------------------------------


def cohorts(flows, kind, share, pairs, vehicle=None):
    agents = []
    for index, row in enumerate(flows[:pairs]):
        count = math.ceil(float(row["motorised_flow"]) * share)
        home = {
            "latitude": float(row["origin_latitude"]),
            "longitude": float(row["origin_longitude"]),
        }
        work = {
            "latitude": float(row["destination_latitude"]),
            "longitude": float(row["destination_longitude"]),
        }
        # The names carry the direction: the agent-list format reads the spawn peak off them.
        for direction, window, start, end in (
            ("outbound", OUTBOUND_WINDOW_S, home, work),
            ("_return", RETURN_WINDOW_S, work, home),
        ):
            agent = {
                "class": kind,
                "name": f"{kind.lower()}{index}_{direction}",
                "distribution": distribution(count, window),
                "origin": point(start, TRIP_JITTER_KM),
                "destination": point(end, TRIP_JITTER_KM),
                "departure_time_window": {"margin": 0, "shift": 0},
            }
            if vehicle:
                agent["vehicle"] = {"capacity": vehicle}
            agents.append(agent)
    return agents


def network_environment(stations, variant):
    pairs = NETWORK_PAIRS + (NETWORK_VARIANT if variant else NETWORK_EAST)
    names = [n for n in NETWORK_ORDER if any(n in pair for pair in pairs)]
    names += [n for pair in pairs for n in pair if n not in names]
    lines = [{"origin": a, "destination": b, **LINE_DISPLAY} for a, b in pairs] + [
        {"origin": b, "destination": a, **LINE_DISPLAY} for a, b in pairs
    ]
    return {
        "stations": [
            {
                "orchestrator": "fleet operator",
                "coordinates": coordinates(stations[n]),
                "name": n,
                "radius": STATION_RADIUS_M,
            }
            for n in names
        ],
        "networks": [{"name": "network", "orchestrator": "fleet operator", "lines": lines}],
    }


def private_cars(flows, sample):
    share, pairs = SAMPLES[sample]
    return scenario(cohorts(flows, "Driver", share, pairs, vehicle=CAR_SEATS), {})


def fleet(flows, stations, service, sample):
    sizes, seats, jitter_km = SERVICES[service]
    share, pairs = SAMPLES[sample]
    first = flows[0]
    depot = {
        "latitude": float(first["origin_latitude"]),
        "longitude": float(first["origin_longitude"]),
    }
    agents = [
        {
            "class": "AutonomousTaxiOrchestrator",
            "name": "fleet operator",
            "distribution": distribution(1, (0, 0)),
        },
        # The fleet stands around the largest pair's origin; "Taxi" in the name is what the
        # agent-list format spawns at its own fixed time.
        {
            "class": "AutonomousTaxi",
            "name": "FleetTaxi",
            "distribution": distribution(sizes[sample], (FLEET_SPAWN_S, FLEET_SPAWN_S)),
            "origin": point(depot, jitter_km),
            "vehicle": {"capacity": seats},
            "departure_time_window": {"margin": 0, "shift": 0},
        },
        *cohorts(flows, "AutonomousTaxiRider", share, pairs),
    ]
    environment = (
        network_environment(stations, variant=sample == "100") if service == "fleet-network" else {}
    )
    return scenario(agents, environment)


def write(path, document):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 1:
        sys.exit(__doc__)
    out = Path(args[0])
    stations, places, flows = load()
    written = 0
    write(out / "lane-bourgoin.json", lane_bourgoin(stations, places))
    write(out / "lane-mermoz.json", lane_mermoz(stations, places))
    written += 2
    if "--grid" in sys.argv:
        for setting, values in GRID.items():
            for name, cell in grid(grid_template(stations, places, values)):
                write(out / "grid" / setting / f"{name}.json", cell)
                written += 1
    for sample in PRIVATE_SAMPLES:
        write(out / f"private-cars-{sample}.json", private_cars(flows, sample))
        written += 1
    for service, (sizes, _, _) in SERVICES.items():
        for sample in sizes:
            write(out / f"{service}-{sample}.json", fleet(flows, stations, service, sample))
            written += 1
    print(f"{written} scenarios written to {out}")


if __name__ == "__main__":
    main()
