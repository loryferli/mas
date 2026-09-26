#!/usr/bin/env python3
"""Fetch the Lyon study area's demand table and boarding points, and commit them under `data/lyon/`.

Run by hand, never by the test suite, like `scripts/fetch-data.py`:

    uv run scripts/fetch-lyon.py

Writes `data/lyon/flows.csv` (the thousand largest motorised commuting flows between Rhône
communes, with each end's centroid) and `data/lyon/stations.json` (the boarding points of the
Bourgoin-Jallieu - Lyon lane and of the seventeen-station network, and the two points the lane's
drivers leave from and go to). `data/lyon/README.md` carries the provenance and every selection
rule; this script is its executable half, so a rule changed here changes there too.

Three sources, all open, none reached during a run:

  * the flows       INSEE, recensement 2021, fichier détail MOBPRO, Licence Ouverte 2.0
  * the centroids   Etalab contours administratifs 2021 (from IGN Admin Express), Licence Ouverte 2.0
  * two stops       OpenStreetMap via Overpass, Open Database Licence 1.0
"""

import csv
import gzip
import io
import json
import math
import sys
import urllib.parse
import urllib.request
import zipfile
from collections import defaultdict
from pathlib import Path

DATA = Path(__file__).resolve().parent.parent / "data" / "lyon"

MOBPRO = "https://www.insee.fr/fr/statistiques/fichier/8205896/RP2021_mobpro.zip"
CONTOURS = (
    "https://etalab-datasets.geo.data.gouv.fr/contours-administratifs/2021/geojson/"
    "communes-100m.geojson.gz"
)
OVERPASS = "https://overpass-api.de/api/interpreter"

# --- the demand rule -------------------------------------------------------------------------
#
# Home and work both in the Rhône; a Lyon resident is placed in its arrondissement, as the work
# end already is; "motorised" is MOBPRO's modes 4 (two-wheeler) and 5 (car, van, lorry).
DEPARTMENT = "69"
MOTORISED = {"4", "5"}
MIN_KM, MAX_KM = 5.0, 50.0
MIN_FLOW = 10.0
# Of the pairs that pass, the largest thousand. See README: that is what the scenarios hold.
PAIRS = 1000

# --- the boarding points ---------------------------------------------------------------------
#
# The network's stations are named places, and a named place is its 2021 commune's centroid -
# except the two that name one specific stop, which is found in OpenStreetMap by its own name.
# Belleville and Saint-Priest are in the variant network only.
NETWORK_COMMUNES = {
    "Villefranche-sur-Saone": "69264",
    "Anse": "69009",
    "Limonest": "69116",
    "Lentilly": "69112",
    "Fleurieux": "69086",
    "Ecully": "69081",
    "Caluire": "69034",
    "Villeurbanne": "69266",
    "Meyzieu": "69282",
    "Saint-Laurent-de-Mure": "69288",
    "Oullins": "69149",
    "Brignais": "69027",
    "Mornant": "69141",
    "Francheville": "69089",
    "Craponne": "69069",
    "Belleville": "69019",
    "Saint-Priest": "69290",
}
# The lane's two stops, which the network's Mermoz and Bourgoin also are. Both are tagged
# amenity=car_pooling and are found by name inside a small box around where they are expected, so
# a renamed or moved stop fails loudly rather than resolving to something else.
STOPS = {
    "Bourgoin": ("La Grive P+R", (45.58, 5.20, 45.62, 5.26)),
    "Mermoz": ("Lyon - Mermoz-Pinel", (45.72, 4.87, 45.74, 4.90)),
}
# Where the lane's drivers start and finish: a town, and the arrondissement the stop is in.
LANE_PLACES = {"Bourgoin-Jallieu": "38053", "Lyon 8e": "69388"}


def fetch(url: str, body: str | None = None) -> bytes:
    request = urllib.request.Request(
        url,
        data=body.encode() if body else None,
        headers={"User-Agent": "mas data fetch (github.com/loryferli)"},
    )
    with urllib.request.urlopen(request, timeout=600) as response:
        return response.read()


def haversine_km(a, b) -> float:
    radius_km = 6371.0088
    lat_a, lat_b = math.radians(a[0]), math.radians(b[0])
    d_lat, d_lon = lat_b - lat_a, math.radians(b[1] - a[1])
    h = math.sin(d_lat / 2) ** 2 + math.cos(lat_a) * math.cos(lat_b) * math.sin(d_lon / 2) ** 2
    return 2 * radius_km * math.asin(math.sqrt(h))


def centroid(geometry) -> tuple[float, float]:
    """The area centroid of a polygon or multipolygon, holes subtracted, on plain degrees. At a
    commune's size the projection error is metres, well inside the rounding written out."""
    polygons = geometry["coordinates"] if geometry["type"] == "MultiPolygon" else [geometry["coordinates"]]
    area = sum_x = sum_y = 0.0
    for polygon in polygons:
        for index, ring in enumerate(polygon):
            sign = 1 if index == 0 else -1
            for (x0, y0), (x1, y1) in zip(ring, ring[1:]):
                cross = x0 * y1 - x1 * y0
                area += sign * cross
                sum_x += sign * (x0 + x1) * cross
                sum_y += sign * (y0 + y1) * cross
    area /= 2
    return sum_y / (6 * area), sum_x / (6 * area)


def fetch_places() -> dict:
    """Every 2021 commune and arrondissement, as `code -> (name, latitude, longitude)`."""
    features = json.loads(gzip.decompress(fetch(CONTOURS)))["features"]
    places = {}
    for feature in features:
        code = feature["properties"]["code"]
        if code.startswith(DEPARTMENT) or code in LANE_PLACES.values():
            latitude, longitude = centroid(feature["geometry"])
            places[code] = (feature["properties"]["nom"], latitude, longitude)
    return places


def fetch_flows() -> dict:
    """Motorised commuters per (home, work), home at arrondissement level in Lyon."""
    archive = zipfile.ZipFile(io.BytesIO(fetch(MOBPRO)))
    handle = io.TextIOWrapper(archive.open("FD_MOBPRO_2021.csv"), encoding="latin1")
    reader = csv.reader(handle, delimiter=";")
    header = next(reader)
    column = {name: header.index(name) for name in ("COMMUNE", "ARM", "DCLT", "TRANS", "IPONDI")}
    flows = defaultdict(float)
    for row in reader:
        home, work = row[column["COMMUNE"]], row[column["DCLT"]]
        if not (home.startswith(DEPARTMENT) and work.startswith(DEPARTMENT)):
            continue
        if row[column["TRANS"]] not in MOTORISED:
            continue
        if row[column["ARM"]] != "ZZZZZ":
            home = row[column["ARM"]]
        flows[(home, work)] += float(row[column["IPONDI"]])
    return flows


def write_flows(places, flows) -> None:
    kept = []
    for (home, work), flow in flows.items():
        if home == work or flow < MIN_FLOW:
            continue
        if home not in places or work not in places:
            sys.exit(f"no 2021 contour for {home} or {work}")
        distance = haversine_km(places[home][1:], places[work][1:])
        if MIN_KM <= distance <= MAX_KM:
            kept.append((-flow, home, work, distance))
    kept.sort()
    with open(DATA / "flows.csv", "w", newline="") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerow([
            "rank", "origin_code", "origin_name", "origin_latitude", "origin_longitude",
            "destination_code", "destination_name", "destination_latitude",
            "destination_longitude", "distance_km", "motorised_flow",
        ])
        for rank, (flow, home, work, distance) in enumerate(kept[:PAIRS]):
            writer.writerow([
                rank, home, places[home][0], f"{places[home][1]:.6f}", f"{places[home][2]:.6f}",
                work, places[work][0], f"{places[work][1]:.6f}", f"{places[work][2]:.6f}",
                f"{distance:.3f}", f"{-flow:.3f}",
            ])
    print(f"flows.csv: {min(PAIRS, len(kept))} of {len(kept)} qualifying pairs")


def fetch_stop(name, box) -> tuple[float, float, int]:
    south, west, north, east = box
    query = (
        f'[out:json][timeout:60];nwr["amenity"="car_pooling"]["name"="{name}"]'
        f"({south},{west},{north},{east});out center;"
    )
    elements = json.loads(fetch(OVERPASS, "data=" + urllib.parse.quote(query)))["elements"]
    if len(elements) != 1:
        sys.exit(f"expected one car_pooling feature named {name!r}, found {len(elements)}")
    element = elements[0]
    point = element.get("center", element)
    return point["lat"], point["lon"], element["id"]


def write_stations(places) -> None:
    stations = []
    for name, (osm_name, box) in STOPS.items():
        latitude, longitude, osm_id = fetch_stop(osm_name, box)
        stations.append({
            "name": name, "latitude": round(latitude, 6), "longitude": round(longitude, 6),
            "source": f"OpenStreetMap car_pooling {osm_name!r} ({osm_id})",
        })
    for name, code in NETWORK_COMMUNES.items():
        commune, latitude, longitude = places[code]
        stations.append({
            "name": name, "latitude": round(latitude, 6), "longitude": round(longitude, 6),
            "source": f"centroid of {commune} ({code})",
        })
    anchors = []
    for name, code in LANE_PLACES.items():
        commune, latitude, longitude = places[code]
        anchors.append({
            "name": name, "latitude": round(latitude, 6), "longitude": round(longitude, 6),
            "source": f"centroid of {commune} ({code})",
        })
    document = {"stations": stations, "places": anchors}
    (DATA / "stations.json").write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")
    print(f"stations.json: {len(stations)} stations, {len(anchors)} places")


def main() -> None:
    DATA.mkdir(parents=True, exist_ok=True)
    places = fetch_places()
    write_flows(places, fetch_flows())
    write_stations(places)


if __name__ == "__main__":
    main()
