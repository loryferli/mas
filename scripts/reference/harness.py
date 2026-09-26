"""Run the reference engine headless and offline, one (scenario, seed) per process, and reduce it
to a row.

The reference engine is the Python simulator the earlier published runs were made with (see the
top-level README), in its two versions: the *lane engine*, which ran the advance-declaration grid,
and the *fleet engine*, which ran the private-car and fleet scenarios. It is not part of this
repository; `run.py` is pointed at a checkout of each.

`run.py` is the command; this module is what one run does. Every change it makes to the engine is
a patch applied at import time, never an edit to its files, and every patch is listed in `PATCHES`
with the reason it is needed. A patch either removes something that cannot work offline
(the routing server, the log files, the console), makes code that cannot run at all run, or makes
it fast without changing what it computes - and the last kind is checked, not trusted: `run.py
--naive-perception` runs the original perception and the two event logs must be identical.

The metrics are this repository's columns (`src/metrics.rs`), with the engine's own notebooks' reading
wherever the two models differ in what they have: a party counts in full, a private driver's own
companion is its party rather than a rider, occupancy is passenger-kilometres over vehicle-
kilometres. Four trailing columns carry what only the notebooks report - see `EXTRA_COLUMNS`.
"""

import contextlib
import io
import math
import random
import sys
import types
from collections import defaultdict
from pathlib import Path

# Where each engine is checked out: `run.py --lane-engine/--fleet-engine`, or these variables.
ENGINE_VARIABLES = {"lane": "MAS_LANE_ENGINE", "fleet": "MAS_FLEET_ENGINE"}

# The engine's own tick. This repository's ticks 1 s; `import --variant as-ran` writes 30 s.
TICK_S = 30

# Every fleet taxi idles for ever and a lane-engine carpool rider has no patience at all, so neither
# engine ends on its own. Two days covers the evening peak with room to spare.
DEFAULT_CLOCK_CAP_S = 2 * 86400

# The Rust engine's default, which a scenario overrides with `road_detour_factor`. Walking is not
# stretched, as in `src/routing.rs`.
DEFAULT_ROAD_DETOUR_FACTOR = 1.3
FOOT_PROFILES = {"foot"}

PATCHES = [
    "routing: `get_route` is a straight line whose duration is stretched by the road detour factor, "
    "the Rust fallback's rule; the GraphHopper server it called no longer answers",
    "movement: a body on a road route moves at its speed over the detour factor, so the route's "
    "duration is the time it takes - as the Rust fallback's journey does",
    "angles: a zero-length vector makes an angle of 0 and the cosine is clamped to [-1, 1]; the "
    "harness router ends a route exactly on a station, which GraphHopper's road-snapped endpoints "
    "never did, and a driver standing on its line's origin divided by zero",
    "perception: a spatial index over bodies instead of every agent against every other, in list "
    "order, checked against the original by --naive-perception",
    "console and log files: stdout is discarded and the four loggers write nowhere; the harness "
    "records state transitions itself",
    "imports that are never reached offline are stubbed: geopy and requests (the live router), "
    "shapely (imported, unused), time_aware_polyline (lane engine, imported, unused), and polyline, "
    "which only decodes a line's display geometry and raised on lane.json's placeholder string",
    "lane engine: the three fleet modules its simulation.py imports did not exist at that commit, and are "
    "empty placeholders",
    "lane engine: `CarpoolOrchestrator`, the class the scenarios name, is the lane engine's `Orchestrator`",
    "lane engine: `Driver.offer` defaults to None; the lane engine never sets it, so the first "
    "`update_state` of any driver raised AttributeError",
    "lane engine: a body starts with a four-seat `Vehicle` so the one-in-three companion drawn in "
    "`Driver.__init__` has somewhere to sit (it raised AttributeError); the companion, a random "
    "integer in `passengers` that every drop-off loop then read `.demand` from, becomes one seat "
    "off the vehicle's capacity, and survives the cohort's own `vehicle` replacing the default",
]

EXTRA_COLUMNS = [
    # A private-car baseline counts every private driver as a user, and its car as occupied by its
    # party.
    "private_drivers",
    "private_drivers_arrived",
    "private_party_km",
    # The engine's metrics notebook excludes cancelled riders by matching the state "CANCEL", which
    # the engine never logs ("CANCELED"), so the published satisfaction rate counted a rider who gave
    # up and was sent home as served. This is that count, beside the correct one in
    # passengers_served.
    "passengers_served_as_published",
    # The engine's taxi, in its fallback branch, boards a rider for another destination without
    # moving it out of WAITING_DRIVER. How often it fired.
    "fleet_misboardings",
    # The fleet operator can send an idle taxi a second rider in the same pass; the taxi serves
    # only the last one it was sent. How many assignments were dropped that way.
    "fleet_lost_assignments",
]


class Route(list):
    """A harness route: the engine's `[[latitude, longitude, seconds], ...]`, carrying its pace."""

    def __init__(self, points, detour_factor):
        super().__init__(points)
        self.detour_factor = detour_factor


class Tracked(list):
    """`Environment.agents`, remembering each member's arrival order, for the perception index.

    Relative order is all perception needs, and removal never changes it, so a counter stamped at
    append is the list position as far as any sort is concerned.
    """

    def __init__(self, items, on_append):
        super().__init__()
        self.order = {}
        self.counter = 0
        self.on_append = on_append
        for item in items:
            self.append(item)

    def append(self, item):
        super().append(item)
        self.counter += 1
        self.order[id(item)] = self.counter
        self.on_append(item)

    def remove(self, item):
        super().remove(item)
        del self.order[id(item)]


class Grid:
    """Bodies by cell. A cell is wider than the 100 m perception radius in both directions at any
    latitude these scenarios use, so the 3 x 3 block around a body holds everything it can see."""

    CELL_DEG = 0.002

    def __init__(self):
        self.cells = defaultdict(dict)
        self.where = {}

    def key(self, coordinates):
        return (
            math.floor(coordinates["latitude"] / self.CELL_DEG),
            math.floor(coordinates["longitude"] / self.CELL_DEG),
        )

    def move(self, body, coordinates):
        old = self.where.pop(id(body), None)
        if old is not None:
            self.cells[old].pop(id(body), None)
        if isinstance(coordinates, dict) and "latitude" in coordinates:
            key = self.key(coordinates)
            self.cells[key][id(body)] = body
            self.where[id(body)] = key

    def near(self, coordinates):
        row, column = self.key(coordinates)
        for d_row in (-1, 0, 1):
            for d_column in (-1, 0, 1):
                yield from self.cells.get((row + d_row, column + d_column), {}).values()


class Ledger:
    """What happened, recorded as it happens: transitions per agent, and kilometres per vehicle
    split by who was aboard."""

    def __init__(self):
        self.spawned = []
        self.in_env = set()
        self.transitions = defaultdict(list)
        self.km = defaultdict(float)
        self.loaded_km = defaultdict(float)
        self.passenger_km = 0.0
        self.private_party_km = 0.0
        self.carried_by = {}
        self.misboarded = set()
        self.lost_assignments = 0

    def spawn(self, agent):
        if hasattr(agent, "body") and id(agent) not in self.in_env:
            self.in_env.add(id(agent))
            self.spawned.append(agent)


def install(tree, engine, detour_factor, naive_perception):
    """Import `tree` with every patch in `PATCHES` applied. Returns what `run` needs."""
    for name in ("geopy", "geopy.distance", "requests", "shapely", "shapely.geometry"):
        sys.modules.setdefault(name, types.ModuleType(name))
    sys.modules["geopy.distance"].geodesic = None
    sys.modules["shapely.geometry"].Point = sys.modules["shapely.geometry"].LineString = None
    stub = types.ModuleType("time_aware_polyline")
    stub.decode_time_aware_polyline = None
    sys.modules["time_aware_polyline"] = stub
    stub = types.ModuleType("polyline")
    stub.decode = lambda encoded: []
    sys.modules["polyline"] = stub

    if tree == "lane":
        # The lane engine's simulation.py imports the fleet modules, which did not exist at that commit.
        for name in ("autonomous_taxi_orchestrator", "autonomous_taxi_rider", "autonomous_taxi"):
            module = types.ModuleType(f"sma.agent.{name}")
            placeholder = "".join(part.capitalize() for part in name.split("_"))
            setattr(module, placeholder, type(placeholder, (), {}))
            sys.modules[module.__name__] = module

    sys.dont_write_bytecode = True  # the engine's checkout stays exactly as it is
    sys.path.insert(0, str(engine))
    with contextlib.redirect_stdout(io.StringIO()):
        import simulation  # noqa: F401 - imports every agent module, which the patches need
    from sma.agent.agent import Agent
    from sma.body.body import Body
    from sma.environment.environment import Environment
    from helper.agent_type import inspect_gents_dict
    from helper.vector2 import Vector2

    def angle(vector, other):
        lengths = vector.getLength() * other.getLength()
        if lengths == 0:
            return 0.0
        return math.acos(max(-1.0, min(1.0, vector.dotproduct(other) / lengths)))

    Vector2.angle = angle

    if tree == "lane":
        from helper.math import distance
        from sma.agent.driver import Driver
        from sma.agent.orchestrator import Orchestrator
        from sma.environment.vehicle import Vehicle

        Orchestrator.__name__ = "CarpoolOrchestrator"  # inspect_gents_dict keys on __name__
        Driver.offer = None
    else:
        from helper.custom_math import distance
        from sma.agent.orchestrator import Orchestrator

        Vehicle = None

    ledger = Ledger()
    grid = Grid()

    def get_route(origin, destination, path=None, detour=None, profile="car", service="graphhopper", speed=4):
        if path:
            raise RuntimeError("a replayed GPS path reached the router; no committed scenario has one")
        factor = 1.0 if profile in FOOT_PROFILES else detour_factor
        stops = [origin] + ([detour] if detour is not None else []) + [destination]
        points = [[stops[0]["latitude"], stops[0]["longitude"], 0]]
        for a, b in zip(stops, stops[1:]):
            km = distance([a["latitude"], a["longitude"]], [b["latitude"], b["longitude"]])
            points.append([b["latitude"], b["longitude"], km * 1000 * factor / speed])
        return Route(points, factor)

    original_route = sys.modules["helper.routing"].get_route
    for module in list(sys.modules.values()):
        if getattr(module, "get_route", None) is original_route:
            module.get_route = get_route

    # Bodies: coordinates feed the index, a route sets the pace, and movement is measured.
    def get_coordinates(body):
        return body.__dict__.get("_coordinates")

    def set_coordinates(body, value):
        body.__dict__["_coordinates"] = value
        grid.move(body, value)

    def get_go_to(body):
        return body.__dict__.get("_go_to", [])

    def set_go_to(body, value):
        if isinstance(value, Route):
            body.__dict__["_detour_factor"] = value.detour_factor
        body.__dict__["_go_to"] = value

    Body.coordinates = property(get_coordinates, set_coordinates)
    Body.go_to = property(get_go_to, set_go_to)

    if Vehicle is not None:
        # The lane engine's companion is a random integer put in `passengers`, and every loop over
        # passengers reads `.demand` from it. It is held here as the seat it takes instead: off the
        # passenger list, out of the capacity every pickup compares against.
        original_body_init = Body.__init__
        original_driver_init = Driver.__init__

        def body_init(body, parent):
            original_body_init(body, parent)
            body.__dict__["_vehicle"] = Vehicle()

        def driver_init(driver):
            original_driver_init(driver)
            vehicle = driver.body.vehicle
            vehicle.companion_seats = sum(1 for p in vehicle.passengers if isinstance(p, int))
            vehicle.passengers = [p for p in vehicle.passengers if not isinstance(p, int)]

        def get_vehicle(body):
            return body.__dict__.get("_vehicle")

        def set_vehicle(body, value):
            old = body.__dict__.get("_vehicle")
            if old is not None and value is not None:
                value.companion_seats = getattr(old, "companion_seats", 0)
            body.__dict__["_vehicle"] = value

        def get_capacity(vehicle):
            return vehicle.__dict__["_capacity"] - vehicle.__dict__.get("companion_seats", 0)

        def set_capacity(vehicle, value):
            vehicle.__dict__["_capacity"] = value

        Body.__init__ = body_init
        Body.vehicle = property(get_vehicle, set_vehicle)
        Driver.__init__ = driver_init
        Vehicle.capacity = property(get_capacity, set_capacity)

    original_move = Body.update_position_with_speed

    def aboard(vehicle, driver):
        people = getattr(vehicle, "occupants", None)
        if people is None:
            people = vehicle.passengers
        return [p for p in people if p is not driver and id(p) in ledger.in_env]

    def move(body, tic):
        before = body.coordinates
        points = list(body.go_to)
        speed = body.speed
        body.speed = speed / body.__dict__.get("_detour_factor", 1.0)
        try:
            original_move(body, tic)
        finally:
            body.speed = speed
        consumed = len(points) - len(body.go_to)
        path = [[before["latitude"], before["longitude"]]]
        path += [[p[0], p[1]] for p in points[:consumed]]
        path.append([body.coordinates["latitude"], body.coordinates["longitude"]])
        km = sum(distance(a, b) for a, b in zip(path, path[1:]))
        # A lane-engine body always has a vehicle (a patch above), so a walker is told apart by its class.
        if km == 0 or body.vehicle is None or type(body.parent).__name__ in RIDER_CLASSES:
            return
        driver = body.parent
        ledger.km[id(driver)] += km
        riders = aboard(body.vehicle, driver)
        if riders:
            ledger.loaded_km[id(driver)] += km
        for rider in riders:
            ledger.carried_by[id(rider)] = driver
            ledger.passenger_km += km * party(rider)
        if type(driver).__name__ == "Driver":
            people = getattr(body.vehicle, "occupants", None)
            companions = len(people) - 1 if people is not None else len(body.vehicle.passengers)
            ledger.private_party_km += km * (1 + companions)

    Body.update_position_with_speed = move

    # Transitions, recorded at the moment every agent already reports them.
    classes = inspect_gents_dict(Agent)
    for cls in classes.values():
        if "update_state" in cls.__dict__:
            cls.update_state = recording(cls.__dict__["update_state"], ledger)

    if "AutonomousTaxi" in classes:
        taxi = classes["AutonomousTaxi"]
        original_taxi_decision = taxi.do_decision

        def taxi_decision(agent, tic, clock):
            if agent.state == "IDLE" and len(agent.inbox) > 1:
                ledger.lost_assignments += len(agent.inbox) - 1
            result = original_taxi_decision(agent, tic, clock)
            for rider in agent.body.vehicle.occupants:
                if rider.state == "WAITING_DRIVER":
                    ledger.misboarded.add(id(rider))
            return result

        taxi.do_decision = taxi_decision

    if not naive_perception:

        def compute_perception(env, agent):
            if not hasattr(agent, "body"):
                agent.perceptions_agents = []
                agent.perceptions_items = []
                return
            seen = []
            for body in grid.near(agent.body.coordinates):
                other = body.parent
                order = env.agents.order.get(id(other))
                if order is None or other.uuid == agent.uuid or not body.visible:
                    continue
                dist, inside = agent.body.fustrum.inside(body, agent.body)
                if inside:
                    body.distance_to = dist
                    seen.append((order, body))
            seen.sort(key=lambda pair: pair[0])
            orchestrators = [o for o in env.agents if isinstance(o, Orchestrator) and o.uuid != agent.uuid]
            agent.perceptions_agents = [body for _, body in seen] + orchestrators
            # Fustrum.inside returns a tuple, which is always true: every agent perceives every item.
            agent.perceptions_items = env.items[:]

        Environment.compute_perception = compute_perception

    return types.SimpleNamespace(
        simulation=sys.modules["simulation"],
        ledger=ledger,
        classes=classes,
        orchestrator=Orchestrator,
    )


def recording(update_state, ledger):
    def wrapper(agent, state, clock):
        ledger.transitions[id(agent)].append((clock, state))
        return update_state(agent, state, clock)

    return wrapper


def party(rider):
    return 1 + getattr(rider, "companions", 0)


def quiet_loggers():
    import logging

    for name in ("env", "rider", "driver", "orchestrator"):
        log = logging.getLogger(name)
        log.handlers[:] = [logging.NullHandler()]
        log.propagate = False


def run(tree, engine, scenario, seed, detour_factor=DEFAULT_ROAD_DETOUR_FACTOR,
        clock_cap_s=DEFAULT_CLOCK_CAP_S, naive_perception=False, events=None):
    """One run of `tree` ("lane" or "fleet") from the checkout at `engine`. Returns the metrics row as
    a dict, and writes the transitions to `events` if given."""
    world = install(tree, engine, detour_factor, naive_perception)
    quiet_loggers()
    random.seed(seed)
    ledger = world.ledger
    with contextlib.redirect_stdout(io.StringIO()):
        env = world.simulation.load_environment_json(str(scenario))
    env.agents = Tracked(env.agents, ledger.spawn)
    clock_attribute = "tic" if tree == "lane" else "clock"

    def settled():
        if any(env.agents_in_queue.values()):
            return False
        for agent in env.agents:
            if not hasattr(agent, "body") or agent.is_freeze:
                continue
            if type(agent).__name__ == "AutonomousTaxi" and agent.state == "IDLE" and not agent.inbox:
                continue
            return False
        return True

    stopped_by_clock = False
    with contextlib.redirect_stdout(io.StringIO()):
        while not settled():
            if getattr(env, clock_attribute) >= clock_cap_s:
                stopped_by_clock = True
                break
            env.run(TICK_S)
            # Drain the discarded console every tick, or a long run holds it all in memory.
            sys.stdout.seek(0)
            sys.stdout.truncate()

    if events is not None:
        write_events(events, ledger)
    return reduce(ledger, stopped_by_clock)


RIDER_CLASSES = {"CarpoolRider", "GhostRider", "PolynomialRider", "AutonomousTaxiRider"}


def reduce(ledger, stopped_by_clock):
    riders = [a for a in ledger.spawned if type(a).__name__ in RIDER_CLASSES]
    drivers = [a for a in ledger.spawned if type(a).__name__ not in RIDER_CLASSES]
    fleet = [d for d in drivers if type(d).__name__ == "AutonomousTaxi"]
    private = [d for d in drivers if type(d).__name__ == "Driver"]

    def states(agent):
        return [state for _, state in ledger.transitions[id(agent)]]

    def ended(agent):
        return "END_JOURNEY" in states(agent)

    def served(rider):
        return ended(rider) and "CANCELED" not in states(rider) and id(rider) in ledger.carried_by

    def waited_s(rider):
        total, since = 0.0, None
        for clock, state in ledger.transitions[id(rider)]:
            if since is not None:
                total += clock - since
                since = None
            if state == "WAITING_DRIVER":
                since = clock
        return total

    def arrived_s(agent):
        return max(clock for clock, state in ledger.transitions[id(agent)] if state == "END_JOURNEY")

    carried = [r for r in riders if id(r) in ledger.carried_by]
    arrived = [r for r in riders if served(r)]
    by_taxi = [r for r in carried if type(ledger.carried_by[id(r)]).__name__ == "AutonomousTaxi"]

    asking = sum(party(r) for r in riders)
    passengers_served = sum(party(r) for r in arrived)
    km_total = sum(ledger.km[id(d)] for d in drivers)
    km_loaded = sum(ledger.loaded_km[id(d)] for d in drivers)
    active = {id(d) for d in ledger.carried_by.values()}
    stranded = sum(
        1
        for a in ledger.spawned
        if not a.is_freeze
        and not (type(a).__name__ == "AutonomousTaxi" and a.state == "IDLE")
        and not ended(a)
    )

    row = {
        "agents_spawned": len(ledger.spawned),
        "riders_total": len(riders),
        "drivers_total": len(drivers),
        "passengers_served": passengers_served,
        "passengers_unserved": asking - passengers_served,
        "service_rate": ratio(passengers_served, asking),
        "drivers_active": sum(1 for d in drivers if id(d) in active),
        "vehicle_km_total": km_total,
        "vehicle_km_loaded": km_loaded,
        "vehicle_km_empty": km_total - km_loaded,
        "empty_distance_share": ratio(km_total - km_loaded, km_total),
        "passenger_km": ledger.passenger_km,
        "mean_occupancy": ratio(ledger.passenger_km, km_total),
        "mean_wait_s": mean(waited_s(r) for r in carried),
        "mean_journey_time_s": mean(arrived_s(r) - r.spawn_time for r in arrived),
        "taxi_km_empty_to_pickup": 0.0 + sum(ledger.km[id(t)] - ledger.loaded_km[id(t)] for t in fleet),
        "mean_time_to_pickup_s": mean(waited_s(r) for r in by_taxi),
        "fleet_utilisation": ratio(sum(1 for t in fleet if id(t) in active), len(fleet)),
        "incentive_paid": 0.0,
        "cost_per_passenger_served": 0.0,
        "model_decisions": 0,
        "model_calls": 0,
        "usd_per_decision": 0.0,
        "tokens_per_decision": 0.0,
        "decision_latency_ms": 0.0,
        "invalid_action_rate": 0.0,
        "stranded": stranded,
        "stopped_by_clock": str(stopped_by_clock).lower(),
        "private_drivers": len(private),
        "private_drivers_arrived": sum(1 for d in private if ended(d)),
        "private_party_km": ledger.private_party_km,
        "passengers_served_as_published": sum(party(r) for r in riders if ended(r)),
        "fleet_misboardings": len(ledger.misboarded),
        "fleet_lost_assignments": ledger.lost_assignments,
    }
    return row


def write_events(path, ledger):
    """The transitions as the engine's events.csv shape would carry them: time, agent, kind, state.
    Agents are numbered by arrival, never by the engine's random uuid, so two runs compare."""
    number = {id(a): i for i, a in enumerate(ledger.spawned)}
    rows = []
    for agent in ledger.spawned:
        for clock, state in ledger.transitions[id(agent)]:
            rows.append((clock, number[id(agent)], type(agent).__name__, state))
    rows.sort(key=lambda row: (row[0], row[1]))
    with open(path, "w") as handle:
        handle.write("t_s,agent_id,kind,state\n")
        for clock, agent, kind, state in rows:
            handle.write(f"{clock:.1f},{agent},{kind},{state}\n")


def ratio(numerator, denominator):
    return numerator / denominator if denominator > 0 else 0.0


def mean(values):
    values = list(values)
    return sum(values) / len(values) if values else 0.0
