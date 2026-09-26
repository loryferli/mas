# mas

A multi-agent simulator for on-demand shared mobility on low-flow networks, in Rust. The question it
exists to answer: **can a ridesharing service survive on a network carrying very little traffic, and
what does it cost to run one?**

## Where it comes from

Two pieces of earlier work, and this repository is the attempt to answer what neither of them could.

**MSc thesis, Politecnico di Torino, 2025: *Ridesharing: an analysis of performance in low-flow
networks*.** Three techniques aimed at one question, and the honest answer was no, or at least not
provably: none of the behavioural levers moved participation enough to turn an unstable network into
a working one. Anticipated departure declaration showed a path only under forced full adoption, and
even there the simulation gave no clear statistical separation between scenarios. What did come out
were cost figures, which are enough to make a strategic decision with and not at all the same thing
as a solution.

**PFIA 2025, Dijon, pp. 86–94: *Véhicules autonomes : simulation multi-agents pour explorer
l'importance de la structuration de réseaux*.** A multi-agent simulation of autonomous on-demand
mobility, asking whether the network a service runs on should be structured or left free-form.
Structured won, and not marginally: better route optimisation, fewer kilometres driven with
passengers aboard, and a service that starts to behave like collective transport rather than a fleet
of taxis. The same structuring looked applicable to carpooling in peri-urban areas: an on-demand
service built around a few trunk lines, which we called *Mobility as a Network*.

## Running it

```bash
cargo test
cargo run -- run --scenario scenarios/minimal.json --seed 42 --out /tmp/a
```
