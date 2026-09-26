# Findings

Everything here comes from committed files: on the corridor, `analysis/results.csv` (100 runs),
`analysis/policies.csv` (150), `analysis/networks.csv` (250), `analysis/walking.csv` (150),
`analysis/fleet.csv` (80), `analysis/declaration.csv` (50), `analysis/adoption.csv` (50),
`analysis/approach.csv` (100), and single runs at seed 42 where one is quoted as such; on the second
study area, the `analysis/lyon-*.csv` sweeps, the original simulator's runs under
`analysis/reference/`, and `analysis/agreement.csv`. No row on the corridor hit the clock cap or
stranded an agent, so every figure there is a completed run. The one place a cut-off run is read is
the re-run of the advance-declaration grid *as the original ran it*, whose riders had no patience at
all: there a rider still waiting at the cap is a rider nobody carried, and is counted as one.
Nothing here needs an API key.

**How the comparisons are read.** Each sweep runs the same ten seeds at every axis point, so two
rules can be differenced *per seed* rather than compared as two means with overlapping bands. Where
that matters the paired difference is quoted with its standard deviation and a win/tie/loss count
over the ten seeds. That is not a formal significance test and is not offered as one. It does
distinguish "the means differ and the seeds disagree about which way" from "every seed moved the same
way", and those two are the whole difference between a finding and a coincidence.

---

## The answer: yes, and the interesting part is the price

A ridesharing service on this corridor works. Thirteen enrolled drivers - one car driver in ten of
the observed flow - serve 85% of thirty-two riders, and it saturates at 95% by thirty-two vehicles
and never goes higher at any fleet size tried.

What that costs, in the only unit the engine can price without an invented rate:

| vehicles | service rate | km per passenger served | of which loaded | empty share | occupancy |
|---|---|---|---|---|---|
| 6 | 0.550 | 4.92 | 3.30 | 0.317 | 2.03 |
| 13 | 0.853 | 6.80 | 3.52 | 0.481 | 1.35 |
| 32 | 0.950 | 13.93 | 4.38 | 0.685 | 0.64 |
| 66 | 0.963 | 28.96 | 5.30 | 0.817 | 0.31 |
| 132 | 0.944 | 58.66 | 5.94 | 0.899 | 0.15 |

(Five seats, ten seeds a row, `analysis/results.csv`. `km per passenger` is computed per run and then
averaged, so it is not the ratio of two means.)

**The loaded column barely moves and the total column runs away.** Carrying a passenger costs three
to six kilometres however big the fleet is - that is the corridor's geometry, not the dispatcher -
and everything else is empty running. Between thirteen and thirty-two vehicles the service buys ten
points of service rate for double the distance. Between thirty-two and a hundred and thirty-two it
buys nothing measurable (0.950 → 0.944, and the seed bands overlap completely) for four times the
distance again. **A low-flow ridesharing service is cheap while it is short of vehicles and
ruinous the moment it has enough of them**, and that is the shape the whole research question turns
on.

## Two drivers an hour per station is what the corridor asks for

The vehicle counts above are the fleet a *scenario* declares, which makes them hard to carry
anywhere else. The transferable unit is supply density: the corridor's four driver cohorts all spawn
over a window of exactly one hour, and six of its seven boarding points are line origins - Aberystwyth
is where everyone is going - so a run's supply is **drivers per hour per station**, the fleet divided
by six. `sweeps/adoption.json` sweeps it at powers of two from one to sixteen, holding the
thirty-two riders fixed, splitting each total across the four cohorts in the baseline's own 4:2:2:5
proportion so the geography of supply does not shift as the density rises.

| drivers/hour/station | drivers | service rate | worst seed | km per passenger served | empty share | mean wait (s) |
|---|---|---|---|---|---|---|
| 1 | 6 | 0.550 | 0.375 | 4.92 | 0.317 | 347 |
| 2 | 12 | 0.841 | 0.688 | 6.26 | 0.457 | 216 |
| 4 | 24 | 0.906 | 0.844 | 11.65 | 0.643 | 111 |
| 8 | 48 | 0.953 | 0.906 | 21.89 | 0.755 | 42 |
| 16 | 96 | 0.928 | 0.844 | 45.48 | 0.881 | 11 |

(Ten seeds a row, `analysis/adoption.csv`. The baseline's thirteen drivers are 2.17 on this axis,
which is why the second row and the 0.853 quoted above are the same result read two ways.)

**The knee is at two, and it is the only step that is not arguable.** Doubling from one to two is
+0.291 service rate on a 10W/0T/0L count. Every step after it is smaller: two to four is +0.066 at
6W/2T/2L, four to eight is +0.047 at 8W/1T/1L, and eight to sixteen is -0.025 at 3W/1T/6L, which is
**no distinguishable difference at all** for the reason in the caveat below. The axis stops paying
somewhere between four and eight, and past eight it only spends.

**What is left unserved at the top of the axis is not a supply problem.** Across seeds 1 to 5 at
sixteen drivers an hour per station, every single unserved rider ends `Canceled` for
`NoLineAvailable` - twelve of them, and not one gave up waiting. Those riders were jittered further
from every boarding point than their 2400 s walk budget reaches, so they never registered on a line
and no quantity of vehicles could have collected them. That is the walking-distance ceiling two
sections below, arriving from a different direction: **once density is past four, walking is the only
thing still binding.**

**Cost moves the whole way and never stops.** Kilometres per passenger served rise on every single
step - 4.92, 6.26, 11.65, 21.89, 45.48 - and the empty share worsens on nine or ten seeds out of ten
at every step from two onward. Sixteen drivers an hour per station spends **nine times** the distance
per passenger that one does, and carries no more people than eight did.

So the corridor's answer to "how much supply does this need" is **two drivers an hour per station to
have a service at all, four to have a good one, and nothing above four is worth its distance.** One
is not a service: it leaves nearly two thirds of the riders behind on the worst of the ten seeds. That
is a statement about density rather than about this particular fleet, and it is the form the result
travels in.

**The caveat, and it applies to every sweep in this document that moves a driver count.** A seed
fixes the run, not the demand. All draws are taken up front, cohort by cohort, and the four driver
cohorts are drawn *before* the four rider cohorts, so changing a driver count changes how much
randomness is consumed before the riders are drawn and the riders come out somewhere else entirely -
at seed 1, the thirty-two riders at eight drivers an hour per station and the thirty-two at sixteen
share **none** of their origins. The win/tie/loss counts on this axis therefore pair runs rather than
pairing demand, which is a weaker thing, and a difference of the size of the eight-to-sixteen step is
inside the draw-to-draw spread rather than above it. The steps this section rests on - +0.291 at 10/10,
and a cost column that rises on every seed at every step - are far larger than that. Holding demand
fixed across a supply axis would need the cohorts drawn from separate streams, which is a change to
`Sim::new` and not a change to a sweep config.

## Seats are worth more than vehicles, by a factor of five

The sharpest single result in the sweep, and the one that is nearly free to act on.

At six vehicles, two seats serve 0.163 and five seats serve 0.550 - over **exactly the same 83.85
kilometres**, because those drivers were making those trips anyway and the extra seats cost no
distance at all. At thirteen vehicles, 0.372 against 0.853 over the same 183.10 km.

Getting there by adding vehicles instead: two seats need sixty-six vehicles to reach 0.963, which is
what five seats reach with thirteen - and sixty-six vehicles drive 891.18 km against 183.10. **Five
times the distance for the same service.** Per passenger carried, two seats never get below 15.5 km
at any fleet size, against 4.9 for five seats.

This is a statement about a corridor whose demand is thin and scattered: a vehicle passing a station
is common, two riders wanting the same station at the same time is not. Add capacity to the vehicles
you already have before adding vehicles.

## The ceiling is walking distance, not matching

Service never reaches 1.0 at any fleet size, capacity or dispatch rule in `results.csv`. The binding
constraint is not the dispatcher and not the fleet: it is how far somebody will walk to a stop.

`sweeps/walking.json` moves the rider cohorts' `max_walk_s` - which prices *both* flanking legs, so
2400 s is twenty minutes at each end - against driver adoption, ten seeds, `analysis/walking.csv`:

| `max_walk_s` | 6 vehicles | 13 | 32 | km per passenger at 32 |
|---|---|---|---|---|
| 1200 s | 0.278 | 0.381 | 0.375 | 37.91 |
| 1800 s (the engine's default) | 0.466 | 0.744 | 0.778 | 17.09 |
| **2400 s (the committed corridor)** | **0.550** | **0.853** | **0.950** | **13.93** |
| 3000 s | 0.575 | 0.891 | 0.988 | 13.38 |
| 3600 s | 0.578 | 0.891 | 0.988 | 13.38 |

Three things fall out of that table, and the first is the important one.

**At twenty minutes of total walking, adding vehicles stops working.** Service goes 0.278 → 0.381 →
0.375 across a fleet that more than quintuples: thirty-two vehicles serve no more people than
thirteen, and cost 37.91 km per passenger against 16.94. A rider with no line inside its own walking
threshold never registers, so no amount of supply can reach it. **A walking problem cannot be
solved with vehicles**, and a service that tried would spend five times the distance discovering
that.

**The corridor's 2400 s is a real assumption, and it is not a generous one.** Against the engine's
1800 s default it is worth +0.109 of service at thirteen vehicles and +0.172 at thirty-two, on ten
seeds out of ten. Against 3000 s it *costs* 0.037 at thirty-two vehicles (8W/2T/0L) - so the
committed figures sit slightly below what an unlimited walk would give, not above it.

**And the constraint saturates by fifty minutes.** 3000 s and 3600 s produce identical service at
every driver count, so beyond that the walk is no longer what binds; the riders still unserved are
the ones on the 23 km Laura House leg who give up before anything gets there.

The boarding points sit up to two kilometres from where the population actually is, because that is
where boarding points are in rural Wales: three rail stations, and four road stops chosen by rule
from OpenStreetMap. Every service figure in this document rests on forty minutes of walking being
acceptable, and that is an assumption about people rather than a measurement.

## The dwell at the station is what the service is made of

Every figure above has a registered driver drive to its line's station and wait there, up to its
patience, for riders to arrive. The other reading of "registered" is a driver that drives its own
route and stops only if somebody is already standing at a station ahead of it - `station_approach:
on_demand`, the way a driver who never looks at the app until it is passing would behave.
`sweeps/approach.json` runs both over the driver axis, ten seeds, `analysis/approach.csv`:

| Drivers | Service, waiting at the station | Service, stopping on demand | Paired | Kilometres saved |
|---|---|---|---|---|
| 6 | 0.550 | 0.116 | -0.434, 0W/0T/10L | 24.1 |
| 13 | 0.853 | 0.256 | -0.597, 0W/0T/10L | 51.0 |
| 32 | 0.950 | 0.463 | -0.487, 0W/0T/10L | 118.2 |
| 132 | 0.944 | 0.694 | -0.250, 0W/0T/10L | 511.0 |

**Stopping on demand loses between a quarter and three fifths of the service at every fleet size,
on every seed.** It saves distance - no detours to stations nobody is at, no dwell - but the
distance it saves was carrying people: empty share rises from 0.481 to 0.805 at thirteen drivers,
and the riders it does carry wait 680 s instead of 205 s. On a low-flow corridor a rider and a
driver passing the same stop in the same minute is the exception, and the dwell is what turns a
near miss into a ride. **What a registered driver is asked to do at the station matters more than
anything the dispatcher decides**, and every other figure in this document is read under the
generous answer.

## Direction beats density

Five constructions derive a line set from the corridor's seven stations: free-form (all 21 pairs),
Delaunay (14), flow-ranked (13, ranked by the size of the commute), the minimum spanning tree (6),
and flow-directed (flow-ranked's 13 pairs, each **oriented** by the way the commute between its two
areas runs, and taken one way only). Every pair becomes two directed lines except under
flow-directed, where each is one. `baseline.json`'s hand-declared network is six *one-way* lines,
all of them into Aberystwyth.

**Halving the network is free.** Flow-ranked's 13 pairs against free-form's 21, paired per seed:

| vehicles | Δ service rate | W/T/L |
|---|---|---|
| 6 | +0.009 (sd 0.015) | 3/7/0 |
| 13 | +0.006 (sd 0.013) | 2/8/0 |
| 32 | +0.009 (sd 0.015) | 3/7/0 |
| 66 | −0.006 (sd 0.013) | 0/8/2 |
| 132 | −0.003 (sd 0.010) | 0/9/1 |

Seven or eight seeds in ten produce *identical* service rates, and distance differs by under 5 km
throughout. Reading the demand to pick which pairs to connect buys almost nothing over connecting
everything - but it costs nothing either, and it is eight fewer lines to operate.

**Quartering it is not free.** Delaunay and the tree lose 0.18 to 0.24 of service against free-form
at every driver count above six, on **ten seeds out of ten**. The tree does drive the fewest
kilometres at every point - 36.8 km fewer than free-form at 132 vehicles - but only because it
serves the fewest people; its occupancy is the worst of the four throughout.

**And every symmetric construction loses to six hand-declared one-way lines.** Paired per seed,
`baseline.json` against each construction on service rate:

| construction | 6 vehicles | 13 | 32 | 66 | 132 |
|---|---|---|---|---|---|
| free-form (21 pairs) | +0.106, 10/0/0 | +0.109, 10/0/0 | +0.053, 8/2/0 | +0.013, 4/6/0 | −0.006, 0/8/2 |
| flow-ranked (13) | +0.097, 10/0/0 | +0.103, 10/0/0 | +0.044, 7/3/0 | +0.019, 6/4/0 | −0.003, 0/9/1 |
| Delaunay (14) | +0.128, 7/2/1 | +0.234, 10/0/0 | +0.269, 10/0/0 | +0.212, 10/0/0 | +0.178, 10/0/0 |
| tree (6) | +0.072, 6/2/2 | +0.222, 10/0/0 | +0.278, 10/0/0 | +0.253, 10/0/0 | +0.219, 10/0/0 |
| **flow-directed (13, one-way)** | **+0.084**, 9/1/0 | **+0.056**, 8/2/0 | **+0.028**, 6/4/0 | +0.013, 4/6/0 | −0.003, 0/9/1 |

Six directed lines beat forty-two directed lines, every seed, wherever the service is short of
vehicles - and only stop beating them once there are so many vehicles that the ranking finds
everybody anyway.

**The mechanism is direction, and it is not subtle.** Every symmetric construction connects
whatever it connects both ways. On a radial morning commute that means half its lines run *away*
from the work, and the operator's ranking sometimes picks one. The hand-declared network has no such
lines because a person knew which way people were going.

**Orienting a construction by the demand recovers about half of that, and costs nothing.**
`flow-directed` is `flow-ranked`'s pairs with each one taken only in the direction the table says the
commute runs, so exactly one variable moves between the two: thirteen lines instead of twenty-six,
the same thirteen station pairs. Paired per seed:

| vehicles | flow-ranked | flow-directed | Δ service rate | W/T/L | Δ km driven |
|---|---|---|---|---|---|
| 6 | 0.453 | 0.466 | +0.013 (sd 0.040) | 1/9/0 | +0.03 km |
| 13 | 0.750 | **0.797** | **+0.047** (sd 0.052) | 6/4/0 | +0.24 km |
| 32 | 0.906 | 0.922 | +0.016 (sd 0.022) | 4/6/0 | +0.44 km |
| 66 | 0.944 | 0.950 | +0.006 (sd 0.013) | 2/8/0 | +0.92 km |
| 132 | 0.947 | 0.947 | +0.000 (sd 0.000) | 0/10/0 | +1.62 km |

**Not one seed at any driver count goes the other way**, and the distance difference is under two
kilometres on runs of hundreds, so the orientation is free. At thirteen vehicles it closes 46% of the
gap between the best symmetric construction and the hand-declared network (0.103 → 0.056).

**Why only half.** `flow-directed`'s thirteen lines are the hand-declared six - every corridor
station into Aberystwyth - **plus seven more** that run between outlying areas, because the table's
second-biggest flows are real: 533 people a day travel from Ceredigion 001 into Ceredigion 003, and
321 into Ceredigion 011. Those seven are honest demand and still the wrong lines for this service:
the operator ranks a line by what it costs, so a driver bound for town is sometimes handed one of
them. Direction is necessary and **not** sufficient - the surplus lines still mislead the ranking,
which is the same finding as *halving the network is free* seen from the other side.

So the hypothesis comes back negative and the reason is now measured rather than inferred.
Structuring the network does not pay on this corridor; *deriving* it from geometry actively hurts;
reading how big a flow is buys nothing; reading **which way it runs** buys half of what a person
drawing six lines by hand achieves. What is still unmeasured is whether anything closes the other
half, and that is the question `scripts/design-network.py` puts to the model - now against a bar of
0.797 rather than 0.750.

## Following demand does not pay, and does not fail cleanly either

`greedy` drops the shared line-ranking yardstick and drives to the line with riders standing on it.
Against `heuristic`, paired per seed:

| vehicles | Δ service rate | Δ km driven | Δ mean wait | Δ empty share |
|---|---|---|---|---|
| 6 | +0.028 (sd 0.132) 5/2/3 | +3.6 km, 6/0/4 | +109 s, 8/0/2 | **+0.277, 10/0/0** |
| 13 | +0.006 (sd 0.069) 4/1/5 | +22.2 km, 8/0/2 | +110 s, 7/0/3 | **+0.144, 10/0/0** |
| 32 | −0.037 (sd 0.046) 1/3/6 | **+233.8 km, 10/0/0** | +108 s, 10/0/0 | **+0.155, 10/0/0** |
| 66 | −0.022 (sd 0.039) 0/6/4 | +152.7 km, 10/0/0 | +61 s, 10/0/0 | **+0.078, 10/0/0** |
| 132 | −0.019 (sd 0.030) 0/6/4 | +137.3 km, 10/0/0 | +29 s, 10/0/0 | **+0.041, 10/0/0** |

Read the columns in the right order. **On service rate, greedy never wins and never clearly loses**:
at six and thirteen vehicles the seeds disagree about the sign (5W/3L, 4W/5L) and the standard
deviation is two to twenty times the mean difference. At thirty-two and above the sign is consistent
but the effect is small. **On cost, greedy loses on every seed at every driver count.** Its empty
distance share is worse ten times out of ten, everywhere, and at thirty-two vehicles it drives 234
extra kilometres - 55% more - for 0.037 *less* service.

So the answer is: following demand buys nothing here and costs a great deal. A driver that drifts
to the line with somebody standing on it is a driver going out of its way, and on a corridor where
the lines are 23 km long that is the dominant term.

**This corrects an earlier reading of the same data.** The two rules appear to cross over - greedy's
mean is ahead at six and thirteen vehicles and behind at thirty-two and above - and that crossover
was previously taken as evidence of two heuristics each seeing what the other misses. Paired on
seed, it is not there: the low-end lead is noise, and only the high-end cost difference survives.
The mechanism argument still holds - greedy follows only demand *already standing at a station*,
which is why it wastes distance - but it is now an argument about greedy's weakness rather than
about a genuine trade-off between two rules. Anything downstream that leaned on the crossover leans
on nothing. `hybrid`'s "ask the model where the two fixed rules disagree" is still a defensible
place to spend a call, but it is a choice about where a model might help, not a conclusion drawn
from a measured crossover, and it should be described that way.

The honest reading of both rules is that **neither is short of a mechanism the other has; both are
short of pair scoring.** A match is keyed on the line both parties registered on, so a rider whose
best line has no driver, and whose second has a driver that never hears about it, goes unserved
under either rule. That is the next section, and it is now built and measured.

## Pair scoring is the demand-following rule done properly, and it only pays where vehicles are scarce

`pairs` drops the line as the match key. A rider still registers on the line it ranked first and
still starts walking there, but it is no longer *owned* by that line: any driver may take it -
`Influence::Claim`, the mechanism advance declaration already used - provided the walk it would
still have to make, from wherever it has got to, is inside the `max_walk_s` **it** declared. The
driver's half of the score is its own detour as a fraction of driving straight there, the rider's
half is that walk as a fraction of its own threshold, and the two are added. Neither party reads
the other's unit and neither threshold is overruled.

Against `heuristic`, paired per seed over the same ten:

| vehicles | Δ service rate | Δ km driven | Δ empty share | Δ occupancy |
|---|---|---|---|---|
| 6 | **+0.075** (sd 0.101) 7/1/2 | +19.4 km, 8/0/2 | +0.261, 9/0/1 | −0.573, 0/0/10 |
| 13 | **+0.053** (sd 0.086) 8/0/2 | +66.4 km, **10/0/0** | +0.155, **10/0/0** | −0.347, 0/0/10 |
| 32 | +0.000 (sd 0.021) 2/6/2 | +209.3 km, 9/0/1 | +0.139, **10/0/0** | −0.195, 0/0/10 |
| 66 | −0.037 (sd 0.044) 0/4/6 | +157.7 km, 9/0/1 | +0.069, **10/0/0** | −0.062, 0/0/10 |
| 132 | −0.025 (sd 0.025) 0/4/6 | +194.4 km, 7/0/3 | +0.034, 9/0/1 | −0.024, 1/0/9 |

**This is the first rule in this repository that beats `heuristic` on service with a stable sign, and
it does it exactly where it was expected to.** Below thirteen vehicles the binding
constraint is that a rider's chosen line has nobody on it; releasing the rider to any driver that
can reach it turns 0.550 into 0.625 and 0.853 into 0.906. Above thirty-two vehicles there is a
driver on every line already, so there is nothing to release, and diverting riders only moves them
about: the sign flips and stays flipped, 0 seeds up out of 10 at both 66 and 132.

**It does not beat `heuristic` on cost anywhere, and it is not close.** Empty distance share is worse
ten seeds out of ten at three of the five driver counts and occupancy is worse ten out of ten at
four of them. Per passenger served it drives 8.63 km at thirteen vehicles against `heuristic`'s 6.80.
Sending a driver to collect a rider that was walking somewhere else is a real detour, paid in empty
kilometres, and the rider's second walk is real too.

**Against `greedy` it dominates**, which is the cleaner comparison because both rules follow demand:

| vehicles | Δ service rate | Δ km driven | Δ mean wait |
|---|---|---|---|
| 6 | +0.047 (sd 0.071) 6/1/3 | +15.8 km, 8/0/2 | −36 s, 3/0/7 |
| 13 | +0.047 (sd 0.066) 8/0/2 | +44.2 km, 9/0/1 | −58 s, 3/0/7 |
| 32 | +0.037 (sd 0.044) 7/2/1 | −24.6 km, 3/0/7 | −43 s, 2/0/8 |
| 66 | −0.016 (sd 0.065) 3/3/4 | +5.0 km, 4/0/6 | −37 s, **0/0/10** |
| 132 | −0.006 (sd 0.020) 1/6/3 | +57.1 km, 7/0/3 | −3 s, 3/0/7 |

More service where vehicles are scarce - the sign is solid at thirteen and thirty-two drivers (8/0/2
and 7/2/1) and only suggestive at six (6/1/3) - and shorter waits at every driver count. On distance
it is level: +44 km at thirteen, **−25 km at thirty-two**, +5 at sixty-six, +57 at a hundred and
thirty-two, so it buys that service without `greedy`'s systematic surcharge rather than under it.
**If a service is going to follow demand at all, this is the rule to follow it with** - but the finding above stands: on this corridor, at the
adoption levels the demand table supports, not following demand is cheaper than either.

**Three things this measurement does not say.** It is greedy per driver, with no view of the rest of
the fleet, so two drivers can reach for the same rider and the apply pass gives it to the first - a
fleet-wide assignment is the same computation over a different pool and is not built. **What that
costs is now measured rather than guessed**: `Sim::claim_collisions` counts every claim that lost
such a race, and on `baseline.json` at seed 42 it is **10 in a whole run** at thirteen drivers and
409 at ten times that fleet, where service is saturated anyway. A driver that loses re-decides on
the next tick, which is one second of its waiting, so the collisions cost seconds and not rides -
which is why the fleet-wide version stays unbuilt. What is *not* measured is the ordering: within a
tick the rider goes to whichever colliding driver sits earlier in the arena rather than to the
cheapest pair. A rider is
diverted at most once, and only while it is still walking; a rider already standing at a station is
left alone, because re-entering a walking state would restart the clock its patience is measured
against and hand it a fresh half hour. And the two halves of the score are weighed one to one, which
is a rate of exchange between a fraction of driving and a fraction of walking that no measurement
here fixes.

## Advance declaration buys the wait and sells the ride, and half adoption is the worst of both

A declaring rider stays where it is until a driver whose window overlaps its own claims it, and only
then walks - to that driver's station, which need not be its first choice. The rendezvous stops
being a number both sides have to compute identically and becomes an intersection. What the rider
gets is being told when to start walking instead of standing at a stop hoping. What it risks is that
the claim never comes, in which case it never walks at all and gives up `max_wait_s` after
declaring.

`analysis/declaration.csv` is fifty runs over that trade: `mixed-declaration.json` splits every
cohort into a declaring half and a non-declaring one, and the axis moves the split from nobody to
everybody on **both sides at once**, because a declaring rider can only be claimed by a declaring
driver. Thirteen drivers and thirty-two riders throughout, ten seeds a point. The ends of the axis
are pinned to committed scenarios by a test: at 0% the run *is* `baseline.json`, at 100% it *is*
`advanced-declaration.json`, agent for agent.

| declaring | drivers / riders | service rate | mean wait | km driven | occupancy |
|---|---|---|---|---|---|
| 0% | 0/13, 0/32 | **0.853** | 205 s | 183.1 | 1.35 |
| 25% | 4/13, 8/32 | 0.738 | 158 s | 180.2 | 1.18 |
| 50% | 7/13, 17/32 | 0.700 | 83 s | 182.5 | 1.14 |
| 75% | 11/13, 25/32 | 0.694 | 55 s | 182.6 | 1.08 |
| 100% | 13/13, 32/32 | 0.753 | **17 s** | 183.1 | 1.19 |

Paired against the same seeds at 0%:

| declaring | Δ service rate | W/T/L | Δ mean wait | W/T/L |
|---|---|---|---|---|
| 25% | −0.116 (sd 0.078) | **0/0/10** | −47 s (sd 133) | 4/0/6 |
| 50% | −0.153 (sd 0.111) | **0/0/10** | −122 s (sd 99) | 1/0/9 |
| 75% | −0.159 (sd 0.111) | 1/0/9 | −150 s (sd 105) | 1/0/9 |
| 100% | −0.100 (sd 0.119) | 1/1/8 | −188 s (sd 100) | **0/0/10** |

**The wait falls monotonically and the service never recovers.** Twelve times less waiting at the
station at full adoption, on ten seeds out of ten - the mechanism works, and it works exactly as
advertised. It costs a tenth of the service at full adoption and **more than that at every partial
level**, with the worst point at 75%. Distance does not move at all: 180 to 183 km across the whole
axis, so this is not a cheaper service, it is the same driving rearranged.

**Partial adoption is worse than either end, and that is the finding.** A declaring rider is invisible
to a driver that does not declare - it is standing at no station, so nothing but a claim reaches it -
so at 25% adoption eight riders are competing for four drivers while twenty-four compete for nine.
The market is split in two and the smaller half is short of vehicles. The arithmetic is consistent
with the whole loss falling on the declaring riders (an extra 3.7 unserved out of 8 at 25%, 4.9 out
of 17 at 50%), though the engine does not break service down by cohort, so that last step is
inference rather than measurement.

That answers, on this corridor, the question the earlier work in the README could not
settle: **advance declaration is not a service-rate lever, it is a waiting-time lever, and it needs
near-total adoption before it stops actively hurting.** Margins too tight to meet cost far more than
any of this - `advanced-declaration.json` at ±120 s serves 6 of 32.

## The on-demand fleet loses on every axis, and its seats are worthless

`analysis/fleet.csv` is eighty runs: fleet size 4, 8, 16 and 32 against vehicle capacity 2 and 4,
ten seeds each, door to door with no lines at all.

| vehicles | service rate | km driven | empty share | occupancy | time to pickup | km per passenger |
|---|---|---|---|---|---|---|
| 4 | 0.522 | 193.9 | 0.597 | 0.40 | 881 s | 11.65 |
| 8 | 0.744 | 350.4 | 0.589 | 0.41 | 810 s | 14.74 |
| 16 | 0.894 | 423.6 | 0.544 | 0.46 | 631 s | 14.84 |
| 32 | 0.897 | 415.2 | 0.542 | 0.46 | 607 s | 14.47 |

`baseline.json`'s thirteen carpool drivers serve 0.853 of the same thirty-two riders over **183.1 km
at an occupancy of 1.35 and 6.80 km per passenger**. The fleet needs sixteen vehicles to reach a
comparable service rate and drives **2.2 times as far per person carried** to do it, at a mean wait
for collection of over ten minutes.

The mechanism is one structural difference. An `AutonomousTaxi` has **no trip of its own**: it exists
only to serve, so every kilometre it drives empty is a cost the service pays rather than a trip
somebody was making anyway. A carpool driver's empty kilometres are somebody's commute.

**Capacity 2 and capacity 4 produce identical rows. Every column, every seed, every fleet size.**
That is the pooling result stated as strongly as it can be: the second seat is never used, so the
seats-beat-vehicles finding that holds for carpooling by a factor of five is worth *exactly nothing*
to a fleet. Riders are scattered over a 1.5 km jitter radius and the engine pools only riders
standing within fifty metres of each other, so on this corridor two riders are never at the same
place at the same time. Occupancy never rises above 0.46 at any fleet size or any capacity.

Adding vehicles saturates rather than fixes it - 0.894 at sixteen and 0.897 at thirty-two, with
utilisation falling from 0.944 to 0.606 - because the riders it never reaches are the ones on the
23 km Laura House leg, and they give up before anything gets there.

**One caveat, and it cuts in the direction that strengthens the conclusion.** Every fleet leg is
door to door, so none of them hits the station-pair route cache and all of them are straight lines:
the fleet's distance is **understated** by the corridor's 1.19 detour factor while carpool's loaded
kilometres are real road geometry. The true gap is wider than the one shown.

## What the model has to beat

The machinery for a model in the loop is built, tested and measured. **The result is not, and no
model figure is committed or quoted anywhere in this repository.**

What is measured is the call economy, against `tests/fixtures/stub-sidecar.py`, which answers from a
rule. On `baseline.json` at thirteen drivers, `llm` answers **3,913 decisions in 44 calls** and
`hybrid` **2,239 in 22**. On `minimal.json`, `hybrid` makes zero calls and starts no process. Those
numbers are a fact about the memo and the event-driven triggers - they say nothing about a model, and
they are not a result.

Producing the result is one command with an API key: `sweeps/model-policies.json` is 4 policies × 3
driver counts × 5 seeds, of which 30 runs reach the model, budgeted at ~1,300 calls and ~$11 on
`claude-opus-5` at `--effort low`. Five seeds rather than ten because the model is not deterministic
and each seed is money.

The bars, stated before the run rather than after:

- **The line choice** competes against **0.906 at thirteen vehicles and 0.950 at thirty-two**, which
  is `pairs` and not `heuristic` - the bar moved when pair scoring landed, and reporting against
  `heuristic`'s 0.853 would be reporting against the weaker opponent at exactly the driver counts
  `sweeps/model-policies.json` sweeps. It also competes against a *cost* baseline it must not blow:
  `greedy` and `pairs` both showed that buying service with distance is easy and worthless. Report
  `km per passenger served` beside `service_rate` - 6.80 at thirteen vehicles, which is
  `heuristic`'s - or the comparison is meaningless.
- **The network design** competes against **0.797 to be worth having and 0.853 to match a person**.
  The first is `flow-directed`, the construction that reads the demand's direction; the second is
  `baseline.json`'s six hand-declared one-way lines. Reporting against flow-ranked's 0.750 would be
  reporting against the weakest opponent on the axis, and the bar moved once already when
  `flow-directed` landed. The question is whether the model finds the pruning as well as the
  asymmetry; it is told nothing about what the five constructions produced or what any of them
  scored.
- **The guarantee decision** competes against "always yes", which is what `dispatch_guarantees` did
  before it was a decision.
- **Repositioning an idle taxi** competes against **doing nothing**, because there has never been a
  repositioning rule. This is the easiest win available and therefore the one to report most
  carefully: beating no-repositioning is a low bar and the write-up has to say so before a reader
  works it out. It also lands in `taxi_km_empty_to_pickup` rather than a column of its own, so it
  cannot look cheap on one column while spending on another; the column it has to earn its distance
  back on is `mean_time_to_pickup_s`.

And the constraints on believing whatever comes back. Every decision context is audited for leakage
against a committed list (`policy::LEAKAGE_AUDIT`, asserted field by field by `tests/llm.rs`):
nothing a context carries is unknowable to the deciding party at decision time - no future spawn, no
undeclared request, no realised demand, no metric value, no objective function. A policy that could
see any of those would beat every real dispatcher and the comparison would be worth nothing. An
invalid action is counted and refused, never retried, and `invalid_action_rate` is per *decision*
rather than per call, so a high rate means much of the run was the fallback wearing the model's name.
A dead sidecar fails the run rather than finishing under a fixed rule.

**If the heuristic wins, that is the finding.** A negative result with cost figures attached is
worth more than a demo without them.

## Re-running the two earlier studies: the carpool study survives, the fleet study does not

Both earlier studies - the carpool study on advance declaration and the fleet study on structured networks, both cited in the README - were re-run on their own ground, the Rhône and the lane
from Bourgoin-Jallieu into Lyon, rebuilt from open data (`data/lyon/`, whose README says how close
the rebuilt demand is to the published one - close, not equal). Every scenario was run three ways,
ten seeds each: by **the original simulator** itself, headless and offline
(`analysis/reference/`); by this engine **as the original ran** - its tick, its walking speed, its
bugs as knobs; and by this engine **corrected**. Two rules were fixed before any of it was looked
at: this engine's as-ran mean inside the original's seed range, and every between-scenario
difference pointing the same way in both (`analysis/agreement.py`, `analysis/agreement.csv`).

**The fleet study's result does not survive its own bugs.** Its claim was that a fleet run over a
structured network of seventeen stations beats the same fleet door to door: fewer kilometres, fuller
vehicles, collective transport rather than taxis. At the 100 sample, ten seeds each:

| | Service | Occupancy | Vehicle-km | Mean wait |
|---|---|---|---|---|
| Door to door, original simulator | 0.906 | 0.878 | 45,839 | 252 s |
| Network, original simulator | **0.442** | 1.318 | 39,227 | 1,389 s |
| Door to door, corrected | 0.999 | 0.843 | 48,471 | 235 s |
| Network, corrected | 0.954 | **0.771** | **86,267** | 967 s |

The original's network does drive fewer kilometres at higher occupancy - and serves fewer than half
its riders. Three defects in the original make that shape, each measured in its own runs:

- **Its fleet operator loses assignments.** A taxi handed a rider is still idle for the rest of the
  same decision, so it can be handed another, and it collects only the last; every rider handed to
  it before is told a taxi is coming, offered to nobody else, and gives up. 2,872 assignments lost
  per run on the network at 100, 278 door to door. Riders who give up are kilometres not driven.
- **Its taxi boards riders it never moves out of waiting**, in a fallback branch: 560 per run on the
  network at 100, none door to door. They ride along uncounted as riders and counted as occupancy.
- **Its metrics notebook counts every rider who gave up as served**, by matching a state name the
  simulator never logs. The published satisfaction of every fleet scenario is therefore 1.000 by
  construction; the service actually delivered was 0.906 door to door and 0.442 over the network.

Corrected, **the network loses on every axis**: 0.954 of service against 0.999, occupancy 0.771
against 0.843, and 78% more vehicle-kilometres, because a rider changing taxi at every station is a
taxi driving out to every station. The 10 sample says the same at eight times the demand: 0.995
against 1.000, occupancy 0.941 against 0.996, and 541,558 vehicle-kilometres against 298,536. That
is this repository's own fleet finding on the Welsh corridor, arrived at from the other direction:
**structuring the network does not rescue an on-demand fleet**, and what looked like it did was the
operator dropping riders.

**How well this engine reproduces the original.** Door to door at 100, as ran, its service is
0.905 against the original's 0.890-0.927 - inside the range. Over the network it is 0.305 against
0.416-0.464 and occupancy 1.218 against 1.302-1.354: close, below, and outside, because the
misboarding above is counted in the original and deliberately not reproduced here. Kilometres run
about 7% under the original throughout, from one known difference: the original scatters trip ends
over a square and this engine over a disc of the same radius. The sign of every network-against-
door-to-door difference agrees, all seven metrics. `analysis/agreement.csv` lists every row. The original was not run at the 10 sample:
one seed of the door-to-door scenario had not finished after 84 minutes, its assignment being a
pure-Python loop over nine thousand taxis, so the 10-sample figures above are this engine's alone.

**The carpool study's anticipation result survives, and so does its caveat - for a reason it did
not know.** Over the 1,872 cells of the advance-declaration grid re-run here (13 settings, drivers and
riders in {2, 6, 10, 20}, declaring shares of 0, 50 and 100% on each side, `analysis/reference/
lyon-lane-grid.csv` against `analysis/lyon-lane-as-ran.csv`), this engine's mean falls inside the
original's seed range in 1,709 cells for service and 1,780 for waiting time. Where the original's
own difference between full declaration and none is larger than its seed-to-seed noise, the waiting
time moves the same way in **43 of 46** cells; service agrees in only 25 of 62, the disagreements
sitting in the wide-margin settings, where the two engines time a driver to its rider differently.
Averaged over the grid, as the original ran it, full declaration on both sides leaves the wait where
it was - 506 s against 508 s in the original, 609 s against 617 s here - and declaring drivers alone
triple it, 1,624 s and 1,718 s. That is the carpool study's finding: a path only under full adoption, and
no clear separation even there.

**The reason is that half of it was switched off.** The original's declaring rider was meant to time
its departure to the driver coming for it, and never did: its window bounds carry a sign error, no
driver ever fits, and the rider always leaves at its declared time. Only the driver side of
anticipated declaration ever ran. Corrected - both sides timing to each other, a 1 s tick, walking at
1.4 m/s - full declaration cuts the average wait from 507 s to **382 s**, a quarter, and declaring
drivers alone no longer triple it (666 s). The carpool study measured anticipated declaration with its rider
half missing, and the conclusion that it offers "a path only under forced full adoption" is a
statement about that half-built mechanism rather than about anticipation. Every grid scenario also
configured a departure guarantee that the original simulator created and never put in the world,
so no published run had one.

**One day, single seeds.** `analysis/images/lyon-100-occupancy.svg`, `lyon-100-distance.svg` and
`lyon-100-times.svg` draw what the original's notebooks drew - occupancy per quarter hour weighted by
distance, cumulative loaded and empty kilometres, and the spread of journey and waiting times - for
the three services at 100 as ran, seed 1: the shape of one day, not a finding about many.

## What would change these numbers

In rough order of how much:

1. **Whatever closes the other half of the direction gap.** `flow-directed` recovers 46% of what six
   hand-drawn one-way lines achieve at thirteen vehicles, for free, and the seven surplus lines it
   keeps are why it does not recover the rest. A construction that reads direction *and* prunes to
   the lines a radial service actually wants is the open question, and it is the one
   `scripts/design-network.py` puts to the model.
2. **Pair scoring across the fleet.** `pairs` scores `(driver, rider)` pairs and demotes the line to
   geometry, and it moves the service rate where vehicles are scarce - but it is greedy per driver
   and blind to what the rest of the fleet is doing. The collisions that causes are counted and cost
   seconds, not rides, so what is left is the *ordering*: a contested rider goes to the driver that
   sits earlier in the arena rather than to the cheapest pair. Assigning across the whole fleet at
   once is the same computation as the fleet's `greedy_pairs` over a different pool.
3. **Separate random streams per cohort.** All draws are taken up front in scenario order, so
   changing a driver count changes what the riders draw and a supply axis compares different demand
   at the same seed. Every win/tie/loss count on a driver axis in this document pairs runs rather
   than demand because of it. Drawing each cohort from its own stream would make a supply sweep a
   true paired comparison, and would change nothing about any single run.
4. **Per-agent walking and driving speeds.** They are drawn once per run because under `heuristic`
   both sides must price a line's flanking legs identically to agree on a station. That agreement is
   an artefact of the rule, not of the world, and removing it changes `heuristic`'s numbers rather
   than fixing them.
5. **A behavioural response to the incentive.** Nothing in the engine responds to a payout, so
   `incentive_paid` is arithmetic. The one cost figure that exists is `baseline.json` at seed 42
   under 0.10 per passenger-kilometre: **22.46 over 224.62 passenger-kilometres, 0.90 per person
   carried** - and the rate is invented, so read the ratio and not the currency.
6. **Caching the approach legs.** Walks to stations and every fleet leg take a straight-line
   fallback, which gets the time right and understates distance by the detour factor. It cuts against
   the fleet and in favour of nothing else, so it strengthens rather than threatens the conclusions
   above.

## Two things this document tries not to do

**An effect that fails to separate is not an effect that has been ruled out.** Greedy's low-end
service advantage, free-form against flow-ranked, and 66 vehicles against 132 all come back as "the
seeds disagree". That means the sweep as run cannot tell them apart at ten seeds. It does not mean
they are equal, and none of them is reported as equal above.

**A metric from a run that hit the clock cap is not a finding.** The engine reports `stranded` and
`stopped_by_clock` separately from everything else precisely so a cut-off run cannot be quietly
averaged into a result. No row of any committed sweep has either set, and no committed scenario
does.
