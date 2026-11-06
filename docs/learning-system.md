# Learning system: target levels, starting point, plan controls, hunts, field guide, calibration

Status: **design, not built.** Written 2026-11-06. This document is the source of truth for the six features below. A reviewing agent should be able to check an implementation against it section by section; §10 is the checklist.

## 0. Scope: learning system, not technical architecture

Everything here changes **how the harness teaches**:
- what the agents are told to do;
- what the goal and plan contain;
- what the learner model records;
- what the learner can control.

**Nothing here changes the technical architecture** (`docs/architecture.md` §6):
- the stack (Rust core, Tauri, plain ES-module UI);
- the agent runtime, op-log storage and replay;
- the model roles;
- sync (still not built);
- the plugin system (still scaffolding only; no real plugins are part of this work).

The implementation does touch code, but only by **extending existing mechanisms**:
- new fields on existing records (`goal`, `plan` sections, `question_ask`, `probe_done`, scope brief);
- a few new tools and session ops, of the same shape as today's;
- extra gate conditions in the existing phase machine;
- UI controls on existing surfaces (plan card, flow, Map, close).

No new subsystem, process, store or dependency is introduced.

**Priority.** Learning outcomes decide the features. The UI's job is to make them usable and remove everything else ("Carrel", `docs/ui-design.md`). Where `ui-design.md` conflicts with a feature here, `ui-design.md` is revised; it doesn't veto the feature.

### Feature flags

The prompts are live, so every prompt instruction that depends on unbuilt code sits inside a template flag block (`{{#flag}} … {{/flag}}`). The composer drops a block whose flag isn't set (`crates/harness/src/prompts.rs::render`), so today's app is unaffected. Turn a flag on in the same change that builds its code.

| Flag | Feature | Prompt files with blocks |
|---|---|---|
| `levels` | §2 Target levels | `tutor/phases/measure.md`, `tutor/phases/teach.md`, `planner/base.md`, `planner/exam.md`, `shared/questions.md` |
| `starting_point` | §3 Starting point | `tutor/phases/measure.md` |
| `plan_controls` | §4 Plan controls (incl. known sections) | `planner/base.md`, `tutor/phases/teach.md` |
| `hunts` | §5 Hunts, debrief, heuristics, frontier | `tutor/base.md`, `tutor/phases/measure.md`, `tutor/phases/teach.md` (+ the whole `tutor/hunt.md`), `planner/base.md` |
| `field_guide` | §6 Field guide | `jobs/researcher.md`, `tutor/phases/teach.md` |
| `calibration` | §7 Calibration feedback | `shared/learner-signals.md`, `tutor/phases/teach.md` |
| `known_section` | §4, per compose: set only when `plan_controls` is on and the current section has `assumedKnown: true` | `tutor/phases/teach.md` |
| `hunt_active` | §5, per compose: set only when `hunts` is on and the current section's hunt is enabled and not skipped | `tutor/phases/teach.md` |

Purpose `course` is introduced together with `levels`: the `goal_set` schema's `purpose` enum gains `"course"` in that change.

`coach/base.md` and `jobs/graph-builder.md` are not live yet (no coach, no graph), so they're written for the target directly, without flags.

---

## 1. Principles behind all six features

1. **The learner owns *what* and *how far*; the system owns *how*.** Learners know their goals, interest, time and how deep they want to go. They're poor judges of learning methods: they prefer re-reading to testing, blocked to mixed practice, and easy material to hard (Kirschner & van Merriënboer 2013). So the learner gets controls over scope, depth, exploration and pace. Ordering, roots, teaching mode and checks stay with the planner and tutor, and the learner's choices are applied *through* them, never around them.
2. **Choice helps when it's about things that matter to the learner.** Offering meaningful choices increases intrinsic motivation and effort (Patall, Cooper & Robinson 2008). Defaults matter too: anything preselected tends to be accepted, so learning-relevant options are never preselected against the learner's interest.
3. **Evidence over feeling, everywhere.** Feeling that you understand isn't evidence of understanding (Deslauriers et al. 2019; Rozenblit & Keil 2002). Searching the web inflates people's estimate of their own knowledge (Fisher, Goddu & Keil 2015). Overconfident learners stop studying too early and retain less (Dunlosky & Rawson 2012). So:
   - only a passed check closes a section;
   - nothing the learner reads, finds or says about themselves counts as evidence until it's checked;
   - the learner is shown how well their confidence matches their results.

---

## 2. Target levels

### What
Every section of a plan has a **target level**: how far the learner should get with that material. There are four levels, in plain words:

| Level | Covers (revised Bloom) | What the learner can do | Example check (voltage divider) |
|---|---|---|---|
| **Explain** | remember + understand | State it and say why it holds | "Why is the current the same through both resistors?" |
| **Use** | apply | Apply it to a new case | "Find $R_1$ given 3 V across 1 kΩ on a 12 V supply." |
| **Analyze** | analyze + evaluate | Take a situation apart, diagnose it, judge between options | "This divider reads 2.1 V instead of 3 V once the load is attached. Why?" |
| **Design** | create | Build something new that meets constraints | "Choose resistors that give 3.3 V from 5 V and stay within 5% when driving a 10 kΩ load." |

### Why
- **The levels are a vocabulary for "how far",** which the goal and plan lacked. "Understand transformers" can mean explaining attention or building a model. Without a target, the tutor over-teaches some learners (e.g. design tasks for someone who needs analysis for a course) and under-teaches others.
- **They come from the revised Bloom taxonomy** (Krathwohl 2002), which crosses six cognitive processes with four kinds of knowledge. The four kinds map onto our node kinds (`fact` ≈ factual, `mechanism` ≈ conceptual, `procedure` ≈ procedural); metacognitive knowledge is §7.
- **Four levels, not six.** Remember and understand are merged because the method never teaches recall without understanding. Analyze and evaluate are merged because, in practice, the checks look alike. Fewer levels can be assigned more consistently **[inference]**, and the learner can read them without a glossary.
- **Not a ladder to climb from the bottom.** Practicing higher-order questions improves higher-order performance, while fact quizzes alone do not (Agarwal 2019). So the probe keeps opening high, and the tutor doesn't drill lower levels first "to build up".
- **A level is not a difficulty.** A Use question can be harder than an Analyze one, and question writers also weigh time, difficulty and the learner's experience (Lemons & Lemons 2013). The level is what kind of thing the check demands; difficulty is tuned separately.

### Where levels live (and where they don't)
- **Not in the knowledge graph.** Nodes and edges are claims and dependencies; a level describes *the learner's hold on* a node. "Build a transformer" is its own `procedure` node with `requires` edges to the concepts it uses. It is not a deeper copy of "explain transformers", and there is no "explain → build" edge.
- **Goal record:** `depth: "pass" | "solid" | "deep"`, which sets the default target levels.
- **Plan section:** `targetLevel`.
- **Checks and probe questions:** `level`, the level the question was written to demand. Writing a question *to* a level is easier and more consistent than classifying an existing question afterwards **[inference]**.
- **Learner model (derived, never stored):** per node, the highest level with a passing check, plus the check ids that show it.
- **Probe map:** per strand, `levelReached`.

### Behavior
**Planner:**
- sets `targetLevel` per section, from `depth`, the goal's `successLooksLike`, and the learner's controls (§4);
- writes the section's `exit` at that level.

Depth defaults:

| Depth | Default target | Typical use |
|---|---|---|
| `pass` | Use; Explain for background-only sections | Course or exam the learner just wants done; exam papers override (below) |
| `solid` | Use, and Analyze for the sections the goal centres on | Most goals |
| `deep` | Analyze, and Design where the field has a natural design task | Curiosity and project goals |

In exam mode the target per section comes from the past papers: the highest level the papers actually ask for on those nodes.

**Tutor:**
- writes every check with `level` set;
- uses checks at lower levels freely while teaching, but makes sure the exit is shown **at the target level**.

**Gate** (code): `section_done` requires both
1. every node in the section has a passing check (unchanged), and
2. at least one passing check in the section at `level ≥ targetLevel`.

**Probe:** brackets record the level reached, e.g. "uses the divider rule; can't analyze a loaded divider." The planner uses this to decide where to start each strand *and* how far each section must go.

**Coach** (when built): targets each node's gap between level reached and target, and adds two problem types:
- **Critique** (Analyze): find the flaw in a given solution, design or argument.
- **Design to constraints** (Design): graded against a rubric committed beforehand.

**Map:** the level appears as text in the detail line ("Reached: Use · Aim: Analyze"), not as an extra box state. The three box states stay readable at a glance.

### Alternatives considered
| Alternative | For | Against |
|---|---|---|
| No levels (today) | Simple | Depth is implicit; over- and under-teaching are invisible |
| Bloom's six levels | Standard | Distinctions the learner can't use and checks can't consistently hit |
| Levels as graph structure (nodes per level, "explain → build" edges) | Visible in the graph | Wrong model: levels describe the learner, not the knowledge; duplicates every node |
| SOLO taxonomy (Biggs) | Describes structural complexity of answers | Better for grading essays than for setting targets; another vocabulary for the learner |
| Mastery percentage per node | One number | Hides *what* the learner can do; conflates forgetting with never-learned |
| **Four plain levels as section targets, learner-model evidence and check tags** (chosen) | Says "how far" in words the learner understands; gates on real evidence | Level tags are the tutor's judgement **[inference: reliable enough at four levels]** |

### Acceptance criteria
- `goal_set` accepts `depth`; `plan_write` sections carry `targetLevel`; `question_ask` (probe, check) carries `level`; `probe_done` strands carry `levelReached`.
- `section_done` is refused when no passing check in the section is at or above `targetLevel`, and the error says which level is missing.
- The learner model reports, per node, the highest passed level and its evidence, computed from ops.
- The Map detail line shows reached and aimed levels.
- No graph node or edge stores a level.

---

## 3. Starting point

### What
At goal time the learner chooses how they're measured:

| Option | Behavior |
|---|---|
| **Measure me** (default) | The full probe: 2–4 questions per strand, 8–15 in total |
| **Quick check** | 3–5 questions in total, on the strands that most change the plan |
| **I'm new to this** | No probe questions. Every strand is recorded as `unprobed` with the reason `new`. The plan starts from the field's foundations, and the first check of each section doubles as measurement |

The choice is remembered as the learner's default in device settings (`startingPoint`) and can be changed in Settings or when it's asked.

### Why
- **Probing is valuable,** and not just for planning: attempting to answer before instruction improves later learning even when the attempt fails (the pretesting effect: Richland, Kornell & Kao 2009; prequestions: Pan & Carpenter 2023). So "Measure me" stays the default.
- **For a complete beginner, the probe's planning value is close to zero.** Every strand bottoms out, and a run of "I don't know" answers is discouraging. A LearnHarness user publicly complained about exactly this, because he always learns topics from scratch.
- **"I'm new to this" doesn't skip measurement; it moves it.** The first check of each section shows whether the learner was really new to that part. If the learner breezes through, the tutor sends the planner an `ahead` request with the evidence, and the plan shrinks.
- **Asking "Have you studied any of this before?" is allowed.** It asks for a fact about the learner's history, not a rating of their skill, so it doesn't break the rule against self-assessment. The tutor may use it to suggest a starting point, but the learner chooses.

### Behavior
- `goal_set` gains `startingPoint: "measure" | "quick" | "new"`.
- With `new`, `probe_done` may be called with zero questions. It still waits for the scope brief, because the planner needs it.
- With `quick`, the tutor prioritises strands where a wrong guess would change the plan most (e.g. a strand that decides whether a foundation section is needed).
- The planner treats `unprobed: new` strands as starting from their roots, and says so in `approach`.

### Alternatives considered
| Alternative | For | Against |
|---|---|---|
| Always probe (today) | Best plan | Wastes beginners' time; the reported complaint |
| Never probe | Fast | Re-teaches what experienced learners hold; misses misconceptions |
| Infer the starting point automatically from the learner model | No question | No data for new topics, which is exactly the case in question |
| **Learner-chosen, remembered default, measurement moved into teaching for "new"** (chosen) | Fits both kinds of learner; nothing lost for "new" | One more question at goal time (skipped when the default is set) |

### Acceptance criteria
- With `startingPoint: "new"`, no `question_ask role:"probe"` is issued, and `probe_done` succeeds once the brief has arrived.
- With `quick`, the tutor asks at most five probe questions; the count is visible in the ops.
- The default persists in `settings.json`.
- In `new` mode, a fast pass of a section's first check by a wide margin leads to an `ahead` `plan_change_request` that cites the evidence.

---

## 4. Plan controls

### What
The plan card gets real controls. They're hidden by default: a **"⋯" menu** on the plan title and a "⋯" on each section open them.

**Whole plan (⋯ on the title):**
- **Depth:** Just pass / Solid / Deep (§2).
- **Explore:** Off / Suggest / On (§5).
- **Sitting length** (minutes).
- **Style:** "Let me reason it out" / "Explain it to me", the default for sections.

**Each section (⋯ on the section):**
- **How far:** Explain / Use / Analyze / Design.
- **Include:** Keep / Don't need this.
- **"Parts I already know…"**: a short free-text comment, e.g. "I already know KVL, just not loaded dividers."
- **Tools:** checkboxes, as today.
- **Hunt:** a toggle, shown only when the planner marked a hunt available (§5).
- **Note for the planner**: free text.

**Visible without opening anything:**
- the plan itself, as today;
- a short line under each edited section ("Analyze only", "Hunt on", "You know part of this");
- greyed known sections.

**Sending:**
- edits collect locally while the learner edits;
- the one primary button becomes **"Send changes"**, with an optional comment, plus "Discard";
- **Approve** is available only when no edits are pending.

### Why
- Planning from a chat prompt alone (LearnHarness) leaves the learner guessing what can be changed and how to say it. Structured controls name the choices that actually change a plan, and the free-text note covers the rest.
- The split follows principle 1: the controls cover what the learner knows best (scope, depth, time, interest). The planner keeps prerequisites and teaching method, and reconciles the learner's edits with them.
- Hiding the controls behind "⋯" keeps the plan readable at a glance (`docs/ui-design.md` §2.3, extraneous load) while leaving the controls one click away.

### How the planner handles edits
All edits go to the planner as one structured diff (`planEdit` op) plus the learner's comment. The planner:
- **"Don't need this":** removes the section. If a later section depends on it, it either keeps a minimal bridge inside the dependent section's notes or explains on the card why the section is needed. The learner can insist.
- **"Parts I already know…":** **splits the section into two**:
  - a **known section**, `assumedKnown: true`, holding the nodes the learner says they know, shown greyed with "You said you know this";
  - the remaining section, taught normally.

  If the comment covers the whole section, the whole section becomes known.
- **How far:** sets `targetLevel` and rewrites the `exit` to match. If the level conflicts with the goal (e.g. Explain on a section whose node is "design a filter"), it warns on the card in one sentence but applies the learner's choice.
- **Depth, style, sitting length:** re-derives section defaults that the learner hasn't set explicitly; never overrides an explicit section setting.
- **Explore:** see §5. Turning it off removes all hunt toggles.
- **Unclear request:** it may ask **one question on the card** with `plan_ask { question, options[] }` (a choice with an "other" field), then wait for the answer. This is the planner's only direct line to the learner. It keeps the planner a planner, not a second tutor.

Re-approval rules don't change: removing sections or changing the goal needs approval; other changes apply with a notice. The same controls are available on the plan sheet during teaching; those edits go through the existing background planner job.

### Known sections, during teaching
When teaching reaches an `assumedKnown` section, the tutor:
1. asks **one** check, at the section's target level, tagged to the hardest node in the section;
2. **pass:** `section_done`. The gate for known sections is one passing check in the section at `≥ targetLevel`;
3. **miss:** code records `knownRefuted` (the section loses `assumedKnown`, un-greys, and the normal per-node gate applies), and the tutor teaches the section right there. No trip to the planner is needed, because the section is already in the plan.

**Why verify instead of trusting.** "I already know this" is a self-assessment, and self-assessments are unreliable, especially for overconfident learners (Dunlosky & Rawson 2012). One check costs about a minute; building on a false foundation costs the rest of the session.

### Alternatives considered
| Alternative | For | Against |
|---|---|---|
| Chat-only plan changes (LearnHarness, and today's "Suggest a change") | Flexible | The learner doesn't know what's changeable; ambiguous requests |
| Learner edits the plan directly (drag sections, edit nodes) | Full control | Breaks prerequisites; the learner owns method decisions they're poor at |
| Controls always visible | Discoverable | Clutters the plan for the majority who approve as-is |
| Trust "I already know this" | Fast | Builds on unchecked self-reports |
| Send the learner back to the planner when a known section fails its check | Planner sees everything | A round trip for something the plan already contains |
| **Hidden "⋯" controls, diff to the planner, split-and-verify known parts** (chosen) | Learner sets what and how far; planner keeps the route coherent; nothing unverified is built on | More planner calls when learners edit |

### Acceptance criteria
- The plan card shows "⋯" on the plan and on each section; the controls are hidden until opened.
- Edits are batched; "Send changes" writes one `planEdit` op and starts one planner job.
- A "parts I already know" comment yields two sections, one with `assumedKnown: true`, rendered greyed.
- A known section closes on one passing check at its target level; a miss writes `knownRefuted` and switches to the normal gate without a planner job.
- The planner can call `plan_ask`; the question renders on the card, and the answer resumes the planner.
- Approve is disabled while edits are pending.

---

## 5. Hunts and debriefs

### What
Before a section is taught, the learner may go on a **hunt**: an assignment to find things out for themselves, using the web, forums, videos, papers, people. They come back when they like, and the tutor **debriefs** what they found before teaching the section.

### Why
- **Exploring before being taught deepens understanding when instruction follows.** Learners who first explore contrasting cases or attempt a problem, and are then taught, understand concepts better than those taught first (Schwartz & Bransford 1998; Schwartz & Martin 2004). A meta-analysis finds the "productive failure" effect real for conceptual knowledge when done with fidelity (Sinha & Kapur 2021). Explanation lands better because the learner arrives knowing what they're missing and holding their own questions (Loibl, Roll & Rummel 2017).
- **Curiosity improves memory,** including for material met while curious (Gruber, Gelman & Ranganath 2014). Searching to learn is its own studied activity (Vakkari 2016).
- **Knowing a field includes knowing its people and places.** Expertise grows partly through taking part in a community of practice: its norms, its folk knowledge, who to ask (Lave & Wenger 1991). A tutor alone can't give a learner that.
- **Breadth.** A hunt surfaces perspectives, applications and adjacent topics that a minimal route deliberately skips.

**But unguided discovery loses to explicit instruction.** *Guided* discovery, with a clear question, scaffolding and feedback afterwards, beats both (Alfieri et al. 2011; Kirschner, Sweller & Clark 2006 on minimal guidance). And searching inflates how much people think they know (Fisher, Goddu & Keil 2015). So:
- a hunt always has a **question**, never just a topic;
- it is always followed by a **debrief that corrects**;
- it **never counts as evidence**.

### When a hunt can happen
**Never decided by the system alone:**
- **Explore setting** (goal record; editable on the plan card): `off` / `suggest` / `on`.
  - `off` is the default, and isn't asked about, for `purpose: "course" | "exam"`.
  - For curiosity, project and work goals, the tutor asks once at goal time.
  - `on` only means available hunts are shown expanded on the plan card. It never enables one.
- **The planner may mark a section `hunt.available`,** with a one-line `reason`, only when **all four** hold:
  1. Explore is `suggest` or `on`.
  2. **The material isn't fully settled:** the strand has practitioner lore, a live debate, several competing explanations, or real applications the learner hasn't met. Settled introductory content (e.g. most of calculus 1) fails this test, and hunts there add little.
  3. **Readable sources exist:** at least one venue whose entry level the learner meets now (§6). A calculus 1 learner can't usefully read research mathematics; a learner who has done op-amp basics can read the EEVblog forum.
  4. **Time allows:** no hunts when the deadline is close.
- **Only the learner enables a hunt** (toggle on the plan card or the plan sheet). Code rejects `plan_write` setting `hunt.enabled`.
- **Always skippable,** before or during, at no cost. The section simply starts with normal teaching.

### The hunt card
At the start of a section with an enabled hunt, the tutor calls `hunt_send`:
```
hunt_send { sectionId,
  question,                        // a question to answer, never a topic
  lookAt: [{ kind, venue?, why }], // kinds: forum thread, datasheet, lecture, hobbyist video, paper, docs, person to ask
  bringBack: [string] }            // what to return with
```
- Good question: "Find out why op-amp circuits almost always use negative feedback, and where people disagree about it."
- Bad question: "Learn about op-amps."
- `bringBack` is always:
  - claims (with sources if they have them);
  - open questions;
  - anything surprising;
  - practical tips.

The learner sees the card in the flow. It offers a return form (notes, links, files), **"I'm back"**, and **"Skip hunt"**. There's no deadline; the session waits, and the learner can still message the tutor meanwhile.

### What the learner brings back
**Anything:** notes, links, files, half-remembered tips, claims with or without sources. Requiring sources would filter out exactly the knowledge the learner actually picked up, and learners don't remember which thread a trick came from.
- A **claim without a source** is judged on its content.
- A **claim with a source:** the researcher reads the source. If the claim and source disagree, the tutor treats it as a probable slip or misremembering and asks, rather than marking the claim wrong.
- **Unreadable attachments** (PDFs and images, until page reading exists) are passed to the tutor as "unread attachment; the learner says it's about X".

### The debrief
Before teaching the section, the tutor:
1. reads everything brought back, sending linked pages to the researcher;
2. sorts each claim into **correct**, **partly correct**, **wrong**, **practitioner tip**, or **interesting but off-route**, and records the sort with `debrief_record`;
3. names wrong and partly correct claims plainly, because searching inflates the feeling of knowing;
4. teaches the section using the learner's findings as reference points: "the forum post you found says X; here's why that's right in this case and wrong in that one";
5. parks off-route items with `frontier_park`.

`section_done` never counts anything from a hunt; the exit still needs passing checks after the debrief.

### Practitioner tips: the `heuristic` node kind
Tips like "put a 100 nF capacitor next to every IC's power pin" are useful, true *enough*, and not derivable in the lesson. They become graph nodes of kind **`heuristic`**, with:
- `attribution`: who says it (a community, a source);
- `scope`: when it applies, and when it doesn't;
- the label "rule of thumb, not a law" whenever the tutor presents it.

`heuristic` is a graph kind, **not a teaching mode**. Heuristics are taught inside the surrounding section's mode, with the label. A tip the debrief finds wrong is recorded as a misconception, not a node.

### Frontier list
Off-route topics from hunts and debriefs go to `frontier.jsonl` in the data repo (append-only, one op per item):
```
{ op: "frontier", opId, at, topic, why, from: { sessionId, huntId? } }
```
A learner can start a new session from any frontier item. This is how hunts open the door to new subjects.

### Alternatives considered
| Alternative | For | Against |
|---|---|---|
| Tutor as the only source (today) | Efficient, verified | No breadth, no community, no self-directed search skill |
| Free exploration, no debrief | Maximum autonomy | Loses to guided instruction; misconceptions and inflated confidence stick |
| Hunts as standard for every section | Maximum breadth | Wasted on settled material and on learners who just want to finish |
| Planner turns hunts on itself | Hunts happen where useful | Takes the decision away from the learner; unwanted detours |
| Sources required for every claim | Easy verification | Filters out what the learner actually learned |
| **Opt-in guided hunts, four-condition eligibility, anything-goes return, mandatory debrief, no exit credit** (chosen) | Breadth and curiosity with correction; learner keeps control | Debrief adds a turn; eligibility is the planner's judgement |

### Acceptance criteria
- `goal_set.explore` defaults to `off` for course and exam goals without asking.
- `plan_write` with `hunt.available` while explore is off, or with `hunt.enabled` at all, is refused.
- `hunt_send` is refused unless the current section's hunt is enabled.
- "Skip hunt" works before and during a hunt, and teaching starts normally.
- The return form accepts notes, links and files, none required.
- `debrief_record` is required before the first `question_ask` of a hunted section (the gate refuses teaching questions until the debrief is recorded, or the hunt was skipped).
- Hunt and debrief ops never satisfy any part of the `section_done` gate.
- `frontier_park` appends to `frontier.jsonl`; frontier items can start a session.

---

## 6. Field guide

### What
For each strand, the scope brief records:
- **discourse type**: `settled` / `practitioner-lore` / `active-debate`;
- **venues** where the field lives:
```
venues: [{ name, url?, kind: "ask" | "discussion" | "reference",
           entryLevel: string,   // what the learner must hold for it to be useful, e.g. "op-amp basics"
           note }]
```
At session close the learner gets a **field guide**: the venues useful now, and those coming up with what unlocks them (e.g. "EEVblog forum: useful after op-amps").

### Why
- It answers "where do I ask, and where do people in this field actually talk?", which a tutor alone can't (Lave & Wenger 1991). It also makes hunts possible (§5, condition 3).
- **Asking and reading are different.** A calculus 1 learner can't follow mathematical discussion but *can* ask on Math StackExchange. So `ask` venues are useful at every level, while `discussion` venues unlock as the learner levels up.
- **Purpose-aware.** With `depth: "pass"`, the guide shows only `ask` venues: where to get help, nothing else.

### Where it lives
- **Now:** venues are stored with the session's scope brief (`brief` op) and shown in the close and in the Map page's plan group, under "Where the field lives".
- **When the knowledge graph exists:** venues attach to **topics**, so the guide grows across sessions and is visible in the topic tree.

A venue is "useful now" when its `entryLevel` is covered by the learner's held nodes. Before the graph exists, the tutor judges this from the session.

### Acceptance criteria
- Scope briefs carry `discourse` per strand and a `venues` list, each venue with a kind and an entry level.
- The close lists the venues useful now and those coming up; with `depth: "pass"`, only `ask` venues are listed.
- The Map page shows the guide per plan group.

---

## 7. Calibration feedback

### What
From the confidence the learner already gives on every answer, show them how well their confidence matches their results, in the same words as the confidence buttons:

> When you picked *Fairly sure*, you were right 9 of 15 times.

### Why
- Knowing what you know is the metacognitive part of the revised Bloom taxonomy (Krathwohl 2002), and it matters: overconfident learners end study too early and retain less (Dunlosky & Rawson 2012).
- The data already exists. `question_ask` answers carry confidence in five buckets (10/30/50/70/90, `docs/ui-design.md` §7.2.4) plus a verdict.

### Behavior
- **Computed by code** from ops: per plan (per topic once the graph exists), per confidence bucket, the share of correct answers.
- **Threshold:** shown only once a plan has **20** graded answers with confidence. Within that, report only buckets with **at least 5** answers.
- **What's said:**
  - name a bucket only when accuracy differs from the bucket's value by **15 percentage points or more**;
  - otherwise, one line saying confidence matches results;
  - always observational, never moralizing.
- **Where:** the close (tutor, from `session_state.calibration`), the Map page's plan group, and on request.
- The tutor also uses it while teaching: an overconfident learner gets more checks before sections close; an underconfident one gets told when they were right.

### Acceptance criteria
- `session_state` includes a `calibration` block only when the threshold is met.
- Close text and Map line match the computed numbers.
- No calibration is shown below the thresholds.

---

## 8. Data, tools and gates (summary for implementers)

### Records
| Record | New fields |
|---|---|
| `goal_set` | `purpose` adds `"course"`; `depth: "pass" \| "solid" \| "deep"`; `explore: "off" \| "suggest" \| "on"`; `startingPoint: "measure" \| "quick" \| "new"` |
| `probe_done` strand | `levelReached: "none" \| "explain" \| "use" \| "analyze" \| "design"`; `confidence: "unprobed"` allows `reason: "new" \| "skipped"` |
| `question_ask` | `level: "explain" \| "use" \| "analyze" \| "design"` (required for `probe` and `check`) |
| Plan section | `targetLevel`; `assumedKnown?: boolean`; `knownNote?: string`; `hunt?: { available: boolean, reason, enabled: boolean }`; `learnerNote?: string` |
| Plan (whole) | `depth`, `explore`, `minutesPerSitting`, `style` (mirrors of the learner's controls) |
| Scope brief | strands gain `discourse`; top-level `venues[]` |
| Graph node `kind` (when the graph exists) | adds `heuristic` with `attribution` and `scope`; provenance `source.type` adds `hunt` (with `unverified: true` until the debrief marks the claim correct) |
| `Outcome` (when practice exists) | `level` |
| Device settings | `startingPoint` default |

### Session ops (new)
| Op | Written when |
|---|---|
| `planEdit { edits, comment }` | The learner sends plan changes |
| `planAsk { question, options }` / `planAnswer { answer }` | The planner asks on the card / the learner answers |
| `knownRefuted { sectionId, questionId }` | A known section's check is missed |
| `huntSend` / `huntReturn { notes, links, files }` / `huntSkip` | Hunt lifecycle |
| `debrief { huntId, claims[] }` | `debrief_record` |

The frontier lives in `frontier.jsonl`, not in a session.

### Tools (new)
| Tool | Agent / phase | Arguments |
|---|---|---|
| `plan_ask` | planner | `{ question, options[] }`; pauses the planner until answered |
| `hunt_send` | tutor: teach | see §5 |
| `debrief_record` | tutor: teach | `{ huntId, claims: [{ claim, source?, verdict: "correct" \| "partial" \| "wrong" \| "tip" \| "offroute", note, attribution?, scope? }] }` |
| `frontier_park` | tutor: all phases | `{ topic, why }` |

### Learner commands (new)
| Command | Effect |
|---|---|
| `edit_plan(sessionId, edits, comment)` | `planEdit` op + planner job |
| `answer_plan_question(sessionId, answer)` | `planAnswer` op, resumes the planner |
| `hunt_return(sessionId, huntId, payload)` / `hunt_skip(sessionId, huntId)` | Ops, then wakes the tutor |
| `start_from_frontier(itemId)` | A new Learn session seeded with the item |

### Gates (additions to `docs/architecture.md` §4.10)
1. `section_done`: also needs ≥ 1 passing check in the section at `level ≥ targetLevel`.
2. `section_done` for `assumedKnown` sections: one passing check at `≥ targetLevel` is enough; after `knownRefuted`, rule 1 and the normal per-node rule apply.
3. `probe_done`: zero questions allowed when `startingPoint: "new"`; at most five when `"quick"`. It still needs the brief.
4. `plan_write`: refused if any section sets `hunt.enabled`, or `hunt.available` while explore is `off`.
5. `hunt_send`: refused unless the current section's hunt is enabled.
6. Teach-phase `question_ask` in a hunted section: refused until a `debrief` op exists for that hunt, or the hunt was skipped.
7. Nothing from a hunt or debrief counts toward any `section_done` condition.

---

## 9. Files changed by this design

**Prompts** (`crates/harness/prompts/`):
- `tutor/base.md`;
- `tutor/phases/measure.md`;
- `tutor/phases/teach.md`;
- **new** `tutor/hunt.md`;
- `planner/base.md`;
- `planner/exam.md`;
- `jobs/researcher.md`;
- `shared/questions.md`;
- `shared/learner-signals.md`;
- `coach/base.md`;
- `jobs/graph-builder.md`;
- `README.md` (tools, composition, flags).

**Docs:**
- this file;
- `docs/architecture.md` (requirements R14–R17, pointer, vocabulary);
- `docs/ui-design.md` (plan card "⋯" controls, known sections, planner question, hunt card, field guide, Map level and calibration lines, Settings default);
- `docs/agent-system-plan.md` (not-built list).

**Composer wiring:**
- With `hunt_active`, `{{hunt}}` in `teach.md` is the body of `tutor/hunt.md`, rendered with `{{hunt.state}}` (`ready` / `out` / `returned`). Without the flag the block, and its placeholder, are dropped.
- With `known_section`, `{{section.knownNote}}` is the learner's comment. With `levels`, `{{section.targetLevel}}` is shown; with `plan_controls`, `{{section.learnerNote}}` is shown (empty string when the learner left none).
- New `session_state` fields are rendered when their flag is on: depth, explore, starting point, target levels, known sections, the hunt state, venues, calibration.
- **Required test:** compose every agent × phase × mode with all flags off (no error; output equals today's apart from one unflagged wording fix in `teach.md`, where checks are no longer described as hidden behind a button, matching the current UI) and with all flags on (no leftover `{{`, no missing placeholder).

---

## 10. Review checklist (for the verifying agent)

Check each item against code and ops, not against UI screenshots alone.

**Levels**
- [ ] `levels` flag on; `targetLevel` on every section; `level` on every probe and check question.
- [ ] `section_done` refused without a passing check at target level; the error names the level.
- [ ] Highest level per node derived from ops, not stored; Map shows "Reached · Aim".
- [ ] No level stored on graph nodes or edges.

**Starting point**
- [ ] `new` → zero probe questions, `probe_done` succeeds after the brief; `quick` → ≤ 5 probe questions.
- [ ] Default persisted in settings and used on the next session.

**Plan controls**
- [ ] "⋯" hides all controls by default; edits batch; one `planEdit` per send; Approve disabled while edits are pending.
- [ ] "Parts I already know" → split, greyed known section; one check closes it; a miss writes `knownRefuted`, no planner job, normal gate after.
- [ ] `plan_ask` renders on the card and resumes the planner.
- [ ] Explicit section settings survive later depth/style changes.

**Hunts**
- [ ] Explore defaults off, unasked, for course and exam goals.
- [ ] Planner can't enable hunts; can't mark them available with explore off.
- [ ] Hunt card has question, where to look, what to bring back; skip works before and during.
- [ ] Return accepts notes, links, files, nothing required.
- [ ] Debrief recorded before teaching questions; claims sorted into the five verdicts; off-route items in `frontier.jsonl`.
- [ ] No hunt or debrief op counts toward `section_done`.

**Field guide**
- [ ] Brief has `discourse` and `venues`; the close shows now/later venues; `pass` shows only `ask` venues; the Map shows the guide.

**Calibration**
- [ ] Hidden below 20 graded answers per plan and below 5 per bucket; ±15-point rule; numbers match the ops.

**General**
- [ ] Each flag is turned on in the same change as its code; with all flags off, prompts compose exactly as before.
- [ ] Every new tool name and field matches `crates/harness/prompts/README.md`.

---

## 11. Risks and open questions

1. **Level tagging reliability:** whether models tag checks consistently at four levels is untested **[inference]**. Mitigation: log the levels, and spot-check a sample of sessions.
2. **Hunt eligibility** is the planner's judgement; it may suggest hunts on settled material. Mitigation: the learner decides anyway, and suggestions are measured (shown vs. enabled vs. skipped).
3. **Debrief length:** a big hunt return can make the debrief turn long. The tutor should debrief only claims that bear on the section, and park the rest.
4. **Venue quality and currency:** forums and Discords move. Venues carry the date the brief found them and are re-checked by research when shown months later.
5. **Calibration on small samples:** thresholds keep noise down but don't remove it; the wording stays observational.

---

## 12. Sources

All DOIs below were checked against Crossref on 2026-11-06, except the book.

Levels and depth
- Krathwohl, D. R. (2002). A revision of Bloom's taxonomy: An overview. *Theory Into Practice*, 41(4). https://doi.org/10.1207/s15430421tip4104_2
- Bloom, B. S. (1984). The 2 sigma problem: The search for methods of group instruction as effective as one-to-one tutoring. *Educational Researcher*, 13(6). https://doi.org/10.3102/0013189X013006004
- Agarwal, P. K. (2019). Retrieval practice & Bloom's taxonomy: Do students need fact knowledge before higher order learning? *Journal of Educational Psychology*, 111(2). https://doi.org/10.1037/edu0000282
- Lemons, P. P., & Lemons, J. D. (2013). Questions for assessing higher-order cognitive skills: It's not just Bloom's. *CBE—Life Sciences Education*, 12(1). https://doi.org/10.1187/cbe.12-03-0024

Starting point
- Richland, L. E., Kornell, N., & Kao, L. S. (2009). The pretesting effect: Do unsuccessful retrieval attempts enhance learning? *Journal of Experimental Psychology: Applied*, 15(3). https://doi.org/10.1037/a0016496
- Pan & Carpenter (2023), prequestions: cited in `docs/ui-design.md` §2.6.
- Kalyuga, S., Ayres, P., Chandler, P., & Sweller, J. (2003). The expertise reversal effect. *Educational Psychologist*, 38(1). https://doi.org/10.1207/S15326985EP3801_4

Learner control
- Kirschner, P. A., & van Merriënboer, J. J. G. (2013). Do learners really know best? Urban legends in education. *Educational Psychologist*, 48(3). https://doi.org/10.1080/00461520.2013.804395
- Patall, E. A., Cooper, H., & Robinson, J. C. (2008). The effects of choice on intrinsic motivation and related outcomes: A meta-analysis. *Psychological Bulletin*, 134(2). https://doi.org/10.1037/0033-2909.134.2.270

Hunts, exploration, community
- Schwartz, D. L., & Bransford, J. D. (1998). A time for telling. *Cognition and Instruction*, 16(4). https://doi.org/10.1207/s1532690xci1604_4
- Schwartz, D. L., & Martin, T. (2004). Inventing to prepare for future learning. *Cognition and Instruction*, 22(2). https://doi.org/10.1207/s1532690xci2202_1
- Sinha, T., & Kapur, M. (2021). When problem solving followed by instruction works: Evidence for productive failure. *Review of Educational Research*, 91(5). https://doi.org/10.3102/00346543211019105
- Loibl, K., Roll, I., & Rummel, N. (2017). Towards a theory of when and how problem solving followed by instruction supports learning. *Educational Psychology Review*, 29. https://doi.org/10.1007/s10648-016-9379-x
- Alfieri, L., Brooks, P. J., Aldrich, N. J., & Tenenbaum, H. R. (2011). Does discovery-based instruction enhance learning? *Journal of Educational Psychology*, 103(1). https://doi.org/10.1037/a0021017
- Kirschner, P. A., Sweller, J., & Clark, R. E. (2006). Why minimal guidance during instruction does not work. *Educational Psychologist*, 41(2). https://doi.org/10.1207/s15326985ep4102_1
- Gruber, M. J., Gelman, B. D., & Ranganath, C. (2014). States of curiosity modulate hippocampus-dependent learning via the dopaminergic circuit. *Neuron*, 84(2). https://doi.org/10.1016/j.neuron.2014.08.060
- Vakkari, P. (2016). Searching as learning: A systematization based on literature. *Journal of Information Science*, 42(1). https://doi.org/10.1177/0165551515615833
- Lave, J., & Wenger, E. (1991). *Situated Learning: Legitimate Peripheral Participation.* Cambridge University Press. (Book; no DOI.)

Evidence over feeling, calibration
- Fisher, M., Goddu, M. K., & Keil, F. C. (2015). Searching for explanations: How the Internet inflates estimates of internal knowledge. *Journal of Experimental Psychology: General*, 144(3). https://doi.org/10.1037/xge0000070
- Dunlosky, J., & Rawson, K. A. (2012). Overconfidence produces underachievement: Inaccurate self evaluations undermine students' learning and retention. *Learning and Instruction*, 22(4). https://doi.org/10.1016/j.learninstruc.2011.08.003
- Deslauriers, L., et al. (2019). Measuring actual learning versus feeling of learning in response to being actively engaged in the classroom. *PNAS*, 116(39). https://doi.org/10.1073/pnas.1821936116
- Rozenblit, L., & Keil, F. (2002). The misunderstood limits of folk science: An illusion of explanatory depth. *Cognitive Science*, 26(5). https://doi.org/10.1207/s15516709cog2605_1
