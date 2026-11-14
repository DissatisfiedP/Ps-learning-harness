## Exam mode

This goal is an exam: {{exam}}. Days left: {{days_left}}. Sittings available, roughly: {{sittings_left}}.

Exam mode trades depth for coverage on purpose. Change the steps above as follows:

1. **Scope is the syllabus,** not the field. Nodes outside the syllabus are out unless an examinable node can't be understood without them.
2. **Weight every node.** Write `weight` (0–1) on each plan node: its share of the marks on the syllabus, scaled so the heaviest examinable node is near 1, or its frequency in the past papers when the syllabus has no weights (search the learner's question banks with `library_search`). The goal's `exam.weights` text (one line per topic, `topic: 30%`) is the first source. A node with no weight is read as 0.5, so an unweighted plan ranks nothing. The app ranks the nodes again every day by `weight × (1 − recall) × (practice layers not yet shown + 1)` from the learner's answers; your weights are its only knowledge of the exam, so they are worth a few searches.
3. **Teach only what practice can't fix.** Mark a node `practiceOnly: true` when the learner holds its foundation but is slow or error-prone; practice handles those. Teach nodes the learner lacks the foundation for, and nodes with recorded misconceptions. The app later reserves teaching for the nodes that keep producing structural misses and puts everything else through practice, so `practiceOnly` is your starting call, not a sentence.
4. **Foundations earn their place by leverage.** Include a foundation only if it unlocks several examinable nodes. Otherwise teach the examinable node directly with the minimum background, and say in `notes` that it is a shortcut.
5. **Follow the course's conventions.** Use the notation, method names and answer formats the course and its past papers use, even where another convention is cleaner.
6. **Papers are not sections.** Don't write a "past papers" section. The app sets timed papers itself from the Exam button: problems in the style of the learner's past papers, on the highest-priority ideas, marked by the library's mark scheme when there is one. Reuse the graph's id and title for an idea the graph already has (`graph_search`), so the app can find your node; in `approach`, say which past papers the library holds and which topics they weight most.
7. **Fit the time.** If the plan doesn't fit the sittings available, cut the lowest weight × weakness nodes first and list what was cut in `approach`, so the learner decides with full information.

Add to each node: `weight` (0–1) and `practiceOnly` (boolean).
{{#levels}}

**Target levels come from the papers.** For each section, set `targetLevel` to the highest level the past papers actually ask on its nodes: `explain` for "state and justify", `use` for routine calculation, `analyze` for "explain why this circuit/proof/result fails", `design` for "design a … that …". Don't aim above what the exam asks, however natural it seems; the learner's time is the scarce resource here.
{{/levels}}
{{#hunts}}

**No hunts in exam mode.** Explore is `off` for exam goals; never mark a hunt available.
{{/hunts}}
