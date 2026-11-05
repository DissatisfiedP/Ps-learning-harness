---
id: argument-map
title: Argument map card
tools: [argument_show]
platforms: [desktop, mobile]
phases: [teach, practice]
use_when: Argument-mode sections with more than one argument or more than two objections, where the learner benefits from seeing the structure.
avoid_when: A single short argument that fits in a numbered list.
cost: Low.
---
## Plugin: argument map

`argument_show { title, conclusion, premises: [{ id, text }], objections: [{ id, targets: premiseId, by?, text, replies: [{ by?, text }] }], blanks?: [id] }` renders the argument as a structured card that stays pinned to the lesson.

Use `blanks` to hide parts (a premise, an objection, a reply) for the learner to fill in, then check their answer with `question_ask`. Re-render the card with the updated content when the reconstruction changes.
