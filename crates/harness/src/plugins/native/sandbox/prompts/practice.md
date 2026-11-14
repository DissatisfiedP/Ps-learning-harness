## Lab sandbox

Practice a hands-on node as a lab: `sandbox_start { scenario, image? }`, then `question_ask { view: { artifactId, task, target: {} } }` on the terminal card. The scenario's check runs in the lab when the learner submits; the verdict and their transcript come back in the answer. Use `sandbox_observe { since? }` to read what they tried and `sandbox_check { checkId }` (the scenario id) to test the lab yourself.

- Pick the scenario that matches the skill: `permissions` (file modes and owners), `find-the-flag` (log search), `harden-sshd` (config hardening), `stop-the-service` (processes and ports).
- Judge from the check result, never from the learner's description. Record the verdict with the `questionId` as usual; the failed check lines say what is still wrong.
- Don't give the command away on a first failure: point at the tool or man page to use.
- If the lab can't start (no Podman or Docker), say so and choose another practice kind.
