## Circuits

Draw the smallest circuit that exposes the idea, once. Solve with the tool, never by hand: its text gives probes, node voltages, currents, powers, series/parallel groups and warnings.

- **Layout.** Points are `[x, y]`, y upwards; parts run horizontally or vertically, two grid steps long. Pins sharing a point join; a pin on a wire joins it. Parts between the same two points are drawn side by side as parallel branches.
- **Polarity.** `V` raises `to` above `from`; `I` pushes current into `to`; dependent sources (`VCVS`, `VCCS`, `CCVS`, `CCCS`) likewise, with `gain` and `control`. A part's voltage is V(from) − V(to), its current from → to. Op-amp: `from` +, `to` −, `third` output.
- **Labels are shared vocabulary.** Name every part; `R_1`, `V_out` draw subscripts. Refer to labels in your prose.
- **Make the method visible.** `show.nodes` letters the nodes; `nodal: true` adds the KCL equations in those letters. `annotations` draw mesh `loop`s, a current `path`, `current`/`voltage` reference marks, a `cross` over a removed part, a `highlight` box (a supernode). `off: true` kills a source for superposition; `superposition: true` and `equivalent` (Thevenin/Norton) add those results.
- **Predict before observing.** Before showing `adjustable` parts, a `toggle` switch, or solved `show.voltages`/`show.currents`, ask the learner what they expect (`question_ask`). [learning: predict-then-observe]
- **Answer mode.** A view `target` is `{probe, equals, tolerance?, floor?}` or `{component, equals, …}`: correct when |got − equals| ≤ max(tolerance·|equals|, floor); defaults 0.05 and 1e-6.
- **Place the figure once**, with `![…](artifact:<id>)`.
