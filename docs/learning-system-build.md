# Learning-system build: interfaces and slices

Source of truth for *behaviour*: `docs/learning-system.md` (§2–§7 and the §10 checklist).
Source of truth for *surfaces*: `docs/ui-design.md` §7.7.1–§7.7.7.
This file only fixes the **interfaces** so the slices fit together: names, shapes, gates, flags.
Where this file and the two above disagree about behaviour, they win — say so in a review note
rather than inventing a third behaviour.

## Build order (each slice lands green before the next starts)

1. **levels + starting point + field guide + calibration** (existing records extended, one new derive block, state text).
2. **plan controls + known sections** (new ops, `plan_ask`, learner commands, known-section gates).
3. **hunts + frontier** (new ops, three tools, `frontier.jsonl`, teach gate).
4. **checklist pass**: the §9 flag-matrix compose test, the §10 acceptance tests, docs updated.

Prompts (`crates/harness/prompts/**`) and UI (`ui/**`, `docs/ui-design.md`) are separate slices and
may land in parallel with slice 1: they only code against the interfaces below.

## Records (`crates/harness/src/state.rs`)

```rust
/// How far a section goes (docs/learning-system.md §2). Ordered: Explain < Use < Analyze < Design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level { Explain, Use, Analyze, Design }
impl Level { pub fn as_str(self) -> &'static str; pub fn parse(s: &str) -> Option<Level>; }

/// A hunt the planner marked available in a section (§5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunt {
    pub available: bool,
    #[serde(default)] pub reason: String,
    /// Never set by `plan_write`; derived from the learner's `huntToggle` ops.
    pub enabled: bool,
}
```

`Section` gains (all `#[serde(default)]`, matching the existing `needs_install` style):

| field | type | note |
|---|---|---|
| `target_level` | `Option<Level>` | JSON `targetLevel`; `None` in older sessions |
| `assumed_known` | `bool` | `assumedKnown` |
| `known_note` | `String` | `knownNote`, the learner's comment |
| `learner_note` | `String` | `learnerNote` |
| `hunt` | `Option<Hunt>` | |

`Plan` gains `depth: String`, `explore: String`, `minutes_per_sitting: Option<u32>`, `style: String`
(JSON `depth`, `explore`, `minutesPerSitting`, `style`), all defaulting to empty.

`CheckVerdict` gains `level: Option<Level>` (filled from the graded question's `question_ask` args).

New derived values in `SessionState`:

```rust
/// nodeId -> highest level with a passing check (derived, never stored).
pub node_levels: BTreeMap<String, Level>,
/// Sections whose assumed-known check was missed.
pub known_refuted: Vec<String>,
/// The current section's hunt, from `huntSend`/`huntReturn`/`huntSkip`/`debrief`.
pub hunt: Option<HuntState>,
/// Every hunt the tutor sent, in op order, each with its own lifecycle: the
/// flow resolves a card by `huntId`, so an earlier section's card keeps its
/// own finished state. `hunt` is the current section's entry.
pub hunts: Vec<HuntState>,
/// The planner's open question on the plan card (`planAsk` with no `planAnswer`).
pub pending_plan_question: Option<PlanQuestion>,
/// Calibration block, present only when the thresholds are met (§7).
pub calibration: Option<Calibration>,
```

```rust
pub struct HuntState {
    pub hunt_id: String, pub section_id: String, pub question: String,
    pub look_at: serde_json::Value, pub bring_back: Vec<String>,
    /// ready | out | returned | skipped
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub returned: Option<serde_json::Value>,
    #[serde(default)] pub debriefed: bool,
}
pub struct PlanQuestion { pub call_id: String, pub question: String, pub options: Vec<String> }
pub struct Calibration { pub graded: u32, pub buckets: Vec<CalibrationBucket>, pub notes: Vec<String> }
pub struct CalibrationBucket { pub confidence: u32, pub label: String, pub correct: u32, pub total: u32 }
```

## Ops (`crates/harness/src/sessions.rs`)

Add variants to `Op` (tag = the camelCase name) and to `at()`/`op_id()`:

| variant | tag | payload |
|---|---|---|
| `PlanEdit` | `planEdit` | `{ op_id, at, edits: Value, comment: Option<String> }` |
| `PlanAsk` | `planAsk` | `{ op_id, at, call_id, question, options: Vec<String> }` |
| `PlanAnswer` | `planAnswer` | `{ op_id, at, call_id, answer }` |
| `KnownRefuted` | `knownRefuted` | `{ op_id, at, section_id, question_id }` |
| `HuntToggle` | `huntToggle` | `{ op_id, at, section_id, enabled: bool }` |
| `HuntSend` | `huntSend` | `{ op_id, at, hunt_id, section_id, question, look_at: Value, bring_back: Vec<String> }` |
| `HuntReturn` | `huntReturn` | `{ op_id, at, hunt_id, notes, links: Vec<String>, files: Vec<String> }` |
| `HuntSkip` | `huntSkip` | `{ op_id, at, hunt_id }` |
| `Debrief` | `debrief` | `{ op_id, at, hunt_id, claims: Value }` |

`hunt_id` is always the `callId` of the `hunt_send` tool call (one conversation-visible id, no second minting).

## Tools (`crates/harness/src/tools.rs`)

Existing tools gain arguments; three tools are new.

| tool | agent | args added / shape |
|---|---|---|
| `goal_set` | tutor (measure) | `purpose` adds `"course"`; `depth: "pass"\|"solid"\|"deep"` (optional, default `solid`); `explore: "off"\|"suggest"\|"on"` (optional, default `off`; forced `off` for `course`/`exam`); `startingPoint: "measure"\|"quick"\|"new"` (optional, falls back to the device setting, else `measure`) |
| `plan_write` | planner | plan-level `depth`, `explore`, `minutesPerSitting`, `style`; section `targetLevel`, `assumedKnown`, `knownNote`, `learnerNote`, `hunt: { available, reason, enabled }` |
| `question_ask` | tutor | `level: explain\|use\|analyze\|design`, in the schema's `required` list in both phases; the gate refuses a `probe` or `check` without one |
| `probe_done` | tutor | strand `levelReached`; `confidence: "unprobed"` allows `reason: "new"\|"skipped"` |
| `plan_ask` | planner (new) | `{ question, options[] }` — one question on the plan card, then the run ends |
| `hunt_send` | tutor: teach (new) | `{ sectionId, question, lookAt: [{ kind, venue?, why }], bringBack: [string] }`, `kind` ∈ `forum\|datasheet\|lecture\|video\|paper\|docs\|person` |
| `debrief_record` | tutor: teach (new) | `{ huntId, claims: [{ claim, source?, verdict, note, attribution?, scope? }] }`, `verdict` ∈ `correct\|partial\|wrong\|tip\|offroute` |
| `frontier_park` | tutor: any phase (new) | `{ topic, why }` |

`validate_plan` (`tools::check_plan`) adds: unknown `targetLevel` refused; `hunt.enabled == true` always refused
("only the learner can enable a hunt"); `hunt.available` refused when the plan's `explore` is `off`, and refused
on a goal whose `purpose` is `course` or `exam` whatever the plan's `explore` says (`goal_set` forces explore off
for those goals; the refusal names the purpose).

`section_done` gate additions (in `agent::section_done`), refusing with the level named:
1. every node passed (unchanged) **unless** the section is `assumed_known`;
2. at least one passing check in the section at `level >= targetLevel` (skipped when `target_level` is `None`,
   which only older sessions have);
3. `assumed_known` and not refuted: one passing check in the section at `>= targetLevel` is enough;
   after a `knownRefuted`, rule 1 applies too.

`probe_done` gate additions:
- `startingPoint: "new"` → no probe questions may exist (`gate_question` refuses a probe question with
  "the learner starts new: no probe questions; their first checks are the measurement"), and `probe_done`
  accepts any strand whose `reason` is `new` (its `confidence` must be `unprobed`);
- `startingPoint: "quick"` → the sixth probe `question_ask` is refused ("quick check: at most five probe questions").

Teach gate (`gate_question`, phase Teach, roles `predict`/`check`): refused while the current section's hunt is
`enabled`, not skipped, and not `debriefed` — "the hunt's debrief hasn't been recorded yet; call debrief_record first".

`hunt_send` gate: refused unless the current section's hunt exists, is `available` and `enabled`, is not skipped,
and no `huntSend` exists for that section yet. `debrief_record` refused unless a `huntReturn` exists for that
`huntId`. `frontier_park` appends `{ "op": "frontier", "opId", "at", "topic", "why", "from": { "sessionId", "huntId"? } }`
to `<repo>/frontier.jsonl` (append-only JSONL, created on first use).

## Commands (`Harness` methods + Tauri commands in `crates/app/src/lib.rs`)

| Harness method | command args | effect |
|---|---|---|
| `edit_plan(&self, session_id, edits: Value, comment: Option<&str>)` | `{ sessionId, edits, comment }` | one `planEdit` op, then one planner job whose request text is the edits + comment |
| `answer_plan_question(&self, session_id, answer: &str)` | `{ sessionId, answer }` | one `planAnswer` op, then one planner job carrying the question and the answer |
| `hunt_return(&self, session_id, hunt_id, payload: Value)` | `{ sessionId, huntId, payload }` | `huntReturn` op (`notes`/`links`/`files` read from `payload`, all optional), then wake the tutor |
| `hunt_skip(&self, session_id, hunt_id)` | `{ sessionId, huntId }` | `huntSkip` op, then wake the tutor |
| `set_hunt(&self, session_id, section_id, enabled: bool)` | `{ sessionId, sectionId, enabled }` | `huntToggle` op (no planner job, no turn) |
| `frontier(&self) -> Result<Vec<FrontierItem>>` | — | reads `frontier.jsonl` |
| `start_from_frontier(&self, item_id: &str)` | `{ itemId }` | new Learn session whose first learner message is the item's topic and why |

`plan_ask` mechanics: the planner tool appends `PlanAsk` and returns `(done(...), Some(None))`, which ends the
planner run without a delivered notice. `answer_plan_question` starts a fresh planner run (the planner always
reads the whole session), so no job-resume state is stored.

Wake after a hunt return: same path as job delivery — append a learner-side tagged message
(`hunt-return`) and run a tutor turn if the phase is Teach.

## Prompt flags and placeholders (`crates/harness/src/prompts.rs`, `crates/harness/src/agent.rs`)

Flags set in `compose_tutor`/`compose_planner` (all eight; each on when the data the block needs can exist):

`levels`, `starting_point`, `plan_controls`, `known_section` (per compose: `plan_controls` on **and** the current
section is `assumed_known`), `hunts`, `hunt_active` (per compose: `hunts` on **and** the current section's hunt is
`available && enabled` and not skipped), `field_guide`, `calibration`.

New values: `section.targetLevel`, `section.knownNote`, `section.learnerNote`, `hunt` (rendered
`tutor/hunt.md`), `hunt.state` (`ready`/`out`/`returned`/`skipped`). All are always set (a value the flag drops is
never read); `section.knownNote`/`learnerNote` are empty strings when the learner wrote none, `section.targetLevel`
is `(none)` when absent.

`session_state` (`Composer::session_state`) additions, data-driven so an older log renders exactly as today:

```
## Goal
…
## Field brief
…                                   (venues are part of the brief JSON)
## Probe map
- {strand json, including levelReached}
## Plan (v2)
approach
- ▶ s1 [mechanism] nodes: n1, n2 · aim: analyze · known · hunt: out
## Field guide                        (only when brief.venues is non-empty)
- ask  Math StackExchange — useful now; entry: limits
- discussion  r/math — useful once you hold: proof writing
## Platform
…
## Learner state
…
## Calibration                         (only when state.calibration is some)
- Fairly sure: 9/15 correct (60%)      (buckets with ≥5 answers)
Confidence matches results.            (the `notes` sentences, verbatim)
```

Calibration wording is produced by code (`state::Calibration::notes` logic in `replay.rs`), and the prompts say
"use `session_state.calibration` verbatim": labels are `Guessing`/`Unsure`/`Even odds`/`Fairly sure`/`Certain`
(10/30/50/70/90), the 20-answer and 5-per-bucket thresholds and the ±15-point rule are code's.

## Decisions where the spec left a mechanism open

| Question | Chosen | Why |
|---|---|---|
| How the learner enables a hunt, when `plan_write` may never set `hunt.enabled` | A `huntToggle` op from a learner command; replay overrides `Hunt.enabled`. | The spec says only the learner may enable; a learner-only op keeps `plan_write` pure and the toggle instant (no planner round trip). |
| `plan_ask` pause | The planner tool ends the run; the answer starts a new run. | The planner reads the whole session anyway; persisted resume state would be a second source of truth. |
| Where learner edits land | One `planEdit` op, then the ordinary planner job (`plan_change_request` path). | Reuses the existing revision → approval rules instead of a second edit path. |
| Known-section miss | Code appends `knownRefuted` inside `verdict_record`. | "code records it" in §4; no planner call. |
| Calibration scope | The whole session (one plan). | The graph does not exist yet; §7 says "per plan (per topic once the graph exists)". |
| Which answers calibration counts | Every graded `question_ask` answer (`probe`, `check`, `predict`) with a confidence — probes have no node, so they reach calibration without entering `checks`/`node_levels`. | §7 counts "the confidence the learner already gives on every answer"; `node_passed` and the section gates must only ever see a real node. |
| Level on `CheckVerdict` | Copied from the graded question's args at replay. | The verdict tool stays unchanged; replay has both ops. |

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets`.
- `crates/harness/tests/learning_e2e.rs` (scripted provider): plan edit → one `planEdit` → one planner job,
  the planner's question → answer → revision, known section pass and miss (with no planner job on the miss),
  hunt send → return → debrief → teach gate and the debrief's refusal of `section_done`, hunt skip,
  a hunt skipped in section 1 that stays finished while section 2's runs, a course goal whose plan can't
  reintroduce a hunt, `plan_write`'s hunt refusals, frontier park and start-from-frontier, and the
  starting-point `new` path (no probe question, `probe_done` after the brief).
- Unit tests hold the rest: `agent.rs` for the level and known-section gates and the `new`/`quick` probe limits;
  `replay.rs` for derived node levels, the calibration thresholds and probe answers counting, and each
  hunt's own state across sections; `tools.rs` for the schema's required `level` and the course/exam hunt
  refusal; `prompts.rs` for the §9 compose matrix
  (`compose_every_agent_phase_and_mode_with_every_flag_off` / `_with_every_flag_on`), the field guide and
  calibration block in `session_state`, and the plan's depth/explore/sitting/style line;
  `config.rs` and `runtime_e2e.rs` for the persisted starting point and the course/exam `explore` forcing.
- `scripts/ui-check/run.sh` still passes (the IPC probe and artifact placement); the learning-system surfaces
  were driven in headless Chromium against a mocked bridge — plan card "⋯" controls and one flat `edit_plan`,
  known/refuted sections, the planner's question, the hunt card (return and skip), the close's guide and
  calibration (including `pass` depth and below-threshold), the Map's level, guide, confidence and frontier
  lines, and Settings → Starting point.
