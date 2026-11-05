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
//
// `tl.plan` is the latest `plan` op payload; section openers take their
// title and goal from it.

const RESEARCH_PREFIX = /^\[research brief\]\s*/;
const NOTICE_PREFIX = /^\[plan update\]\s*/;

export function createTimeline() {
  return { items: [], byKey: new Map(), seen: new Set(), plan: null, lastSection: null };
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
  return add(tl, { kind: op.role === 'user' ? 'user' : 'assistant', key: op.opId, msg: op });
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
  if (op.name === 'question_ask') {
    return add(tl, { kind: 'question', key, callId: op.callId, args, answer: null, failed: null, verdict: null, note: null });
  }
  if (op.name === 'choice_ask') {
    return add(tl, { kind: 'choice', key, callId: op.callId, args, answer: null, failed: null });
  }
  if (op.name === 'verdict_record') {
    const item = tl.byKey.get(`call:${args.questionId}`);
    if (!item) return null;
    item.verdict = args.verdict ?? item.verdict;
    item.note = args.note ?? item.note;
    return { updated: item };
  }
  return null;
}

/** Ops after which the running head, desk and plan must be refreshed. */
export function affectsState(op) {
  return op.op === 'phase' || op.op === 'plan' || op.op === 'approve' || op.op === 'pluginGrant' || op.op === 'tool';
}

/** Whether a question/choice item still waits for the learner. */
export function isPending(item) {
  return (item.kind === 'question' || item.kind === 'choice') && !item.answer && !item.failed;
}
