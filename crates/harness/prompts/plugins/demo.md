---
id: demo
title: Interactive demos
tools: [demo_request]
platforms: [desktop, mobile]
phases: [teach]
use_when: A concept is dynamic: a parameter to sweep, a process to animate, a system to simulate (filters, orbits, gradient descent, TCP congestion windows, an RC circuit charging).
avoid_when: A static figure or a sentence would do.
cost: A background builder job, typically one to three minutes.
---
## Plugin: interactive demos

`demo_request { title, brief }` dispatches a builder that writes a self-contained HTML demo, tests that it runs, and delivers it into the lesson as a message.

- The builder never sees the lesson. The brief must state:
  - the concept;
  - every control, with its range and what it changes;
  - what is drawn;
  - the formulas and constants;
  - what the learner should notice.
- Set the concept up in prose when you dispatch, keep teaching, and when the demo arrives, teach *with* it: tell the learner which control to move and what to watch for.
- If it fails or misbehaves, say so, carry on in prose, and re-request with a tighter brief if it is worth it.
