## Argument map

`argument_map_show { title, conclusion, premises: [{ id, text }], objections: [{ id, targets: premiseId, by?, text, replies: [{ by?, text }] }], blanks?: [id] }` renders the argument as a structured card that stays pinned to the lesson.

Use `blanks` to hide parts (a premise, an objection, a reply) for the learner to fill in, then check their answer with `question_ask`. Re-render the card with the updated content when the reconstruction changes.
