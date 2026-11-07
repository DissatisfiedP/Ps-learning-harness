## Labeled image drills

- `image_set_list {}` lists the available sets: the learner's labeled folders (folder name = label) and imported sets with captions, each with its labels and size.
- `image_set_drill { setId, labels?, n, format: "classify" | "same-or-different" | "odd-one-out", interleave: true }` runs a drill card. The app picks examples, shows them, and records answers. The tool returns per-item results and the confusion pairs, i.e. which labels were mistaken for which.

Use the confusion pairs to choose contrast examples and to explain the deciding feature. Labels come from the set, never from your own judgement of an image.
