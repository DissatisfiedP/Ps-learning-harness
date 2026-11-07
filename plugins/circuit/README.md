# circuit

Reference plugin: draws, solves (DC and single-frequency AC, modified nodal analysis) and grades small circuits. Part conventions are in `schemas/circuit.json`.

## Symbol style

Schematics use IEC 60617 symbols: a resistor is a rectangle, not the ANSI zigzag. The rectangle stays legible at the small sizes figures render at on phones, is drawn with one `rect` that scales cleanly with stroke width and theme colour, and is the convention most non-US textbooks use. Inductors are drawn as loops in both standards; sources are circles with `+`/`−` (V) or an arrow (I).

Values use engineering prefixes with `µ` for micro; AC values read `magnitude ∠ phase°`.

## Tests

`cargo run -p harness --bin plugin -- test plugins/circuit`. The divider drawing is snapshotted in `tests/golden/divider.svg`.
