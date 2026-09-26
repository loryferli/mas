#!/usr/bin/env python3
"""A sidecar that reaches no model, so the suite can exercise the seam offline.

The engine's side of the pipe cannot tell this from `scripts/sidecar.py`: same protocol, same
`usage` block, same one line in and one line out. That is the whole reason the model is reached
through a pipe rather than through a client inside the engine - swapping it out costs no Rust.

    python3 tests/fixtures/stub-sidecar.py <mode> [request-log]

Modes:

    second-line   Every answer is valid, and the line answer is deliberately *not* the one either
                  fixed rule would give: the second-ranked option rather than the first. A run
                  under it that matched `heuristic` would mean the decision never reached the
                  world.
    decline       As `second-line`, except that the operator's guarantee vehicle is held back.
                  The one decision whose fixed answer was always yes, so declining is the only way
                  to see that a policy can actually withhold it.
    invalid       Every answer names something that does not exist. What the engine does with it
                  is the point: count it, refuse it, fall back, and never ask again.
    die           Reads one request and exits without answering, which is a sidecar that has
                  died mid-run.

`request-log`, if given, gets one line per request received - which is how a test asserts on what
the engine actually sent rather than on what it meant to send.
"""

import json
import sys

# Deliberately not zero: a test asserts the cost columns carry through, and all-zero usage would
# pass a broken pipeline just as well as a working one.
USAGE = {"input_tokens": 400, "output_tokens": 30, "usd": 0.0025}


def valid(decision: str, context: dict, decline: bool = False) -> dict:
    if decision == "guarantee":
        return {"dispatch": not decline}
    if decision == "line":
        # The second option, or the only one there is.
        option = 1 if len(context["options"]) > 1 else 0
        return {"action": "take_line", "option": option}
    if decision == "assign":
        pairs = min(len(context["taxis"]), len(context["riders"]))
        return {"pairs": [{"taxi": i, "rider": i} for i in range(pairs)]}
    if decision == "reposition":
        # Everybody to the busiest boarding point so far, which is a rule rather than a judgement
        # and is exactly why the real answer is not written here.
        pickups = [station["pickups_so_far"] for station in context["stations"]]
        busiest = pickups.index(max(pickups)) if pickups else 0
        return {
            "moves": [
                {"taxi": i, "station": busiest} for i in range(len(context["taxis"]))
            ]
        }
    raise ValueError(f"no such decision: {decision}")


def broken(decision: str) -> dict:
    if decision == "line":
        return {"action": "take_line", "option": 999}
    if decision == "guarantee":
        return {"dispatch": "yes please"}
    if decision == "assign":
        return {"pairs": [{"taxi": 999, "rider": 999}]}
    if decision == "reposition":
        return {"moves": [{"taxi": 999, "station": 999}]}
    raise ValueError(f"no such decision: {decision}")


def main() -> None:
    mode = sys.argv[1]
    log = open(sys.argv[2], "a", encoding="utf-8") if len(sys.argv) > 2 else None
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        if log is not None:
            log.write(line + "\n")
            log.flush()
        if mode == "die":
            return
        request = json.loads(line)
        decision = request["decision"]
        if mode in ("second-line", "decline"):
            action = valid(decision, request["context"], decline=mode == "decline")
        else:
            action = broken(decision)
        print(json.dumps({"action": action, "usage": USAGE}), flush=True)


if __name__ == "__main__":
    main()
