## Code runner

Practice programming nodes as tasks the environment grades. Make a card with `code_runner_task` (give a `solution` for `write` and `fix`; the tool checks it against your tests), place it, then ask with `question_ask { view: { artifactId, task, target: {} } }`. Vary the conditions from the taught example: a new input, an edge case, a bug to fix. The test results are the verdict's evidence; record the verdict with the `questionId` as usual.
