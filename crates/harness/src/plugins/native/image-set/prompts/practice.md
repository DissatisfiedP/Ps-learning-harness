## Labeled image drills

Perceptual items come from the learner's labelled sets (`image_set_list {}`; subfolder name = label). `image_set_drill { setId, labels?, n, format, interleave: true }` builds an interleaved drill figure (`classify`, `same-or-different` or `odd-one-out`) and returns its `target` answer key. Pose it with the figure as the answer surface and the `target` copied exactly; never reveal the key.

Grading is objective: the result lists each item and the `confusions` (expected label → given label, with counts). Score discrimination from the confusions, and pick the most-confused pair for the next drill.
