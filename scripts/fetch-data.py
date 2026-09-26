#!/usr/bin/env python3
"""Fetch the study area's demand table and boarding points, and commit them under `data/`.

Run by hand, never by the test suite, on the same rule as `cargo run -- build-cache`: one
command, its output committed, and `cargo test` stays offline.

    uv run scripts/fetch-data.py

Writes `data/flows.csv` (the travel-to-work flows between the corridor's areas, by mode, with
each area's population-weighted centroid) and `data/stations.json` (the boarding points inside
the corridor). `data/README.md` carries the provenance, the licences and the selection rules;
this script is the executable half of that document, so a rule changed here has to change there
too.

Three sources, all open, and none of them reached during a run:

  * the flows       Nomis, 2011 Census table WU03EW, Open Government Licence v3
  * the centroids   ONS Open Geography Portal, Open Government Licence v3
  * the boundaries  ONS Open Geography Portal, Open Government Licence v3
  * the stops       OpenStreetMap via Overpass, Open Database Licence 1.0
"""

import csv
import json
import math
import sys
import urllib.parse
import urllib.request
from pathlib import Path

DATA = Path(__file__).resolve().parent.parent / "data"

# --- the corridor ----------------------------------------------------------------------------
#
# North Ceredigion into Aberystwyth. Every area here sends at least a quarter of its working
# residents to the two Aberystwyth areas, which is the criterion `data/README.md` records; the
# four areas of Ceredigion that fall below it are the south of the county, which commutes to
# Cardigan and Lampeter instead and is a different corridor.
CORRIDOR = [
    "W02000116",  # Ceredigion 001 - Borth, Bow Street, Tirymynach
    "W02000117",  # Ceredigion 002 - Aberystwyth town
    "W02000118",  # Ceredigion 003 - Aberystwyth south and Llanbadarn
    "W02000120",  # Ceredigion 005 - Llanrhystud and the coast road south
    "W02000421",  # Ceredigion 011 - the Rheidol and Ystwyth valleys
]

FLOW_DATASET = "NM_1208_1"

# Nomis codes for the method-of-travel dimension, in the order the columns are written. `0` is
# the row total and is written as `total_count`, which the mode columns must sum to.
MODES = [
    ("1", "work_at_home_count"),
    ("2", "metro_count"),
    ("3", "train_count"),
    ("4", "bus_count"),
    ("5", "taxi_count"),
    ("6", "motorcycle_count"),
    ("7", "car_driver_count"),
    ("8", "car_passenger_count"),
    ("9", "bicycle_count"),
    ("10", "on_foot_count"),
    ("11", "other_count"),
]

GEOPORTAL = "https://services1.arcgis.com/ESMARspQHYMw9BZ9/arcgis/rest/services"
CENTROIDS_LAYER = "MSOA_Dec_2011_PWC_in_England_and_Wales_2022"
BOUNDARIES_LAYER = "MSOA_Dec_2011_Boundaries_Generalised_Clipped_BGC_EW_V3_2022"

# The boundary box handed to Overpass is the corridor's own extent, padded so a stop sitting a
# little outside a generalised boundary is still fetched and then tested properly.
BBOX_PAD_DEGREES = 0.02

# Two boarding points closer than this are the same interchange described twice - a bus station
# in the forecourt of a railway station, a stop on the pavement outside it. The higher-ranked
# one survives, so the rail station wins and the model gets one station rather than three.
DEDUPE_KM = 0.3

# Highest first. A boarding point of any of the first three kinds is taken wherever it is in the
# corridor; a plain bus stop is taken only as the one nearest an area's centroid, because there
# are hundreds of them and a corridor of hundreds of stations is not a corridor.
KIND_RANK = ["rail", "bus_station", "park_ride", "bus_stop"]


def fetch(url: str, body: str | None = None) -> bytes:
    # A user agent, because Overpass refuses the default one, and a POST for the Overpass query
    # because it does not fit in a URL politely.
    request = urllib.request.Request(
        url,
        data=body.encode() if body else None,
        headers={"User-Agent": "mas data fetch (github.com/loryferli)"},
    )
    with urllib.request.urlopen(request, timeout=180) as response:
        return response.read()


def haversine_km(a, b) -> float:
    radius_km = 6371.0088
    lat_a, lat_b = math.radians(a[0]), math.radians(b[0])
    d_lat, d_lon = lat_b - lat_a, math.radians(b[1] - a[1])
    h = (
        math.sin(d_lat / 2) ** 2
        + math.cos(lat_a) * math.cos(lat_b) * math.sin(d_lon / 2) ** 2
    )
    return 2 * radius_km * math.asin(math.sqrt(h))


# --- the areas -------------------------------------------------------------------------------


def geoportal_query(layer: str, fields: str, out_format: str) -> dict:
    codes = ",".join(f"'{code}'" for code in CORRIDOR)
    query = urllib.parse.urlencode(
        {
            "where": f"MSOA11CD IN ({codes})",
            "outFields": fields,
            "outSR": "4326",
            "f": out_format,
        }
    )
    return json.loads(fetch(f"{GEOPORTAL}/{layer}/FeatureServer/0/query?{query}"))


def fetch_centroids() -> dict:
    """Each area's population-weighted centroid, as `code -> (name, latitude, longitude)`."""
    features = geoportal_query(CENTROIDS_LAYER, "msoa11cd,msoa11nm", "json")["features"]
    centroids = {
        f["attributes"]["msoa11cd"]: (
            f["attributes"]["msoa11nm"],
            f["geometry"]["y"],
            f["geometry"]["x"],
        )
        for f in features
    }
    missing = [code for code in CORRIDOR if code not in centroids]
    if missing:
        sys.exit(f"no centroid for {missing}")
    return centroids


def fetch_boundaries() -> dict:
    """Each area's outline, as `code -> [ring]`, a ring being a list of (longitude, latitude)."""
    features = geoportal_query(BOUNDARIES_LAYER, "MSOA11CD", "geojson")["features"]
    boundaries = {}
    for feature in features:
        geometry = feature["geometry"]
        rings = (
            geometry["coordinates"]
            if geometry["type"] == "Polygon"
            else [ring for part in geometry["coordinates"] for ring in part]
        )
        boundaries[feature["properties"]["MSOA11CD"]] = rings
    missing = [code for code in CORRIDOR if code not in boundaries]
    if missing:
        sys.exit(f"no boundary for {missing}")
    return boundaries


def contains(rings, latitude: float, longitude: float) -> bool:
    """Ray casting, exclusive-or across the rings so a hole counts as outside.

    ponytail: the boundaries are the generalised release, so a stop within a few tens of metres
    of an area's edge can land on the wrong side. Fetch the full-resolution boundaries if a
    station ever has to be attributed exactly.
    """
    inside = False
    for ring in rings:
        crossings = False
        for index in range(len(ring)):
            x1, y1 = ring[index - 1]
            x2, y2 = ring[index]
            if (y1 > latitude) != (y2 > latitude):
                if longitude < x1 + (latitude - y1) * (x2 - x1) / (y2 - y1):
                    crossings = not crossings
        inside ^= crossings
    return inside


def area_of(boundaries, latitude: float, longitude: float):
    for code in CORRIDOR:
        if contains(boundaries[code], latitude, longitude):
            return code
    return None


# --- the flows -------------------------------------------------------------------------------


def flow_url() -> str:
    codes = ",".join(CORRIDOR)
    modes = ",".join(["0"] + [code for code, _ in MODES])
    return (
        f"https://www.nomisweb.co.uk/api/v01/dataset/{FLOW_DATASET}.data.csv"
        f"?USUAL_RESIDENCE={codes}&PLACE_OF_WORK={codes}"
        f"&TRANSPORT_POWPEW11={modes}&MEASURES=20100"
        "&select=USUAL_RESIDENCE_CODE,PLACE_OF_WORK_CODE,TRANSPORT_POWPEW11,OBS_VALUE"
    )


def write_flows(centroids) -> None:
    """`data/flows.csv`: one row per ordered pair of corridor areas, with its mode breakdown.

    Only area-to-area flows are asked for, so the source's four special workplace codes - works
    mainly at or from home, no fixed place, offshore installation, outside the United Kingdom -
    are not in the extract. That is why `work_at_home_count` is zero on every row and is kept
    anyway: the mode columns have to sum to `total_count`, and a column that is always zero says
    out loud that the home workers were excluded rather than lost.
    """
    counts = {}
    reader = csv.DictReader(fetch(flow_url()).decode().splitlines())
    for row in reader:
        pair = (row["USUAL_RESIDENCE_CODE"], row["PLACE_OF_WORK_CODE"])
        counts.setdefault(pair, {})[row["TRANSPORT_POWPEW11"]] = int(row["OBS_VALUE"])

    path = DATA / "flows.csv"
    with path.open("w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(
            [
                "origin_code",
                "origin_name",
                "origin_latitude",
                "origin_longitude",
                "destination_code",
                "destination_name",
                "destination_latitude",
                "destination_longitude",
                "total_count",
                *[name for _, name in MODES],
            ]
        )
        for origin in CORRIDOR:
            for destination in CORRIDOR:
                cell = counts.get((origin, destination), {})
                by_mode = [cell.get(code, 0) for code, _ in MODES]
                total = cell.get("0", 0)
                if sum(by_mode) != total:
                    sys.exit(
                        f"{origin} -> {destination}: modes sum to {sum(by_mode)}, "
                        f"the source's total is {total}"
                    )
                origin_name, origin_latitude, origin_longitude = centroids[origin]
                dest_name, dest_latitude, dest_longitude = centroids[destination]
                writer.writerow(
                    [
                        origin,
                        origin_name,
                        f"{origin_latitude:.6f}",
                        f"{origin_longitude:.6f}",
                        destination,
                        dest_name,
                        f"{dest_latitude:.6f}",
                        f"{dest_longitude:.6f}",
                        total,
                        *by_mode,
                    ]
                )
    print(f"wrote {path} ({len(CORRIDOR) ** 2} rows)")


# --- the boarding points ---------------------------------------------------------------------


def overpass_query(boundaries) -> str:
    latitudes = [
        point[1] for code in CORRIDOR for ring in boundaries[code] for point in ring
    ]
    longitudes = [
        point[0] for code in CORRIDOR for ring in boundaries[code] for point in ring
    ]
    box = (
        f"{min(latitudes) - BBOX_PAD_DEGREES:.4f},{min(longitudes) - BBOX_PAD_DEGREES:.4f},"
        f"{max(latitudes) + BBOX_PAD_DEGREES:.4f},{max(longitudes) + BBOX_PAD_DEGREES:.4f}"
    )
    return f"""[out:json][timeout:180];
(
  node["railway"~"^(station|halt)$"]({box});
  nwr["amenity"="bus_station"]({box});
  nwr["park_ride"]({box});
  node["highway"="bus_stop"]({box});
);
out center tags;
"""


def kind_of(tags) -> str | None:
    """Which sort of boarding point this is, or `None` for something that is not one.

    A railway station needs a national-rail station code to count. Without that test the
    corridor picks up the Vale of Rheidol heritage line and the Aberystwyth cliff railway, which
    are eleven boarding points nobody commutes from.
    """
    if tags.get("railway") in ("station", "halt"):
        return "rail" if tags.get("ref:crs") else None
    if tags.get("amenity") == "bus_station":
        return "bus_station"
    if tags.get("park_ride") not in (None, "no") and tags.get("name"):
        return "park_ride"
    if tags.get("highway") == "bus_stop":
        return "bus_stop"
    return None


def write_stations(centroids, boundaries) -> None:
    """`data/stations.json`: the corridor's boarding points, and how each was chosen."""
    body = urllib.parse.urlencode({"data": overpass_query(boundaries)})
    response = fetch("https://overpass-api.de/api/interpreter", body)
    elements = json.loads(response)["elements"]

    interchanges, stops = [], {}
    for element in elements:
        kind = kind_of(element.get("tags", {}))
        if kind is None:
            continue
        point = element if element["type"] == "node" else element.get("center", {})
        latitude, longitude = point.get("lat"), point.get("lon")
        if latitude is None:
            continue
        code = area_of(boundaries, latitude, longitude)
        if code is None:
            continue
        name = element["tags"].get("name")
        entry = {
            "name": name,
            "latitude": latitude,
            "longitude": longitude,
            "area_code": code,
            "kind": kind,
            "openstreetmap": f"{element['type']}/{element['id']}",
        }
        if kind == "bus_stop":
            stops.setdefault(code, []).append(entry)
        elif name:
            entry["selected_by"] = "interchange in the corridor"
            interchanges.append(entry)

    chosen = list(interchanges)
    for code in CORRIDOR:
        _, latitude, longitude = centroids[code]
        candidates = stops.get(code, [])
        if not candidates:
            # ponytail: the corridor has bus stops in every area, so the fall-back of
            # placing a station at the centroid has never fired. Write it the day an area
            # without one is added, and record which stations are placeholders.
            sys.exit(f"{code} has no boarding point; place one at its centroid by hand")
        nearest = min(
            candidates,
            key=lambda stop: (
                haversine_km(
                    (latitude, longitude), (stop["latitude"], stop["longitude"])
                ),
                stop["name"] or "",
            ),
        )
        nearest["selected_by"] = "nearest bus stop to the area's centroid"
        nearest["name"] = nearest["name"] or f"stop {nearest['openstreetmap']}"
        chosen.append(nearest)

    # Highest-ranked first, so the survivor of a near-duplicate pair is the better interchange,
    # then by name so the file is the same whatever order Overpass answered in.
    chosen.sort(key=lambda s: (KIND_RANK.index(s["kind"]), s["name"]))
    stations = []
    for station in chosen:
        here = (station["latitude"], station["longitude"])
        if any(
            haversine_km(here, (k["latitude"], k["longitude"])) < DEDUPE_KM
            for k in stations
        ):
            continue
        stations.append(station)
    stations.sort(key=lambda s: s["name"])

    path = DATA / "stations.json"
    path.write_text(
        json.dumps(
            {
                "source": "OpenStreetMap, queried through Overpass",
                "licence": "Open Database Licence 1.0",
                "attribution": "© OpenStreetMap contributors",
                "corridor": CORRIDOR,
                "stations": stations,
            },
            indent=2,
            ensure_ascii=False,
        )
        + "\n"
    )
    print(f"wrote {path} ({len(stations)} stations)")
    for station in stations:
        print(f"  {station['name']:22s} {station['kind']:12s} {station['area_code']}")


def main() -> None:
    centroids = fetch_centroids()
    boundaries = fetch_boundaries()
    write_flows(centroids)
    write_stations(centroids, boundaries)
    print("\nnow run `cargo run -- build-cache` to extend data/routes.json")


if __name__ == "__main__":
    main()
