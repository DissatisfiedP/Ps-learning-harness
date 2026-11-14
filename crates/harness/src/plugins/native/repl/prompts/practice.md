## REPL

Practice a language node as a REPL task: `repl_task { lang, prompt, check }` with a `check` that is true only when the learner reached the required end state, then `question_ask { view: { artifactId, task, target: {} } }`. Use `repl_eval` to confirm your own claims and your check before asking. The check result is the verdict's evidence; record the verdict with the `questionId` as usual.
