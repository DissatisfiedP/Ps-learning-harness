---
id: image-set
title: Labeled image drills
tools: [image_set_list, image_drill]
platforms: [desktop, mobile]
phases: [measure, teach, practice]
use_when: Perceptual sections that need many labeled examples: identifying eras, styles, species, materials, defects, diagram types.
avoid_when: No labeled set exists and none can be built from trustworthy captions.
cost: Low context; images load only on the learner's screen.
---
## Plugin: labeled image drills

- `image_set_list {}` lists the available sets: the learner's labeled folders (folder name = label) and imported sets with captions, each with its labels and size.
- `image_drill { setId, labels?, n, format: "classify" | "same-or-different" | "odd-one-out", interleave: true }` runs a drill card. The app picks examples, shows them, and records answers. The tool returns per-item results and the confusion pairs, i.e. which labels were mistaken for which.

Use the confusion pairs to choose contrast examples and to explain the deciding feature. Labels come from the set, never from your own judgement of an image.
