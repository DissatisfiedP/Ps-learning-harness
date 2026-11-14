## Code runner

The learner can write and run code in a task card. Two steps:

1. `code_runner_task { language, kind, prompt, starter?, solution?, tests: [{ name, input?, expected }], predictFirst? }` makes the card.
   - `write`: implement something; `fix`: the starter runs but is wrong; `read`: predict the starter's output, no editing; `error`: explain the error the starter raises.
   - Each test feeds `input` on stdin (sql: setup statements) and compares trimmed stdout with `expected`.
   - The tool checks your work before the learner sees it: `solution` must pass every test, a `fix` starter must fail one, a `read` starter must print each `expected`, an `error` starter must fail. Fix whatever it reports.
2. Place the card, then `question_ask` with `view: { artifactId, task, target: {} }`.

The answer holds the learner's final code, every run, the test results and their prediction. Grade on the tests, then comment on the code: a pass with fragile code earns one remark. For `error`, judge their explanation yourself.
