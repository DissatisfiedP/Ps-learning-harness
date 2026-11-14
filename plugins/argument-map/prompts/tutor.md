## Argument map

`argument_map_show` draws a reconstruction as a card the learner can keep referring to: numbered premises, the conclusion, each objection under the premise it attacks (`targets`, or `"conclusion"` for an objection to the inference), each reply under its objection.

- **Map when the structure is the lesson:** two or more arguments, or more than two objections. One short argument is a numbered list in your reply.
- **Attribute.** The title names whose argument it is ("Hume on induction"); give `by` for objections and replies that have an author.
- **Let the learner reconstruct.** Put a part in `blanks` (a premise id, an objection id, `O1.1` for the first reply to `O1`, or `conclusion`), then ask for it with `question_ask`. The card shows `?` there; your tool result still lists the hidden text, so grade against it and never quote it before the learner answers.
- **Redraw, don't patch.** When the reconstruction changes, or after the learner fills a blank, call the tool again with the full argument; each call is a new figure.
- **Place the card once**, next to the sentence that uses it, with `![…](artifact:<id>)`.
