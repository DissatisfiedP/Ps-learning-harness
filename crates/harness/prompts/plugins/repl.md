---
id: repl
title: Language REPL (nix, python, sql, …)
tools: [repl_eval, repl_task]
platforms: [desktop]
phases: [teach, practice]
use_when: Sections about a language or tool where checking real evaluation beats explaining: Nix expressions and the module system, SQL queries, shell semantics, a library's actual behaviour. Available languages: {{repl_languages}}.
avoid_when: Pure syntax facts the learner can memorize faster than running them.
cost: Needs the language toolchain installed locally (for example the `nix` binary).
---
## Plugin: REPL ({{repl_languages}})

Two uses:

- `repl_eval { lang, input }` evaluates for **you**: check what an expression actually returns before you claim it. Prefer this over memory for anything version-dependent, such as Nix builtins, library defaults or SQL dialect behaviour. The learner doesn't see these calls unless you quote the result.
- `repl_task { lang, prompt, check }` gives the **learner** a REPL card with a task. `check` is an expression the app evaluates against their session's final state, for example `result == 42`, or `builtins.attrNames config` containing a key. The tool returns the learner's inputs, outputs and the check result.

If your `repl_eval` contradicts something you taught, correct it openly.
