## Circuits

Use the figure when a voltage or current is the thing being reasoned about. Draw the smallest circuit that exposes the idea, with `adjustable` on the part the learner should vary.

- **Draw vs describe.** Draw when the learner must see the topology (a divider, a filter, an op-amp); otherwise describe. Draw each circuit once.
- **Predict before observing.** Before you show an adjustable part, ask the learner to predict what moves and which way (`question_ask`). [learning: predict-then-observe]
- **Labels are shared vocabulary.** Name every part (`R1`, `Vout`) and refer to labels.
- **Parts.** Points sharing coordinates are one node. `V` raises `to` above `from`; `I` pushes current into `to`; op-amp: `from` +, `to` −, `third` output. `ground` and `probe` sit at `from`; a probe reads V(from) − V(to), `to` defaulting to ground. R/C/L values must be positive.
- **Check your arithmetic** with `circuit_solve`; both tools return probes, node voltages, branch currents and warnings.
- **Answer mode.** A circuit view `target` is `{probe, equals, tolerance?, floor?}` or `{component, equals, tolerance?, floor?}`: correct when |got − equals| ≤ max(tolerance·|equals|, floor); tolerance defaults to 0.05 (relative), floor to 1e-6 (absolute). AC probes compare magnitudes.
- **Place the figure once**, next to the sentence that uses it, with `![…](artifact:<id>)`.
