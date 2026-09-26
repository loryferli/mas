# The study area, and the data it comes from

Everything in this directory is committed, fetched by hand, and never touched during a run.
Two commands produce it, both run by a person and neither run by `cargo test`:

```bash
uv run scripts/fetch-data.py        # flows.csv and stations.json
cargo run -- build-cache            # routes.json, over the stations the scenarios name
```

`tests/study_area.rs` checks what is here against what the scenarios ask for, and asserts that no
Rust source in the repository names any of the hosts below or spawns a process. That is the offline
guarantee: a run is a pure function of its scenario, its seed and these files.

---

## The corridor, and why this one

The research question is whether a ridesharing service can survive on a network carrying very
little traffic, so the study area has to genuinely carry very little. The criteria, fixed before
the area was picked:

* a rural travel-to-work catchment of a handful of areas, tens of thousands of people rather than
  millions;
* one obvious employment destination, so the corridor has a direction rather than a scatter;
* long thin flows into it, so the demand on any one pair is small enough that a shared ride is a
  real coordination problem rather than a queue.

**North Ceredigion into Aberystwyth**, Mid Wales. Five middle-layer super output areas, 44,485
residents at the 2011 Census, and one town that takes the work:

| Area | Name | Population | Working residents | Of them, into Aberystwyth |
|---|---|---:|---:|---:|
| W02000116 | Ceredigion 001 - Borth, Bow Street, Tirymynach | 7,660 | 2,467 | 63% |
| W02000117 | Ceredigion 002 - Aberystwyth town | 9,998 | 2,367 | 82% |
| W02000118 | Ceredigion 003 - Aberystwyth south and Llanbadarn | 7,761 | 2,369 | 76% |
| W02000120 | Ceredigion 005 - Llanrhystud and the coast road south | 7,344 | 2,075 | 37% |
| W02000421 | Ceredigion 011 - the Rheidol and Ystwyth valleys | 11,722 | 3,364 | 56% |

The selection rule is the last column: **an area is in the corridor if at least a quarter of its
working residents work in the two Aberystwyth areas.** Ceredigion's other four areas fall below it
- 18%, 15%, 14% and 7% - because they commute to Cardigan and Lampeter instead, and are a different
corridor. Nothing was chosen for its scenery: the criterion is a share of a flow, and it is
reproducible from `flows.csv` alone.

**How empty this is.** 11,772 commutes run between the five areas in total, and the largest single
pair is 1,235 people a day - the Rheidol and Ystwyth valleys into Aberystwyth town, spread over a
morning across 13 kilometres of the A4120 and A44. 62% of them drive a car alone. That is the
low-flow case the engine exists to model.

**The external check the mode breakdown buys.** The source reports *driving a car or van* and
*passenger in a car or van* separately, so the corridor arrives with an observed car-passenger
share: **805 of 8,094 car commutes, 9.95%**, or 6.8% of all commuting. It is not a target to tune
against - tuning to it would make the model circular - it is the one outside number available for
asking whether a modelled participation rate is plausible at all. `mean_occupancy` in a run is the
figure to hold next to it.

Corridor totals, from `flows.csv`:

| Mode | People | Share |
|---|---:|---:|
| driving a car or van | 7,289 | 61.9% |
| on foot | 2,696 | 22.9% |
| passenger in a car or van | 805 | 6.8% |
| bus, minibus or coach | 535 | 4.5% |
| bicycle | 251 | 2.1% |
| motorcycle, scooter or moped | 80 | 0.7% |
| taxi | 66 | 0.6% |
| train | 33 | 0.3% |
| underground, metro, light rail, tram | 3 | 0.0% |
| other | 14 | 0.1% |
| **total** | **11,772** | |

---

## `flows.csv` - the demand table

One row per **ordered pair** of corridor areas, so 25 rows, each carrying the pair's total commute,
its split by mode, and both areas' population-weighted centroids.

* **Source** - Nomis, 2011 Census table **WU03EW**, *Location of usual residence and place of work
  by method of travel to work (MSOA level)*, dataset `NM_1208_1`. Crown copyright, released under
  the **Open Government Licence v3.0**, which permits redistributing a derived extract with
  attribution.
* **Vintage** - 2011, **not 2021, and deliberately.** The 2021
  Census origin-destination tables are published at middle-layer super output area resolution only
  as *safeguarded* data, which needs an application and cannot be committed to a public repository;
  the open 2021 release is local-authority resolution, which is a whole county per cell and cannot
  describe a corridor. WU03EW is the most recent openly licensed area-to-area flow table with a
  mode breakdown, and the mode breakdown is the point of choosing this source at all. A reader
  should read the absolute counts as *the shape of a rural commute*, not as this morning's traffic.
* **Universe** - all usual residents aged 16 and over in employment the week before the census.
* **Exact filter** - `USUAL_RESIDENCE` and `PLACE_OF_WORK` both restricted to the five corridor
  codes; `TRANSPORT_POWPEW11` set to the row total and all eleven modes; `MEASURES=20100`. The
  query is built in `scripts/fetch-data.py`, which is the executable copy of this paragraph.
* **What the filter excludes, and why a column is all zeros** - asking for area-to-area flows
  leaves out the source's four special workplace codes: works mainly at or from home, no fixed
  place of work, offshore installation, and outside the United Kingdom. That is why
  `work_at_home_count` is zero on every row. The column is kept rather than dropped so the mode
  columns still sum to `total_count` - `tests/study_area.rs` asserts they do - and so the exclusion
  is stated rather than lost.
* **Disclosure control** - the census swaps records between areas to protect against identifying
  anyone, so small cells are approximate and a few are zero where a real person travelled. It does
  not matter at the grain used here, and it is why nothing downstream treats a single cell as
  exact.

The centroids are the **population-weighted centroids** of the December 2011 areas, from the ONS
Open Geography Portal (`MSOA_Dec_2011_PWC_in_England_and_Wales_2022`), Crown copyright, Open
Government Licence v3.0. They are where the people are, not where the polygon's middle is, which
matters in areas this large and this empty.

---

## `stations.json` - the boarding points

Where a line can start or end. **Source:** OpenStreetMap through the Overpass API, © OpenStreetMap
contributors, **Open Database Licence 1.0** - which requires attribution and is why the file
carries it as a field rather than only here.

The area boundaries used to attribute a stop to an area are the generalised clipped December 2011
boundaries from the same ONS portal, under the same licence.

**The selection rule**, in two parts, so the set is derived rather than hand-picked:

1. every **national-rail station, bus station and named park-and-ride site** standing inside a
   corridor area; plus
2. the **bus stop nearest each corridor area's population-weighted centroid** - there are 229 bus
   stops in the corridor and a corridor of 229 stations is not a corridor.

Then two rules that keep the set honest:

* a railway station counts only if it carries a national-rail station code. Without that test the
  corridor picks up the Vale of Rheidol heritage line and the Aberystwyth cliff railway: eleven
  boarding points nobody commutes from.
* two points within 300 m are the same interchange described twice - a bus station in a railway
  station's forecourt, a stop on the pavement outside it. The higher-ranked survives, which is why
  Aberystwyth is one station rather than three.

Seven stations come out of it:

| Station | Kind | Area | Chosen by |
|---|---|---|---|
| Aberystwyth | rail | W02000117 | interchange in the corridor |
| Borth | rail | W02000116 | interchange in the corridor |
| Bow Street | rail | W02000116 | interchange in the corridor |
| Dolau | bus stop | W02000116 | nearest to the area's centroid |
| Glan Rheidol | bus stop | W02000118 | nearest to the area's centroid |
| Laura House | bus stop | W02000120 | nearest to the area's centroid |
| Trawscoed Bridge | bus stop | W02000421 | nearest to the area's centroid |

Every corridor area has at least one, so the fall-back - placing a station at an area's
centroid where the corridor has too few - has never fired. `scripts/fetch-data.py` stops with a
message rather than inventing one, and `selected_by` records the rule for each station so a
placeholder would be visible the day one is needed.

---

## `routes.json` - the route cache

Road geometry and the routing server's own durations for every directed station pair any committed
scenario can ask for: 42 pairs across the seven corridor stations, plus the two of the synthetic
`minimal.json`. Built by `cargo run -- build-cache`, which shells out to `curl` against the public
Open Source Routing Machine demo server (`router.project-osrm.org`), itself routing over
OpenStreetMap data - © OpenStreetMap contributors, ODbL 1.0.

Measured on this cache, the corridor's roads run **626.0 km against 527.8 km of straight line, a
detour factor of 1.19**, which is what the corridor scenarios set `road_detour_factor` to. The
built-in default stays 1.3, the figure measured on the synthetic corridor, so `minimal.json` is
untouched.

---

## Re-fetching

Both fetchers are idempotent and their output is sorted, so re-running them on unchanged upstream
data produces the same bytes. Upstream does change: OpenStreetMap is edited continuously, so a
bus stop can be renamed or moved and the station set can shift. That is exactly why the extract is
committed rather than fetched at run time - and why a diff in `stations.json` has to be read before
it is accepted, because it moves every figure downstream of it.
