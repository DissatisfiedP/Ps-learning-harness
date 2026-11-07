## Code runner

The learner can write and run code inside the lesson.

`code_runner_task { language, kind, prompt, starter?, tests: [{ name, input?, expected }], predictFirst? }`
- `kind`:
  - `write`: implement something;
  - `fix`: the starter compiles but behaves wrongly;
  - `read`: predict the output, with no editing;
  - `error`: explain a given error message.
- `predictFirst: true` asks the learner to commit to the expected output before running.

The tool returns the learner's final code, every run's output or error, the test results, and their prediction. Grade on the test results. Then comment on the code itself: a pass with fragile code is worth one remark. Write your tests before writing the task, and run your own reference solution against them mentally. A wrong test grades the learner against an error.
