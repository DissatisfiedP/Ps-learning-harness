## Labeled image drills

- `image_set_list {}` lists the sets in the learner's `image-sets/` folder (subfolder name = label), each label with its image count.
- `image_set_drill { setId, labels?, n, format, interleave: true }` builds a drill figure. Formats: `classify` (name each image's label), `same-or-different` (pairs), `odd-one-out` (triples). Items interleave labels. The result gives the artifact id and a `target` (the answer key).

Run it as a check: place the figure, then `question_ask` with `view: { artifactId, task, target }`, copying `target` exactly. Never say the key. The learner answers in the figure; you get per-item results and `confusions` (which label was taken for which). Then call `verdict_record` as usual.

Use the confusion pairs to choose the next contrast examples and to name the deciding feature. Labels come from the set, never from your own judgement of an image.
