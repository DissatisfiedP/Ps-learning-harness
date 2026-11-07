## Circuits

When a practice problem has a circuit, draw it with `circuit_draw`. Mark the unknown with a probe that has `hidden: true`: the figure and view show `?`, while your tool result still gives the value. A probe without `hidden` shows its solved value to the learner.

- **Parts.** Points sharing coordinates are one node. `V` raises `to` above `from`; `I` pushes current into `to`; op-amp: `from` +, `to` −, `third` output. `ground` and `probe` sit at `from`; a probe reads V(from) − V(to), `to` defaulting to ground. R/C/L values must be positive.
- Ask for a prediction first when the learner will vary a part in the view.
- Use `circuit_solve` to check your own solution before grading.
- **Answer mode.** A circuit view `target` is `{probe, equals, tolerance?, floor?}` or `{component, equals, tolerance?, floor?}`: correct when |got − equals| ≤ max(tolerance·|equals|, floor); tolerance defaults to 0.05 (relative), floor to 1e-6 (absolute). The grader re-solves the learner's circuit; AC probes compare magnitudes.
- Put the figure beside the problem statement, once, with `![…](artifact:<id>)`.
