# Graph core build: interfaces

Status: **built 2026-11-07.** What shipped, the deviations and what is unproven are recorded in
`docs/agent-system-plan.md` ("Decisions in the graph, sync, plan-card and CI build", and known gap 14).
The one addition to the record shapes below: a `node` op also carries an optional `title` (the read model and the
porting binary need it, and the migration keeps the legacy titles).

Source of truth for behaviour: `docs/architecture.md` §4.2, §4.5, §4.6, §4.7 and `docs/agent-system-plan.md`
"Not built yet" item 1. This file fixes only the interfaces so the work is checkable and so sync can rely on the file layout.

## What is already there (do not re-invent)

- `crates/harness/src/knowledge.rs` reads `<repo>/graph/{topics.json,nodes.jsonl,edges.jsonl,checks.jsonl}`
  into `KnowledgeMap`; edges carry `relation` (`requires` | `contrasts`) and only `requires` orders anything.
- `crates/harness/src/proposals.rs` is the reviewed-edge path (`edges.proposed.jsonl`, accept/dismiss, cycle checks) and
  `crates/harness/src/bin/extract_edges.rs` writes proposals.
- `crates/harness/src/bin/import_learnharness.rs`, `crates/harness/src/bin/port_learnharness_session.rs` write/read the store.
- The Map page (`ui/map.js`) draws the read model; `knowledge_map` is the only command.
- A graph-builder prompt exists at `crates/harness/prompts/jobs/graph-builder.md` but nothing dispatches it.
- Session ops are the evidence: `replay::transcript(ops)` gives the builder its input; `replay_state` gives the plan and checks.

## Store becomes an op log

`graph/ops.jsonl` — append-only, one JSON object per line, every op carrying `op`, `opId` (ULID) and `at` (ISO-8601).
Last-writer-wins per subject when folding, ordered by `(at, opId)`. Op kinds:

| `op` | payload | meaning |
|---|---|---|
| `topic` | `topicId, title, parent` | upsert a topic (id stable, title may be renamed) |
| `node` | `nodeId, statement, kind, home, provenance` | upsert a proposition. `kind` ∈ mechanism, fact, procedure, perceptual, argument, **heuristic**; a `heuristic` also carries `attribution` and `scope` |
| `place` | `nodeId, home, by` | move a node. `by: "learner"` always wins over any later `by: "builder"` |
| `edge` | `from, to, relation, weight, why, provenance` | `relation` ∈ requires, contrasts, supports, attacks; `weight` ∈ `load-bearing` \| `trivial` (meaningful for requires) |
| `sameAs` | `a, b` | the two ids are the same proposition: fold into the earlier one, remap its edges, keep the folded id as an alias |
| `chain` | `chainId, premises[], conclusion, statement` | a multi-premise argument: the conclusion follows from these premises together |
| `tombstone` | `nodeId, why` | the claim was wrong or merged away; readers must ignore it and its edges |
| `split` | `topic, groups[{ title, nodeIds[] }]` | an accepted topic split: create the child topics and `place` the nodes |
| `venue` | `topicId, kind, name, url?, entryLevel, note, checked?` | where a topic's material lives. `kind` ∈ ask, discussion, reference; deduped by `(topicId, name)` with the last writer winning, so a re-submission corrects the note instead of adding a second row |

`provenance` = `{ "type": "session" | "import" | "hunt" | "learner" | "builder", "sessionId"?, "huntId"?, "at"? }`.
`checks.jsonl` stays a separate evidence file (learner evidence, not graph structure) and keeps its current reader.

**Migration, once:** when `graph/ops.jsonl` is absent and any legacy file exists, synthesize ops from
`topics.json` / `nodes.jsonl` / `edges.jsonl` (ids kept, provenance `{type: "import"}`), write `ops.jsonl`, and never
rewrite the legacy files. After that the op log is the only structure source.

**Cutover:** every writer writes ops — `import_learnharness`, `port_learnharness_session` (writes a session, reads the graph),
`extract_edges` (proposals unchanged; `--accept` goes through `proposals::decide`, which now appends `topic`/`node`/`edge`
ops instead of rewriting `nodes.jsonl`/`edges.jsonl`), and the new builder job. `knowledge::load` folds ops only
(plus the one-time migration), and `proposals`' review file stays as it is.

## Read model additions (`KnowledgeMap`, serialized to the Map)

- `KnowledgeNode`: `provenance: Option<Value>`, `attribution: Option<String>`, `scope: Option<String>`,
  `aliases: Vec<String>` (ids folded into this one).
- `KnowledgeEdge`: `weight: String` (default `load-bearing` for older lines), `why: Option<String>`,
  `provenance: Option<Value>`.
- `chains: Vec<KnowledgeChain>` (`{ chainId, premises, conclusion, statement }`), only chains whose premises and
  conclusion still exist and are not tombstoned (re-validation at load; a dropped chain is logged, never fatal).
- `splits_proposed: Vec<ProposedSplit>` read from `graph/splits.proposed.json` (`{ topic, groups[{ title, nodeIds, why }] }`).
- `venues: Vec<KnowledgeVenue>` (`{ topicId, kind, name, url?, entryLevel, note, checked?, provenance? }`), one row per
  `(topicId, name)` with the last writer's fields, ordered ask → discussion → reference, then by name.
- `topic_venues: Map<topicId, Vec<KnowledgeVenue>>` — each topic's own venues plus its **descendants'**, so a venue on
  `Mathematics › Calculus` is on `Mathematics` too. Present only for topics that have any; same order. This is the
  aggregation the Map draws, computed at load so the UI does not re-derive the tree.
- A venue naming an unknown topic, an empty `name`/`entryLevel`, or a `kind` outside the three is dropped and logged
  at load, like a dangling edge: one bad line must not make the map unreadable.
- A derived index (in memory, rebuilt at load): node by id, children by topic, edges by end, requires adjacency,
  alias → canonical. Nothing derived is written to disk.

## Validation (code owns it; the model gets the problems back and retries)

Refuse, naming every problem at once, in the same style as `plan_write`:
unknown topic/home/parent, unknown node in an edge/chain/split, `kind`/`relation`/`weight` outside the enums,
a `heuristic` without `attribution` or `scope`, a self-edge, an edge whose ends are tombstoned,
a `requires` edge that would close a loop (contrasts/supports/attacks are never cycle-checked),
a duplicate `(from, to, relation)`, a chain with an empty premise list or the conclusion among its premises,
a `split` whose groups do not partition that topic's nodes or that leaves a group empty, a split of a topic with ≤ 30 nodes,
a `venue` whose topic is unknown, whose `kind` is outside the three, whose `name` or `entryLevel` is empty, or that
repeats a `(topicId, name)` already in the same submission.

## Graph builder job

New job kind `"graph"` in `jobs.rs::job_body`. Args `{ sessionId, reason }`. Role **Librarian**
(`Harness::role_vision(Librarian)` for image policy if needed). Tools: `graph_search { query }` (≤ 20 candidate
nodes/topics with ids, titles and home, searched over the current graph) and `graph_submit { topics?, nodes?, edges?,
sameAs?, chains?, splits?, venues? }` (validated as above; a refused submit returns the problems and the run continues).
`venues` are the session's "where the field lives" rows, each attached to the topic whose material it serves; the
builder reads them from the transcript's scope brief (with its per-strand `discourse`) and the code dedupes them by
`(topicId, name)`, so a second session on the same topic refines a row instead of copying it. The session brief itself
is untouched: the close still judges which venue is "useful now" from it (`docs/learning-system.md` §6).

Input to the model: `jobs/graph-builder.md` + `{{graph_summary}}` (topic tree, up to ~200 node ids/titles and the
venues already recorded, capped), + `{{transcript}}` (`replay::transcript`, capped ~24k chars) + `{{plan}}` (the
session's plan as a hint). The builder runs until `graph_submit` succeeds, like the planner.

Triggers: after `session_end` (spawn once per session; the args carry `reason: "close"`), and on demand through
`graph_build(sessionId)`. **It never delivers into the conversation and never wakes the tutor** — the graph is not
conversation material; the job ending emits the ordinary job event so the UI can refresh the Map. A failed run is
`failed` on the job op with the reason; it must not block anything.

## Topic splitter

`graph_splits()` command: for every topic with > 30 nodes, compute connected components of that topic's internal
`requires` graph; when there are ≥ 2 components of ≥ 5 nodes, write `graph/splits.proposed.json` with
`graphs` + placeholder names (the largest node's title, suffixed) and `why` naming the component sizes. The builder may
also submit `splits` with better names. `decide_splits { accept: [topic], dismiss: [...] }` appends `split` ops and
rewrites the proposal file; a dismissed topic is not re-proposed until the graph changes (keep a
`dismissed: [{topic, nodes}]` list in the proposal file and skip when the node count is unchanged).

## Commands (Harness methods + Tauri)

| method | args | effect |
|---|---|---|
| `graph_build(&self, session_id)` | `{ sessionId }` | spawns the `graph` job (refuses with a readable message when one is already running for that session) |
| `graph_splits(&self)` | — | recomputes proposals, returns them |
| `decide_splits(&self, accept, dismiss)` | `{ accept: [topic], dismiss: [topic] }` | appends `split` ops, updates the proposal file, returns what happened |

## Verification

- Unit tests for the fold (upsert, place with learner precedence, `sameAs` with edge remap and alias, tombstone,
  chain re-validation, split application, venue dedupe and ancestor aggregation with its kind-then-name order), the
  one-time migration, and every validation refusal (venues included).
- An e2e with the scripted provider: session → Close → the graph job runs, `graph_submit` is refused once (bad edge)
  then fixed, ops land in `graph/ops.jsonl`, `knowledge_map` shows the node/edge/provenance, the Map read model keeps
  `contrasts` out of the prerequisite view, and turning a session's ops through `graph_build` twice does not duplicate.
  A second e2e submits venues on two topics and asserts `knowledge_map.venues` / `topicVenues` (parent aggregation) and
  that a rebuild with the same `(topicId, name)` updates the row instead of adding one.
- `cargo test --workspace --offline`, `cargo clippy --workspace --all-targets --offline`, and the existing
  `scripts/ui-check/run.sh ipc placement` still pass.
