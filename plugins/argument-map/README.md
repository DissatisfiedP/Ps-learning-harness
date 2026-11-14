# argument-map

Draws an argument reconstruction as a card the learner keeps referring to (argument mode, `crates/harness/prompts/tutor/modes/argument.md`; van Gelder 2005 on argument mapping). One tool, `argument_map_show`, returns the card as an artifact; the renderer `render` redraws it from the stored data.

## Layout

`src/main.luau` only wires the exports `plugin.toml` names (`show`, `render`). The rest of `src/`, one responsibility per file:

| File | Responsibility |
|---|---|
| `model.luau` | Types, the request's checks (unique ids, objection targets, blanks), and the learner-safe card stored as the artifact's data |
| `wrap.luau` | Word wrap counted in UTF-8 characters (SVG has no text flow) |
| `card.luau` | The SVG: title, premises with their objections and replies nested beneath, the conclusion, objections to the inference |
| `summary.luau` | The model-facing text, including the hidden parts' text |

## Card conventions

- Premises have a solid outline, the conclusion (marked `∴`) a heavy one, objections a dashed one and replies a dotted one, and each objection and reply carries a heading ("Objection to P3", "Reply — Hume"), so the kinds read without colour. Everything is drawn in `currentColor` and follows the UI theme.
- Objections sit under the premise they `targets`; `"conclusion"` puts them under the conclusion as "Objection to the inference".
- Replies are addressed as `<objection id>.<n>` (`O1.1` is `O1`'s first reply); the id `conclusion` is reserved.
- A part named in `blanks` is drawn as a shaded box reading `? (fill in)`; its heading (and `by`) stays, so the learner knows what kind of part is missing. Its text is left out of the SVG and of the stored data, so neither a replay nor the renderer can reveal it. The tool's text gives the model the hidden parts, which is how it grades the learner's answer.
- No interactive view: filling a blank is an open answer graded by the tutor with `question_ask`, and nothing on the card is worth manipulating.

## Shipping

Like `circuit`, this folder is the app repo's copy; the published package lives in the owner's registry (`docs/plugins.md` §8.1) and is installed from there. Pure computation, no permissions.

## Tests

`cargo run -p harness --bin plugin -- test plugins/argument-map`. The full and blanked Hume cards are snapshotted in `tests/golden/`.
