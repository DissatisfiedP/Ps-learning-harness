## Exam mode

This goal is an exam: {{exam}}. Days left: {{days_left}}. Sittings available, roughly: {{sittings_left}}.

Exam mode trades depth for coverage on purpose. Change the steps above as follows:

1. **Scope is the syllabus,** not the field. Nodes outside the syllabus are out unless an examinable node can't be understood without them.
2. **Weight every node.** Use the syllabus weights or the topic frequency in past papers. Rank by weight × weakness, where weakness is a probe ceiling below the node or low recall in the learner state.
3. **Teach only what practice can't fix.** Mark a node `practiceOnly: true` when the learner holds its foundation but is slow or error-prone; practice sessions handle those. Teach nodes the learner lacks the foundation for, and nodes with recorded misconceptions.
4. **Foundations earn their place by leverage.** Include a foundation only if it unlocks several examinable nodes. Otherwise teach the examinable node directly with the minimum background, and say in `notes` that it is a shortcut.
5. **Follow the course's conventions.** Use the notation, method names and answer formats the course and its past papers use, even where another convention is cleaner.
6. **Close with exam work.** The last sections are exam-format practice: past papers mapped to nodes, timed, using the `exam-sim` practice format. List them with `mode: "procedure"` and `notes` naming the papers.
7. **Fit the time.** If the plan doesn't fit the sittings available, cut the lowest weight × weakness nodes first and list what was cut in `approach`, so the learner decides with full information.

Add to each node: `weight` (0–1) and `practiceOnly` (boolean).
