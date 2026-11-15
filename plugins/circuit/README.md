# circuit

Draws, solves and grades small linear circuits: DC and single-frequency AC, by modified nodal analysis. The request format is `schemas/circuit.json`.

## What it does

| | |
|---|---|
| Parts | `R`, `C`, `L`; independent `V` and `I` sources; dependent sources `VCVS`, `VCCS`, `CCVS`, `CCCS`; ideal `opamp`; `wire`, `switch`, `ground`; `node` and `terminal` labels; `probe` |
| Probes | a voltage at a point or between two points; the voltage, current or power of a part. `hidden: true` makes it the learner's unknown |
| Figure | node letters, solved node voltages and branch currents (`show`); parallel branches spread side by side; subscripts in every label |
| Annotations | `current` and `voltage` reference marks, a current `path`, mesh `loop`s, a `cross` over a part or region, a `highlight` box, a `note` |
| Tool text | probes, node voltages, branch currents, powers, series and parallel groups with their combined resistance, the time constant of a one-capacitor or one-inductor circuit, warnings |
| On request | `nodal`: the KCL equations, with supernodes and source constraints; `superposition`: each source's share of every probe; `equivalent`: Thevenin and Norton at a load or two points |
| View | sliders for `adjustable` values and gains, `toggle` switches, checkboxes for node names, voltages and currents, tables of probes and of each part's current and power, answer submission |

## Conventions

- Points are `[x, y]` on an integer grid, x to the right and **y upwards**. Pins that share a point are joined; a pin that lies on a wire is joined to it (a T-junction). A pin on the body of any other part is not, and warns.
- A two-terminal part's voltage is V(`from`) − V(`to`) and its current flows from `from` to `to` through it (passive sign convention). `V` raises `to` above `from`, so a source delivering power has a positive current and a negative (absorbed) power. `I` pushes its current into `to`.
- A dependent source's output is `gain` × its control: the voltage or current, in the convention above, of the part `control.part`, or the voltage between `control.plus` and `control.minus`. A wire may be the controlling part: it is then solved as a 0 V source so its current exists.
- AC values are phasors, peak unless `analysis.ac.rms`; powers are P (and Q in the text).
- The op-amp is ideal: V₊ = V₋, no input current.
- Labels take subscripts: `R_1`, `V_out`, `I_{sc 2}`. A subscript is the letters and digits after `_`, or anything in braces.
- While any probe is hidden, the figure, the artifact data and the view carry no node voltage, current or power, since any of them could give the hidden value away. The tool text, which only the model reads, has everything.

## Layout

`src/main.luau` only wires the exports `plugin.toml` names (`circuit_draw`, `circuit_solve`, `render`, `solve`, `grade`). The rest of `src/`, one responsibility per file:

| File | Responsibility |
|---|---|
| `model.luau` | Types (parts, annotations, circuits, solutions), request defaults, grid points, part helpers, the learner-safe copy of a solution |
| `phasor.luau` | DC or AC number domain: source values, admittances, the power scale |
| `netlist.luau` | Connectivity: which points form a node, T-junctions, node numbers and names |
| `solver.luau` | Modified nodal analysis: validation, stamping, connectivity warnings, the solve, probes, currents and powers |
| `analysis.luau` | Series and parallel groups, Thevenin/Norton, superposition, time constant |
| `equations.luau` | The node-voltage equations as text |
| `report.luau` | The model-facing text |
| `format.luau` | Engineering notation and phasor text |
| `canvas.luau` | The SVG being drawn: bounds, rounding, text with subscripts, which boxes are taken, and placing a label where none is |
| `symbols.luau` | One symbol per part, and its caption |
| `marks.luau` | Arrows, polarities, paths, loops, crosses, highlights, labels, and the places each may take |
| `schematic.luau` | Grid-to-page layout and the drawing order |
| `grade.luau` | Answer-mode grading by re-solving the submitted circuit |

## Symbol style

ANSI/IEEE symbols: a resistor is a zigzag, an independent source a circle, a dependent source a diamond; voltage sources carry `+`/`−`, current sources an arrow. Symbols are drawn along their own axis, so a part may run in any direction; captions stay upright. The drawing uses `currentColor` for the circuit, so it follows the UI theme, and fixed mid-tone colours for what is drawn over it: blue for currents, red for voltages and crosses, green for nodes.

Values use engineering prefixes with `µ` for micro; AC values read `magnitude ∠ phase°`.

## Limits

Linear parts only: no diodes, transistors, transformers or coupled inductors. One analysis per call: DC (steady state; capacitors open, inductors shorted) or AC at one frequency. No transient waveforms, only the time constant.

The layout of the parts is the caller's: they are drawn where their points say, wires are not routed, and only parallel branches are moved apart. Labels do move: a caption, a current or voltage reading, a node or probe label and a path or highlight label each take the first of a few places beside their part or point where nothing is drawn yet (the least covered one when all are taken). Loops, notes and polarity signs stay where they are put, so in a crowded mesh a long reading can still end up over something.

## Tests

`cargo run -p harness --bin plugin -- test <this folder>` from the app repo. `tests/golden/` snapshots the divider drawing, an annotated figure and a nodal-analysis report; after an intended change, rewrite them with `PLUGIN_UPDATE_GOLDEN=1` and look at the SVGs.
