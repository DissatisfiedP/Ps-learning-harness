// Folds a Learn session's op log into the ordered list of things the
// conversation shows. Items are updated in place (a question block gets its
// answer and verdict, a job line its state and brief), so every op yields at
// most one added or updated item.
//
// Item kinds:
//   user / assistant  { msg }
//   notice            { text, error? }               plan-notice messages
//   job               { jobId, jobKind, state, question, error, result, brief }
//   question          { callId, args, answer, failed, verdict, note }
//   choice            { callId, args, answer, failed }
//   divider           { text }                       compaction
//   section           { sectionId, close, plan }     UI-only, from `phase` ops
//   artifacts         { ops }                        figures with no reply block
//
// `tl.plan` is the latest `plan` op payload; section openers take their
// title and goal from it. `tl.lastExercise` is the newest question or choice
// item, so the desk can ask "is one pending?" without scanning the list.
// `tl.artifacts` is every artifact op by id; `pendingArtifacts` are those
// created since the last visible tutor message (they attach to it, or flush
// as a figures block before the next learner message).

const RESEARCH_PREFIX = /^\[research brief\]\s*/;
const NOTICE_PREFIX = /^\[plan update\]\s*/;

export function createTimeline() {
  return {
    items: [],
    byKey: new Map(),
    seen: new Set(),
    plan: null,
    lastSection: null,
    lastExercise: null,
    artifacts: new Map(),
    pendingArtifacts: [],
  };
}

function add(tl, item) {
  tl.items.push(item);
  tl.byKey.set(item.key, item);
  return { added: item };
}

function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

function jobItem(tl, jobId) {
  return tl.byKey.get(`job:${jobId}`);
}

/**
 * Applies one op. Returns `{ added }`, `{ updated }`, or null when the op has
 * no visible effect (or was already applied).
 */
export function applyOp(tl, op) {
  if (op.opId) {
    if (tl.seen.has(op.opId)) return null;
    tl.seen.add(op.opId);
  }
  switch (op.op) {
    case 'message':
      return applyMessage(tl, op);
    case 'tool':
      return applyTool(tl, op);
    case 'job': {
      const existing = jobItem(tl, op.jobId);
      if (existing) {
        existing.jobKind = op.kind ?? existing.jobKind;
        existing.state = op.state ?? existing.state;
        existing.question = op.question ?? existing.question;
        existing.error = op.error ?? existing.error;
        existing.result = op.result ?? existing.result;
        return { updated: existing };
      }
      return add(tl, {
        kind: 'job',
        key: `job:${op.jobId}`,
        jobId: op.jobId,
        jobKind: op.kind,
        state: op.state,
        question: op.question,
        error: op.error,
        result: op.result,
        brief: null,
      });
    }
    case 'compaction':
      return add(tl, { kind: 'divider', key: op.opId, text: 'Earlier turns summarized' });
    case 'plan':
      if (op.payload) tl.plan = op.payload;
      return null;
    case 'phase':
      return applyPhase(tl, op);
    case 'artifact':
      if (op.artifactId) {
        tl.artifacts.set(op.artifactId, op);
        tl.pendingArtifacts.push(op);
      }
      return null;
    default:
      return null;
  }
}

/** A section opener for `phase: teach` (with a section) and `phase: close`. */
function applyPhase(tl, op) {
  const close = op.phase === 'close';
  if (!close && !(op.phase === 'teach' && op.sectionId)) return null;
  const id = close ? '\u0000close' : op.sectionId;
  if (tl.lastSection === id) return null;
  tl.lastSection = id;
  return add(tl, { kind: 'section', key: op.opId ?? `section:${id}`, sectionId: close ? null : op.sectionId, close, plan: tl.plan });
}

function applyMessage(tl, op) {
  const content = op.content ?? '';
  if (op.tag === 'research-brief') {
    const brief = content.replace(RESEARCH_PREFIX, '');
    const existing = op.jobId ? jobItem(tl, op.jobId) : null;
    if (existing) {
      existing.brief = brief;
      return { updated: existing };
    }
    return add(tl, {
      kind: 'job',
      key: op.jobId ? `job:${op.jobId}` : op.opId,
      jobId: op.jobId ?? null,
      jobKind: 'research',
      state: 'done',
      question: null,
      brief,
    });
  }
  if (op.tag === 'plan-notice') {
    return add(tl, { kind: 'notice', key: op.opId, text: content.replace(NOTICE_PREFIX, '') });
  }
  if (op.role === 'assistant' && !content.trim()) return null;
  const kind = op.role === 'user' ? 'user' : 'assistant';
  const item = { kind, key: op.opId, msg: op };
  if (kind === 'assistant') {
    // Artifacts created since the last visible tutor message belong to it.
    if (tl.pendingArtifacts.length) item.artifacts = tl.pendingArtifacts.splice(0);
    return add(tl, item);
  }
  // A learner message closes the turn: any artifact the reply never carried
  // becomes its own figures block just before the learner speaks.
  const extras = [];
  if (tl.pendingArtifacts.length) {
    const ops = tl.pendingArtifacts.splice(0);
    extras.push(add(tl, { kind: 'artifacts', key: `artifacts:${ops[0].opId}`, ops }).added);
  }
  const { added } = add(tl, item);
  return extras.length ? { added, extras } : { added };
}

function applyTool(tl, op) {
  // Only the tutor's tool calls are shown; the planner's are internal.
  if (op.agent && op.agent !== 'tutor') return null;
  const key = `call:${op.callId}`;
  if (op.result) {
    const item = tl.byKey.get(key);
    if (!item) return null;
    if (op.result.isError) {
      item.failed = op.result.content || 'The question could not be asked.';
    } else {
      item.answer = parseJson(op.result.content) ?? { answer: op.result.content };
      if (item.answer.verdict && !item.verdict) item.verdict = item.answer.verdict;
    }
    return { updated: item };
  }
  const args = op.args ?? {};
  if (op.name === 'question_ask' || op.name === 'choice_ask') {
    const item = op.name === 'question_ask'
      ? { kind: 'question', key, callId: op.callId, args, answer: null, failed: null, verdict: null, note: null }
      : { kind: 'choice', key, callId: op.callId, args, answer: null, failed: null };
    tl.lastExercise = item;
    return add(tl, item);
  }
  if (op.name === 'verdict_record') {
    const item = tl.byKey.get(`call:${args.questionId}`);
    if (!item) return null;
    item.verdict = args.verdict ?? item.verdict;
    item.note = args.note ?? item.note;
    return { updated: item };
  }
  if (op.name === 'hunt_send') {
    // The hunt card sits in the flow where the tutor sent it. Its lifecycle
    // (`out` → `returned`/`skipped`) comes from `session_state`, not from here:
    // the hunt id is this call's id.
    return add(tl, {
      kind: 'hunt',
      key: `hunt:${op.callId}`,
      huntId: op.callId,
      sectionId: args.sectionId ?? null,
      args,
      hunt: null,
    });
  }
  return null;
}

/**
 * Brings one hunt card's lifecycle up to date from `session_state`. Returns the
 * item to re-render when something changed, null otherwise; a hunt whose
 * `hunt_send` op was never seen has no card (older logs, partial state).
 */
function applyHuntState(tl, hunt) {
  if (!hunt?.huntId) return null;
  const item = tl.byKey.get(`hunt:${hunt.huntId}`);
  if (!item) return null;
  const before = JSON.stringify(item.hunt);
  item.hunt = hunt;
  if (before === JSON.stringify(hunt)) return null;
  return { updated: item };
}

/**
 * Brings every hunt card up to date from `session_state.hunts`: one card per
 * `hunt_send` op, resolved by hunt id, so a card from an earlier section keeps
 * its own finished state once teaching has moved on (a single "current hunt"
 * would leave it looking live). Returns the items to re-render.
 */
export function applyHuntStates(tl, hunts) {
  const updated = [];
  for (const hunt of hunts ?? []) {
    const change = applyHuntState(tl, hunt);
    if (change?.updated) updated.push(change.updated);
  }
  return updated;
}

/** Ops after which the running head, desk and plan must be refreshed. */
export function affectsState(op) {
  return (
    op.op === 'phase' ||
    op.op === 'plan' ||
    op.op === 'approve' ||
    op.op === 'pluginGrant' ||
    op.op === 'tool' ||
    // Learning-system ops (docs/learning-system-build.md): each changes what
    // the plan card, the desk or the close shows.
    op.op === 'planEdit' ||
    op.op === 'planAsk' ||
    op.op === 'planAnswer' ||
    op.op === 'knownRefuted' ||
    op.op === 'huntToggle' ||
    op.op === 'huntSend' ||
    op.op === 'huntReturn' ||
    op.op === 'huntSkip' ||
    op.op === 'debrief'
  );
}

/** Whether a question/choice item still waits for the learner. */
export function isPending(item) {
  return (item.kind === 'question' || item.kind === 'choice') && !item.answer && !item.failed;
}
