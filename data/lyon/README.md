# The Lyon study area, and the data it comes from

The second study area: the Rhône, and the lane from Bourgoin-Jallieu into Lyon. It is where the
two earlier pieces of work in the top-level README were run, rebuilt here from open data only.
Everything in this directory is committed, fetched by hand, and never touched during a run:

```bash
uv run scripts/fetch-lyon.py                        # flows.csv and stations.json
python3 scripts/build-lyon-scenarios.py OUT [--grid]  # the scenarios, from these two files
```

`fetch-lyon.py` is the executable half of this document: change a rule there and change it here in
the same commit.

## Sources

| What | Source | Licence | Retrieved |
|---|---|---|---|
| Commuting flows by mode | INSEE, recensement de la population 2021, *Mobilités professionnelles des individus : déplacements commune de résidence / commune de travail* - fichier détail `FD_MOBPRO_2021` | Licence Ouverte 2.0 | 2026-09-26 |
| Commune and arrondissement centroids | Etalab, *contours administratifs* 2021, `communes-100m` (derived from IGN Admin Express) | Licence Ouverte 2.0 | 2026-09-26 |
| The two lane stops | OpenStreetMap, via Overpass | Open Database Licence 1.0 | 2026-09-26 |

The contours are the **2021** geography on purpose: MOBPRO 2021 codes communes as they were in
2021, and two of them (Pierre-Bénite, 69152, and Porte des Pierres Dorées, 69159) have since merged
into others and are missing from today's geography.

## `flows.csv` - the demand

One row per directed pair of places, the thousand largest, largest first.

1. **Home and work both in the Rhône** (department 69).
2. **A Lyon resident is placed in its arrondissement** (MOBPRO's `ARM`), as the workplace
   (`DCLT`) already is, so Lyon is nine places and not one.
3. **Motorised only**: MOBPRO modes 4 (motorised two-wheeler) and 5 (car, van, lorry), summed with
   the census weight `IPONDI`.
4. **5 to 50 km** straight line between the two places' centroids; a place's centroid is the area
   centroid of its 2021 contour.
5. **At least 10 commuters** a day.
6. **The thousand largest** of the 5,059 pairs that pass.

The last rule is not in the method the published scenarios describe (rules 1-5), but it is what
those scenarios hold: exactly a thousand pairs, the smallest carrying 62 people. Here the
thousandth carries 55.6, and the thousand carry 139,113 people once each is rounded up (the
`complete` sample).

A scenario samples each pair as `ceil(flow x share)` people per direction, so no pair is empty:
`10` is a share of 0.075, `100` a share of 0.0075, `200` the first 250 pairs at 0.0075, and
`complete` the whole flow. The names are the published ones and do not mean a tenth or a
hundredth.

## `stations.json` - the boarding points

**The lane**: two carpool stops tagged `amenity=car_pooling` in OpenStreetMap, found by their own
names inside a small box around where each is expected - *La Grive P+R* at the A43's exit 7 outside
Bourgoin-Jallieu, and *Lyon - Mermoz-Pinel* at the metro stop in Lyon 8e. The lane's drivers leave
from the centroid of Bourgoin-Jallieu and drive to the centroid of Lyon 8e (`places`).

**The seventeen-station network** is a set of named places, so each station is the 2021 centroid of
the commune it names, except Mermoz and Bourgoin, which name the two lane stops above. Nineteen
names in all: the variant network run at the `100` sample has Belleville and Saint-Priest in place
of Bourgoin and Saint-Laurent-de-Mure. **The network is not derived by any rule here.** Its
stations are named carpool places rather than flow endpoints, so no triangulation of the demand
produces them; the topology (sixteen shared undirected pairs plus two per variant, each run both
ways - 36 lines) is part of the scenario specification in `build-lyon-scenarios.py`.

## How close this is to the inputs of the published runs

Checked by hand against the scenario files those runs used, which are not committed.

- **Lane**: every cohort count is identical, by construction. The stops are within 40 m of the ones
  used; the driver anchors within 1.2 km (Bourgoin-Jallieu) and 0.3 km (Lyon 8e).
- **Advance-declaration grid**: identical counts and settings. The published grid has fourteen
  folders but thirteen settings: the folder named for a 6000 s lead with 600 s margins contains a
  6000 s lead with no margins, a duplicate of another. Several other folder names disagree with
  their contents (the 600 s-lead folders declare a 300 s rider lead); `build-lyon-scenarios.py` names
  settings by what they contain.
- **Demand**: 849 of the thousand published pairs match a pair here with both ends within 2.5 km
  (median 1.2 km, the difference between two centroid sources). Their counts agree to a median of
  1.2%, 148 exactly. The published pairs total 158,975 people against 139,113 here, and include
  pairs as short as 2.3 km, so their distance rule was not the stated straight-line 5 km; no
  variant of residence level, workplace level, modes or distance floor tried here reproduces them
  exactly. **The demand is the published method on the published source, not the published
  numbers**, and every comparison against the published runs has to be read with that.
- **Network stations**: within 3 km of the ones used, the two lane stops within 40 m.

  | Station | km | Station | km | Station | km |
  |---|---:|---|---:|---|---:|
  | Villefranche-sur-Saone | 1.17 | Villeurbanne | 1.47 | Brignais | 1.33 |
  | Anse | 1.55 | Meyzieu | 1.89 | Mornant | 2.86 |
  | Limonest | 0.98 | Mermoz | 0.01 | Francheville | 1.45 |
  | Lentilly | 1.36 | Saint-Laurent-de-Mure | 1.75 | Craponne | 1.85 |
  | Fleurieux | 1.06 | Bourgoin | 0.04 | Belleville | 2.22 |
  | Ecully | 1.10 | Oullins | 2.87 | Saint-Priest | 2.65 |
  | Caluire | 1.25 | | | | |

  The published stations are specific carpool points a kilometre or two from their commune's
  centre; OpenStreetMap tags only some of them, so a centroid is the one rule that covers all
  seventeen.
- **Fleet**: sizes, seats and spawn jitter are the published ones. The fleet stands around the
  largest pair's origin, which is where the published fleet stood too (Villeurbanne, 0.9 km).
