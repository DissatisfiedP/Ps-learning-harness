## Circuits

When a practice problem has a circuit, draw it with `circuit_draw` and check your own solution against the tool's text (probes, node voltages, currents, powers), never by hand.

- **The unknown is a hidden probe.** Give the asked quantity a probe with `hidden: true`: at a point (voltage), or on a `part` with `quantity` voltage, current or power. The figure shows `?` and no solved value anywhere while a probe is hidden; your tool result still has every value.
- **Layout.** Points are `[x, y]`, y upwards; parts horizontal or vertical, two grid steps long. Pins sharing a point join; a pin on a wire joins it; parts between the same two points are parallel branches.
- **Polarity.** `V` raises `to` above `from`; `I` pushes current into `to`; dependent sources (`VCVS`, `VCCS`, `CCVS`, `CCCS`) likewise, with `gain` and `control`. Op-amp: `from` +, `to` −, `third` output.
- **State the problem in the figure's terms.** Label parts (`R_1` draws a subscript) and mark reference directions with `current`/`voltage` annotations. Leave out `loop`s and `show.nodes` when choosing the method is the exercise.
- **For your grading**, `nodal`, `superposition` and `equivalent` add the equations, each source's share and the Thevenin/Norton values to the tool text only.
- **Answer mode.** A view `target` is `{probe, equals, tolerance?, floor?}` or `{component, equals, …}`: correct when |got − equals| ≤ max(tolerance·|equals|, floor); defaults 0.05 and 1e-6. The grader re-solves the learner's circuit.
- Put the figure beside the problem statement, once, with `![…](artifact:<id>)`.
