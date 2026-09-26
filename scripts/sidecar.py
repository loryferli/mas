#!/usr/bin/env python3
"""The model in the loop, as a sidecar: one JSON line in on stdin, one JSON line out on stdout.

The engine holds no HTTP client, no async runtime and no TLS stack. It spawns this, writes one
request per decision, and reads one reply. The Anthropic SDK lives here, in the same Python
environment the analysis layer already uses, and the engine never learns that an API exists.

    request   {"decision": "line", "context": {...}, "schema": {...}}
    reply     {"action": {...}, "usage": {"input_tokens": n, "output_tokens": n, "usd": f}}
    failure   {"error": "..."}

Two things this deliberately does not do.

**It does not decide what the model may see.** The context arrives serialised, already audited
against `policy::LEAKAGE_AUDIT` on the Rust side, and is passed through verbatim. Adding a field
here would be adding it behind the audit's back.

**It does not own the action space.** The JSON schema arrives with every request and is handed
straight to `output_config.format`, so the shape the model may answer in is defined once, in
`policy.rs`, beside the code that validates it. A copy here is a copy that drifts.

A failure is reported and never papered over: any exception becomes `{"error": ...}`, and the
engine ends the run rather than falling back to a fixed rule and reporting its numbers under the
model's name.

Run it by hand to see what it does:

    echo '{"decision":"guarantee","context":{"riders_triggered":1},
           "schema":{"type":"object","properties":{"dispatch":{"type":"boolean"}},
                     "required":["dispatch"],"additionalProperties":false}}' \\
      | uv run scripts/sidecar.py
"""

import argparse
import json
import sys

import anthropic

# Input and output price per million tokens, for the models this is worth running against. Used
# only to fill in `usd`: cost is a first-class metric in the write-up, so the number has to come
# from somewhere, and a table here is honest about being a table rather than a meter reading.
PRICES_PER_MTOK = {
    "claude-opus-5": (5.00, 25.00),
    "claude-sonnet-5": (2.00, 10.00),
    "claude-haiku-4-5": (1.00, 5.00),
}

# What the service is trying to do, and what it is trying not to spend. Stating the objective is
# not leakage - an operator knows what it is optimising; leakage would be showing the model a
# realised metric, a future arrival or a trip nobody has declared, and the context it is handed
# carries none of those by construction.
OPERATOR = """\
You are the dispatcher for a shared-mobility service on a rural corridor in mid-Wales: seven
boarding points over 23 kilometres, a few dozen travellers a morning, and a handful of vehicles.
Demand is thin and scattered, which is the whole difficulty - there is rarely a second passenger
standing where the first one is.

You are optimising two things against each other:

- carrying as many of the people who asked as possible, and not making them wait;
- driving as few empty kilometres as possible, because on this corridor an empty kilometre is
  most of what the service costs.

You are given only what the operator can actually know at the moment of the decision. There is no
forecast, no list of trips nobody has announced yet, and no score. Decide from what is in front of
you, and prefer the cheap answer when the expensive one is not clearly better: a vehicle sent
somewhere it is not needed is a cost with no service against it.

Answer with the JSON object the response schema describes and nothing else."""

# One task line per decision. Each says what the numbers mean and what the answer decides; none
# says what to prefer, because a rule written here would be a fifth fixed heuristic wearing the
# model's name.
TASKS = {
    "line": """\
A driver has signed up and is choosing which line to serve on the trip it was already making.

`options` is every line the operator publishes, quickest first. `detour_pct` is what serving that
line turns the driver's own trip into, as a percentage of driving straight to its destination - 100
costs the driver nothing, 160 is a trip 60% longer. `riders_waiting` is how many travellers the
operator can offer on that line right now: standing at its first stop, or having announced a trip
on it ahead of departing. `max_detour_pct` is the most this particular driver will accept, and null
means it will accept any.

Answer `{"action": "take_line", "option": i}` with `i` the index in `options`, or
`{"action": "drive_own_trip", "option": 0}` if no line is worth serving - that driver then drives
its own trip and carries nobody. An option outside the list, or one beyond `max_detour_pct`, is
refused and counted against you.""",
    "guarantee": """\
The service promises that a traveller who has waited too long still gets a vehicle. One has now
waited past the trigger on this line, and you decide whether to send the operator's own vehicle.

That vehicle has no trip of its own, so every kilometre it covers is a cost the service pays and
nobody else was going to make - `depot_to_origin_km` is what it covers empty before it collects
anybody. `drivers_inbound` is how many ordinary drivers are already registered on this line and on
their way to its first stop: they may reach the traveller first, and one of them costs the service
nothing. `patience_spent` runs from 0 to 1 and is how much of the traveller's own patience has
gone; at 1 it is about to give up whatever anybody does. Declining does not end the matter - the
question comes back after `cooldown_s` if somebody is still waiting.

Answer `{"dispatch": true}` to send one now, or `{"dispatch": false}` to hold it back.""",
    "assign": """\
The on-demand fleet: vehicles with no trips of their own, serving door to door. Pair the idle
vehicles with the travellers waiting to be collected.

`taxis` and `riders` are two lists; a pair names one index in each. Positions are latitude and
longitude in degrees. `patience_spent` runs from 0 to 1: a traveller near 1 will give up soon, so
reaching it is worth more driving than reaching one that has just hailed. `seats` is the size of a
traveller's party and must fit `free_seats`.

Answer `{"pairs": [{"taxi": i, "rider": j}, ...]}`. You need not pair everybody. A vehicle or a
traveller named twice, an index outside its list, or a party larger than the vehicle makes the
whole answer invalid: it is refused, counted against you, and the greedy rule decides instead.""",
    "reposition": """\
The on-demand fleet again, with nothing to do. Vehicles wait wherever they last set somebody down.
You may send an idle one somewhere else to wait for the demand you expect next.

`stations` are the corridor's boarding points; `pickups_so_far` is how many travellers this run has
already collected nearest each of them, which is the only history you are given. Positions are
latitude and longitude in degrees.

Every kilometre driven to reposition is empty and is charged to the service, and moving nothing is
a perfectly good answer - the rule you are being compared against is standing still.

Answer `{"moves": [{"taxi": i, "station": k}, ...]}`, with indices into `taxis` and `stations`.
Omit a vehicle to leave it where it is; `{"moves": []}` leaves the whole fleet alone.""",
}


def price(model: str, usage) -> float:
    """What one call cost, from the table. Unknown model: zero rather than a guess, so a wrong
    figure never reaches a write-up dressed as a measurement."""
    rates = PRICES_PER_MTOK.get(model)
    if rates is None:
        return 0.0
    inputs = usage.input_tokens + getattr(usage, "cache_read_input_tokens", 0) or 0
    return (inputs * rates[0] + usage.output_tokens * rates[1]) / 1_000_000


def answer(client: anthropic.Anthropic, model: str, effort: str, request: dict) -> dict:
    decision = request["decision"]
    task = TASKS.get(decision)
    if task is None:
        raise ValueError(f"no such decision: {decision}")

    response = client.messages.create(
        model=model,
        max_tokens=16000,
        system=f"{OPERATOR}\n\n{task}",
        output_config={
            "effort": effort,
            "format": {"type": "json_schema", "schema": request["schema"]},
        },
        messages=[{"role": "user", "content": json.dumps(request["context"])}],
    )
    if response.stop_reason == "refusal":
        raise RuntimeError(f"the model declined the {decision} decision")
    text = next(block.text for block in response.content if block.type == "text")
    return {
        "action": json.loads(text),
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "usd": price(model, response.usage),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--model",
        default="claude-opus-5",
        help="the model to ask. Anything not in PRICES_PER_MTOK reports a cost of zero.",
    )
    parser.add_argument(
        "--effort",
        default="low",
        choices=["low", "medium", "high", "xhigh", "max"],
        help=(
            "how hard the model thinks per decision. This is the cost-quality knob for the whole "
            "sweep and belongs in the budget you compute before running one: tens of decisions a "
            "run, times the runs, times the price. Low is the default because these are small "
            "dispatch decisions, not because it is the best answer - it is a point on an axis "
            "worth sweeping."
        ),
    )
    args = parser.parse_args()

    client = anthropic.Anthropic()
    # Line by line, and flushed every time: the engine is blocked on this reply, so a buffered
    # answer is a hung run.
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            reply = answer(client, args.model, args.effort, json.loads(line))
        except Exception as failure:  # noqa: BLE001 - every failure ends the run, loudly.
            reply = {"error": f"{type(failure).__name__}: {failure}"}
        print(json.dumps(reply), flush=True)


if __name__ == "__main__":
    main()
