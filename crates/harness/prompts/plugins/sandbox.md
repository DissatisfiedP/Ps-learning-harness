---
id: sandbox
title: Lab sandbox
tools: [lab_start, lab_observe, lab_check]
platforms: [desktop]
phases: [measure, teach, practice]
use_when: Hands-on systems or security skills that must be performed against a real target (enumeration, exploitation, hardening, log analysis, networking).
avoid_when: The section is conceptual; a diagram or a worked transcript would do.
cost: Starts a local container or VM; needs Docker or Podman on the learner's machine.
---
## Plugin: lab sandbox

A disposable local lab is available. The learner works in a terminal card inside the app; you see what they run and its output.

- `lab_start { scenario, image? }` boots a lab from the scenario catalog (`{{lab_scenarios}}`) and returns its targets and network.
- `lab_observe { since? }` returns the learner's commands and their output since the given point.
- `lab_check { checkId }` runs the scenario's success check (flag found, service hardened, port closed) and returns pass or fail with details.

Rules:
- Every target is inside the lab network. Never direct the learner at hosts outside it.
- The learner types the commands; you don't run attacks for them. Read their output with `lab_observe` before giving feedback.
- Grade a task only through `lab_check`, never from the learner's description.
