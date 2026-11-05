---
id: code-runner
title: Code runner
tools: [code_task]
platforms: [desktop, mobile]
phases: [measure, teach, practice]
use_when: Programming sections where the learner must write, read, debug or predict the output of code. Languages on every platform: JavaScript, TypeScript, Python (Pyodide). Desktop also: {{desktop_languages}}.
avoid_when: The section is about concepts that don't need running code, e.g. what a hash table is for.
cost: Python loads a 10–20 MB runtime the first time it is used on a device.
---
## Plugin: code runner

The learner can write and run code inside the lesson.

`code_task { language, kind, prompt, starter?, tests: [{ name, input?, expected }], predictFirst? }`
- `kind`:
  - `write`: implement something;
  - `fix`: the starter compiles but behaves wrongly;
  - `read`: predict the output, with no editing;
  - `error`: explain a given error message.
- `predictFirst: true` asks the learner to commit to the expected output before running.

The tool returns the learner's final code, every run's output or error, the test results, and their prediction. Grade on the test results. Then comment on the code itself: a pass with fragile code is worth one remark. Write your tests before writing the task, and run your own reference solution against them mentally. A wrong test grades the learner against an error.
