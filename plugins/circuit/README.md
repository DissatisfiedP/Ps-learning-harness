# circuit

Reference plugin: draws, solves (DC and single-frequency AC, modified nodal analysis) and grades small circuits. Part conventions are in `schemas/circuit.json`.

## Layout

`src/main.luau` only wires the exports `plugin.toml` names (`circuit_draw`, `circuit_solve`, `render`, `solve`, `grade`). The rest of `src/`, one responsibility per file:

| File | Responsibility |
|---|---|
| `model.luau` | Types (parts, circuits, solutions), request defaults, grid points, part helpers, the learner-safe copy of a solution |
| `phasor.luau` | DC or AC number domain: source values and admittances |
| `netlist.luau` | Connectivity: which points form a node, node numbers and names |
| `solver.luau` | Modified nodal analysis: stamping, connectivity warnings, the solve, probes and branch currents |
| `format.luau` | Engineering notation and phasor text |
| `report.luau` | The model-facing summary text |
| `symbols.luau` | One SVG symbol per part |
| `schematic.luau` | Grid-to-pixel layout and the SVG document |
| `grade.luau` | Answer-mode grading by re-solving the submitted circuit |

## Symbol style

Schematics use IEC 60617 symbols: a resistor is a rectangle, not the ANSI zigzag. The rectangle stays legible at the small sizes figures render at on phones, is drawn with one `rect` that scales cleanly with stroke width and theme colour, and is the convention most non-US textbooks use. Inductors are drawn as loops in both standards; sources are circles with `+`/`−` (V) or an arrow (I).

Values use engineering prefixes with `µ` for micro; AC values read `magnitude ∠ phase°`.

## Tests

`cargo run -p harness --bin plugin -- test plugins/circuit`. The divider drawing is snapshotted in `tests/golden/divider.svg`.
