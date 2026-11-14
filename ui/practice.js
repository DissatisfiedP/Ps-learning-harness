// The Practice session screen (docs/ui-design.md §7.3; the backend contract is
// §11b). Practice is the coach's tool loop: the page is drawn from one read,
// `practice_state`, and never from the raw ops. The coach's text streams only
// once a problem is open (the backend redacts the rest), so nothing here has
// to hide anything: what `PracticeView` does not carry is not on the screen.
//
// Two formats share the screen. A stream poses one problem at a time with
// feedback after each; a set lists every problem, holds the answers locally
// and hands them in together, then shows the review problem by problem.
//
// The module owns its section (#practice): head, page, the answer card and the
// desk. The one shared composer (images and files, as in Learn) is moved into the
// answer card by the host (`composerHost`), exactly as it moves between Home
// and the Learn desk.

import { attachmentList } from './attachment.js';
import { resolveArtifacts } from './artifacts.js';
import { confidenceField, confidenceLabel } from './cards.js';
import { enhanceLive } from './content.js';
import { calibrationBlock } from './fieldguide.js';
import { NUDGE_TEXT, nudgeEl, takeNudge } from './focus.js';
import { renderMarkdown } from './markdown.js';
import { LiveReply, assistantEl } from './reply.js';
import { createExamPaper } from './exam.js';

// ---------- formats (the one place a format is added) ----------

/** The formats the learner can start. Each entry is a radio in every chooser. */
export const FORMATS = [
  { id: 'stream', label: 'One at a time' },
  { id: 'set', label: 'A set of problems' },
];

const FORMAT_KEY = 'lh.practice.format';
const choosers = new Set();

function storedFormat() {
  try {
    const id = localStorage.getItem(FORMAT_KEY);
    if (FORMATS.some((f) => f.id === id)) return id;
  } catch {
    // Storage is optional; the default stands.
  }
  return FORMATS[0].id;
}

let format = storedFormat();

/** The format the learner picked last (shared by every chooser on screen). */
export const practiceFormat = () => format;

function setFormat(id) {
  format = id;
  try {
    localStorage.setItem(FORMAT_KEY, id);
  } catch {
    // Not remembered; the choosers still agree with each other.
  }
  for (const sync of choosers) sync();
}

let chooserSeq = 0;

/** A "format" radio group. Fill `host` with it; it stays in step with every other one. */
export function formatChooser(host) {
  const fs = h('fieldset', 'modes format-choice');
  fs.append(h('legend', 'sr-only', 'Practice format'));
  const name = `practice-format-${++chooserSeq}`;
  const radios = [];
  for (const f of FORMATS) {
    const label = h('label', 'mode');
    const r = document.createElement('input');
    r.type = 'radio';
    r.name = name;
    r.value = f.id;
    r.addEventListener('change', () => r.checked && setFormat(f.id));
    label.append(r, h('span', 'mode-name', f.label));
    fs.append(label);
    radios.push(r);
  }
  const sync = () => radios.forEach((r) => (r.checked = r.value === format));
  choosers.add(sync);
  sync();
  host.append(fs);
  return fs;
}

// ---------- small pieces ----------

function h(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function button(text, className) {
  const b = h('button', `btn ${className}`, text);
  b.type = 'button';
  return b;
}

let uid = 0;
const nextId = (p) => `${p}-${++uid}`;

const WRITING = new Set(['writing', 'set-writing']);
const VERDICTS = {
  correct: ['✓', 'Correct'],
  partial: ['◐', 'Partly right'],
  wrong: ['✗', 'Not quite'],
  dontknow: ['○', 'Don’t know'],
  dismissed: ['–', 'Skipped'],
};
const MISS = {
  cue: 'Didn’t recall it.',
  discrimination: 'Mixed it up with a similar idea.',
  execution: 'Right idea, slip in the working.',
  careless: 'A careless slip.',
  structural: 'The idea itself is off.',
};
const TYPE_LABEL = { critique: 'Critique', design: 'Design' };
const BUSY = {
  writing: 'Writing a problem…',
  'set-writing': 'Writing the set…',
  other: 'Writing…',
  midway: 'The coach is writing. The reply will appear when it’s finished.',
  slow: 'Still working. You can stop it below.',
};
const SLOW_MS = 30000;
const ARM_MS = 4000;
const ALIGN_GAP = 32;
const CONFIDENCE_TAIL = /\n*\[confidence: \d+% — [^\]]*\]\s*$/;

/** Markdown + KaTeX + figures, the way tutor messages render. */
function prose(markdown, ctx, className = 'prose') {
  const div = h('div', className);
  div.innerHTML = renderMarkdown(markdown ?? '');
  resolveArtifacts(div, ctx);
  enhanceLive(div);
  return div;
}

const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** Home's resumption cue for a Practice session: where it stands, or null when it can't go on. */
export function practicePlace(view) {
  if (!view || view.legacy || view.status === 'ended') return null;
  if (view.format === 'set') return view.status === 'set-review' ? 'Practice · set review' : 'Practice · set';
  const n = view.problems.length;
  return n ? `Practice · problem ${n}` : 'Practice';
}

// ---------- drafts (a set's answers are held on this device until handed in) ----------

const draftKey = (id) => `lh.practice.draft.${id}`;

function loadDrafts(id) {
  try {
    const raw = JSON.parse(localStorage.getItem(draftKey(id)) ?? '{}');
    return new Map(Object.entries(raw && typeof raw === 'object' ? raw : {}));
  } catch {
    return new Map();
  }
}

function saveDrafts(id, drafts) {
  try {
    if (drafts.size) localStorage.setItem(draftKey(id), JSON.stringify(Object.fromEntries(drafts)));
    else localStorage.removeItem(draftKey(id));
  } catch {
    // Held in memory only.
  }
}

// ---------- keyed flow ----------

/** Brings `container`'s children to `items` ([{ key, sig, build }]), reusing elements whose signature is unchanged. */
function reconcile(container, items, store) {
  const next = new Map();
  for (const it of items) {
    const old = store.get(it.key);
    next.set(it.key, old && old.sig === it.sig ? old : { sig: it.sig, el: it.build() });
  }
  const keep = new Set([...next.values()].map((n) => n.el));
  for (const node of store.values()) if (!keep.has(node.el)) node.el.remove();
  let i = 0;
  for (const node of next.values()) {
    const at = container.children[i];
    if (at !== node.el) container.insertBefore(node.el, at ?? null);
    i++;
  }
  store.clear();
  for (const [k, v] of next) store.set(k, v);
}

// ---------- the controller ----------

/**
 * @param {object} deps
 * @param {HTMLElement} deps.root           the #practice section
 * @param {(name: string, args?: object) => Promise<any>} deps.cmd   backend call (rejects with text)
 * @param {(text: string) => void} deps.announce
 * @param {object} deps.composerHost        { mount(slot, { label, placeholder, sessionId }), unmount(), reset(), restore(text, attachments, error) }
 * @param {() => void} deps.onBusy          the Send/Stop state changed
 * @param {(patch: { targets: string[], format: string }) => void} deps.start   start another session
 * @param {() => void} deps.onBack
 * @param {(options: object) => void} deps.openSettings
 */
export function createPractice(deps) {
  const { root, cmd, announce, composerHost } = deps;
  const $ = (sel) => root.querySelector(sel);
  const dom = {
    title: $('#practice-title'),
    word: $('#practice-word'),
    status: $('#practice-status'),
    scroll: $('#practice-scroll'),
    picked: $('#practice-picked'),
    flow: $('#practice-flow'),
    live: $('#practice-live'),
    answer: $('#practice-answer'),
    note: $('#practice-note'),
    stop: $('#practice-stop'),
    secondary: $('#practice-secondary'),
    primary: $('#practice-primary'),
  };

  let s = null; // the open session

  const view = () => s?.view ?? null;
  const ctx = () => ({ artifacts: s.artifacts, sessionId: s.id });

  // The exam format's paper (ui/exam.js) lives in #practice-exam-slot.
  const examPaper = createExamPaper({
    slot: $('#practice-exam-slot'),
    cmd,
    announce,
    prose: (markdown, className) => prose(markdown, ctx(), className),
    handedIn: () => s.answers,
    ctx: () => ctx(),
    refresh: () => refresh(),
    onBack: () => deps.onBack(),
  });

  // ----- session lifecycle -----

  function open({ summary, data = null, view: v = null }) {
    close();
    s = {
      id: summary.id,
      title: summary.title ?? '',
      view: null,
      messages: data?.messages ?? [],
      artifacts: new Map(),
      answers: new Map(), // problemId -> { text, confidence } as handed in (from the op log)
      nodes: new Map(),
      drafts: loadDrafts(summary.id),
      settleAfter: 0, // a coach message was persisted: drop the streamed copy once a read at or after this sequence shows it
      pendingEnd: false, // a turn ended and the state read that follows has not been applied yet
      nudges: [],
      seq: 0,
      timer: 0,
      running: false, // between turnStart and turnEnd, as events told it
      midway: false,
      turnError: null,
      live: null,
      busyEl: null,
      busyKey: '',
      slowTimer: 0,
      errorEl: null,
      errorKey: '',
      card: null,
      cardKey: '',
      acting: false,
      armTimer: 0,
    };
    for (const op of data?.ops ?? []) learnOp(op);
    root.hidden = false;
    composerHost.unmount(); // the answer card borrows it when there is something to answer
    dom.flow.textContent = '';
    dom.live.textContent = '';
    dom.answer.textContent = '';
    dom.status.textContent = '';
    dom.title.textContent = s.title;
    if (v) apply(v, { initial: true });
    else {
      render();
      refresh();
    }
  }

  function close() {
    if (!s) return;
    clearTimeout(s.timer);
    clearTimeout(s.slowTimer);
    clearTimeout(s.armTimer);
    s.live?.dispose();
    composerHost.unmount();
    examPaper.destroy();
    root.hidden = true;
    s = null;
    deps.onBusy();
  }

  function setTitle(title, id = null) {
    if (!s || !title || (id && s.id !== id)) return;
    s.title = title;
    dom.title.textContent = title;
  }

  // ----- the op log: only what the view does not carry -----

  function learnOp(op) {
    if (op.op === 'artifact' && op.artifactId) s.artifacts.set(op.artifactId, op);
    else if (op.op === 'practice' && op.event === 'setSubmit') {
      for (const a of op.answers ?? []) s.answers.set(a.problemId, { text: a.text ?? '', confidence: a.confidence ?? null });
    }
  }

  // ----- events (the Learn `AgentEvent`s, agent "coach") -----

  function event(ev) {
    if (!s) return;
    switch (ev.kind) {
      case 'turnStart':
        s.running = true;
        s.turnError = null;
        s.midway = false;
        dom.status.textContent = '';
        renderBusy();
        renderControls();
        deps.onBusy();
        if (WRITING.has(s.view?.status ?? 'writing')) announce(s.view?.status === 'set-writing' ? BUSY['set-writing'] : BUSY.writing);
        break;
      case 'text':
        if (!s.midway) liveReply().addText(ev.delta);
        break;
      case 'thinking':
        if (!s.midway) liveReply().addThinking(ev.delta);
        break;
      case 'op':
        learnOp(ev.op);
        if (ev.op.op === 'message' && ev.op.role === 'assistant' && s.live) s.settleAfter = s.seq + 1;
        if (ev.op.op === 'practice' || ev.op.op === 'message' || ev.op.op === 'artifact') scheduleRefresh();
        break;
      case 'turnEnd':
        s.running = false;
        s.turnError = ev.error ?? null;
        s.pendingEnd = true;
        refresh();
        deps.onBusy();
        break;
    }
  }

  function liveReply() {
    if (!s.live) {
      s.live = new LiveReply({ showThinking: true, ctx: () => ctx() });
      removeBusy();
      dom.live.prepend(s.live.el);
    }
    return s.live;
  }

  function dropLive() {
    if (!s?.live) return;
    s.live.dispose();
    s.live.el.remove();
    s.live = null;
  }

  // ----- reading the state -----

  function scheduleRefresh() {
    clearTimeout(s.timer);
    const mine = s;
    s.timer = setTimeout(() => mine === s && refresh(), 50);
  }

  async function refresh() {
    if (!s) return;
    const mine = s;
    const seq = ++s.seq;
    let v;
    try {
      v = await cmd('practice_state', { sessionId: s.id });
    } catch (e) {
      if (s === mine && seq === s.seq) {
        dom.status.textContent = `Couldn’t read the session: ${e}`;
        if (s.pendingEnd) finishTurn();
      }
      return;
    }
    if (s !== mine || seq !== s.seq) return;
    apply(v, { seq });
  }

  /** The turn is over: its streamed copy goes (the feed has the message). */
  function finishTurn() {
    s.pendingEnd = false;
    dropLive();
    removeBusy();
  }

  function apply(v, { initial = false, seq = 0 } = {}) {
    const prev = s.view;
    s.view = v;
    let turnEnded = false;
    if (s.pendingEnd) {
      if (v.turnRunning) {
        // The turn table has not released yet: read again shortly.
        const mine = s;
        setTimeout(() => mine === s && refresh(), 150);
      } else {
        turnEnded = true;
        finishTurn();
      }
    }
    // A persisted coach message replaces its streamed copy, once the feed carries it.
    if (s.settleAfter && seq >= s.settleAfter) {
      s.settleAfter = 0;
      dropLive();
    }
    // Joined a turn already in progress: its earlier deltas were missed.
    if (v.turnRunning && !s.running && !s.live) s.midway = true;
    else if (!v.turnRunning) s.midway = false;
    render();
    if (initial) resume();
    else transition(prev, v, turnEnded);
    deps.onBusy();
  }

  // ----- render -----

  function render() {
    renderHead();
    renderBody();
    examPaper.update(view(), s.id);
    renderBusy();
    renderError();
    renderAnswer();
    renderControls();
  }

  const running = () => Boolean(s && (s.running || s.view?.turnRunning));

  function renderHead() {
    const v = view();
    dom.title.textContent = s.title;
    let word = 'Practice';
    if (v?.legacy) word = 'Read-only';
    else if (v?.status === 'ended') word = 'Practice · ended';
    else if (v?.format === 'set') word = 'Practice · set';
    else if (v?.format === 'exam') word = 'Practice · exam paper';
    dom.word.textContent = word;
    const picked = v?.picked ?? [];
    dom.picked.hidden = !picked.length;
    dom.picked.textContent = picked.length ? `On: ${picked.map((p) => p.title).join(', ')}` : '';
  }

  /** The flow: stream, set or a legacy transcript. The one place a format draws its body. */
  function renderBody() {
    const v = view();
    // exam format: ui/exam.js draws the paper into #practice-exam-slot; the flow stays empty.
    if (v?.format === 'exam') {
      reconcile(dom.flow, [], s.nodes);
      return;
    }
    if (v?.legacy) {
      reconcile(dom.flow, legacyItems(), s.nodes);
      return;
    }
    reconcile(dom.flow, v ? (v.format === 'set' ? setItems(v) : streamItems(v)) : [], s.nodes);
  }

  const problemOf = (v, id) => v.problems.find((p) => p.problemId === id);

  function legacyItems() {
    return s.messages.map((msg, i) => ({
      key: `m${i}`,
      sig: String(i),
      build: () => {
        if (msg.role === 'user') {
          const e = h('div', 'entry user');
          e.append(h('p', 'you-label', 'You'), h('p', 'you-text', msg.content ?? ''));
          return e;
        }
        return assistantEl(msg, { showThinking: true, artifacts: [], ctx: ctx() });
      },
    }));
  }

  /** Feed items other than problems and conclusions, as flow items. */
  function feedItem(item, i) {
    switch (item.kind) {
      case 'learner':
        return { key: `learner:${i}:${item.at}`, sig: JSON.stringify(item), build: () => learnerEl(item) };
      case 'coach':
        return {
          key: `coach:${item.opId}`,
          sig: JSON.stringify([item.text, item.thinking]),
          build: () => assistantEl({ content: item.text, thinking: item.thinking }, { showThinking: true, artifacts: [], ctx: ctx() }),
        };
      default:
        return null;
    }
  }

  function withExtras(v, items) {
    const out = [];
    const nudges = s.nudges.filter((n) => items.some((it) => it.key === n.after));
    for (const it of items) {
      out.push(it);
      for (const n of nudges.filter((x) => x.after === it.key)) out.push({ key: n.key, sig: n.key, build: () => nudgeEl(n.text) });
    }
    for (const n of s.nudges.filter((x) => !nudges.includes(x))) out.push({ key: n.key, sig: n.key, build: () => nudgeEl(n.text) });
    if (v.status === 'ended') out.push({ key: 'summary', sig: JSON.stringify([v.problems.map((p) => p.result?.verdict), v.calibration]), build: () => summaryEl(v) });
    return out;
  }

  function streamItems(v) {
    const items = [];
    v.feed.forEach((item, i) => {
      if (item.kind === 'problem') {
        const p = problemOf(v, item.problemId);
        if (!p?.statement) return;
        items.push({
          key: `problem:${p.problemId}`,
          sig: JSON.stringify([p.statement, p.type, p.index, p.result?.retestOf ?? null]),
          build: () => problemCard(p, v),
        });
      } else if (item.kind === 'concluded') {
        const p = problemOf(v, item.problemId);
        items.push({
          key: `concluded:${item.problemId}`,
          sig: JSON.stringify([item.verdict, p?.result, p?.answerCheck, p?.hints]),
          build: () => concludedEl(item, p),
        });
      } else {
        const it = feedItem(item, i);
        if (it) items.push(it);
      }
    });
    return withExtras(v, items);
  }

  function setItems(v) {
    const items = [];
    const review = v.status === 'set-review' || v.status === 'ended';
    if (WRITING.has(v.status)) return withExtras(v, items);
    const handedIn = v.feed.findIndex((f) => f.kind === 'learner' && f.intent === 'set');
    if (handedIn >= 0) {
      const item = v.feed[handedIn];
      items.push({ key: 'handed-in', sig: JSON.stringify(item), build: () => learnerEl(item) });
    }
    for (const p of v.problems) {
      if (!p.statement) continue;
      items.push(
        review
          ? {
              key: `set:${p.problemId}`,
              sig: JSON.stringify([p, s.answers.get(p.problemId) ?? null, v.setSize]),
              build: () => setReviewCard(p, v),
            }
          : { key: `set:${p.problemId}`, sig: JSON.stringify([p.statement, p.type, p.index, v.setSize]), build: () => setOpenCard(p, v) },
      );
    }
    v.feed.forEach((item, i) => {
      if (item.kind === 'coach' || (item.kind === 'learner' && item.intent !== 'set')) {
        const it = feedItem(item, i);
        if (it) items.push(it);
      }
    });
    return withExtras(v, items);
  }

  // ----- blocks -----

  function kickerText(p, v) {
    let t = `Problem ${p.index + 1}`;
    if (v.format === 'set' && v.setSize) t += ` of ${v.setSize}`;
    if (TYPE_LABEL[p.type]) t += ` · ${TYPE_LABEL[p.type]}`;
    if (p.result?.retestOf) t += ' · Retest';
    return t;
  }

  function exerciseBlock(p, v, pending) {
    const block = h('section', `exercise problem${pending ? ' pending' : ''}`);
    block.dataset.problem = p.problemId;
    const k = h('h2', 'kicker', kickerText(p, v));
    k.id = nextId('prob');
    block.setAttribute('aria-labelledby', k.id);
    block.append(k, prose(p.statement, ctx(), 'prose statement'));
    return block;
  }

  function problemCard(p, v) {
    return exerciseBlock(p, v, false);
  }

  function learnerEl(item) {
    const e = h('div', 'entry user');
    e.append(h('p', 'you-label', 'You'));
    let text = item.text ?? '';
    if (item.intent === 'hint') text = 'Asked for a hint.';
    else if (item.intent === 'dismiss') text = 'Gave up on this problem.';
    else text = text.replace(CONFIDENCE_TAIL, '');
    e.append(h('p', 'you-text', text));
    const chips = attachmentList(item.attachments);
    if (chips) e.append(chips);
    if (typeof item.confidence === 'number') e.append(h('p', 'you-meta', confidenceLabel(item.confidence)));
    return e;
  }

  function verdictLine(verdict) {
    const [glyph, text] = VERDICTS[verdict] ?? ['•', verdict];
    const p = h('p', 'verdict');
    p.tabIndex = -1;
    p.dataset.refocus = '';
    const g = h('span', 'glyph', `${glyph} `);
    g.setAttribute('aria-hidden', 'true');
    p.append(g, document.createTextNode(`${text}.`));
    return p;
  }

  function duration(sec) {
    if (!sec) return '';
    return sec >= 90 ? `${Math.round(sec / 60)} min` : `${sec} s`;
  }

  /** Verdict, what was tested, and the revealed solution: only after the problem concluded. */
  function resultBlock(p, verdict) {
    const r = p?.result;
    const wrap = h('div', 'result');
    wrap.append(verdictLine(r?.verdict ?? verdict));
    if (!r) return wrap;
    const meta = [];
    if (r.targets?.length) meta.push(`Tested: ${r.targets.map((t) => t.title).join(', ')}`);
    if (p.hints) meta.push(`${plural(p.hints, 'hint')} used`);
    meta.push(`Rated ${r.rating}`);
    const d = duration(r.durationSec);
    if (d) meta.push(d);
    wrap.append(h('p', 'you-meta result-meta', meta.join(' · ')));
    if (r.missClass && MISS[r.missClass]) wrap.append(h('p', 'result-miss', `What went wrong: ${MISS[r.missClass]}`));
    if (r.misconception) wrap.append(h('p', 'result-miss', `Misconception: ${r.misconception}`));
    wrap.append(revealEl(p));
    return wrap;
  }

  function revealEl(p) {
    const rev = p.result.reveal;
    const d = h('details', 'reveal');
    d.open = true;
    d.append(h('summary', '', 'Worked solution'));
    const body = h('div', 'reveal-body');
    if (rev.solution?.length) {
      const ol = h('ol', 'reveal-moves');
      for (const m of rev.solution) {
        const li = h('li');
        if (m.decisionPoint) li.append(h('span', 'reveal-key', 'Key step'));
        li.append(prose(m.move, ctx(), 'prose'));
        ol.append(li);
      }
      body.append(ol);
    }
    if (rev.finalAnswer) body.append(h('p', 'reveal-label', 'Answer'), prose(rev.finalAnswer, ctx(), 'prose'));
    if (rev.acceptableVariants?.length) {
      body.append(h('p', 'reveal-label', 'Also accepted'));
      const ul = h('ul', 'reveal-list');
      for (const x of rev.acceptableVariants) {
        const li = h('li');
        li.append(prose(x, ctx(), 'prose'));
        ul.append(li);
      }
      body.append(ul);
    }
    if (rev.rubric?.length) {
      body.append(h('p', 'reveal-label', 'What a good answer has'));
      const ul = h('ul', 'reveal-list');
      for (const c of rev.rubric) {
        const li = h('li');
        li.append(prose(c.criterion, ctx(), 'prose'));
        if (c.essential) li.append(h('span', 'reveal-key', 'Essential'));
        ul.append(li);
      }
      body.append(ul);
    }
    if (p.answerCheck === 'passed') body.append(h('p', 'reveal-note', 'A script reproduced this answer.'));
    else if (p.answerCheck === 'skipped') {
      body.append(h('p', 'reveal-note', 'Not checked by a script (Python with SymPy isn’t available here), so this answer is unverified.'));
    }
    d.append(body);
    return d;
  }

  function concludedEl(item, p) {
    const block = h('section', 'concluded');
    block.dataset.problem = item.problemId;
    block.setAttribute('aria-label', `Result: ${(VERDICTS[item.verdict] ?? [, item.verdict])[1]}`);
    block.append(resultBlock(p, item.verdict));
    return block;
  }

  // ----- set cards -----

  function persistDraft(id, patch) {
    const d = { text: '', confidence: null, ...s.drafts.get(id), ...patch };
    if (!d.text && d.confidence == null) s.drafts.delete(id);
    else s.drafts.set(id, d);
    saveDrafts(s.id, s.drafts);
    renderControls();
  }

  function setOpenCard(p, v) {
    const block = exerciseBlock(p, v, true);
    const form = h('div', 'ex-form');
    const draft = s.drafts.get(p.problemId);
    const field = h('div', 'field');
    const label = h('label', '', `Your answer to problem ${p.index + 1}`);
    const ta = h('textarea', 'input');
    ta.id = nextId('set-answer');
    label.htmlFor = ta.id;
    ta.rows = 4;
    ta.placeholder = 'Working and answer. Leave it empty if you don’t know.';
    ta.value = draft?.text ?? '';
    ta.dataset.first = '';
    ta.addEventListener('input', () => persistDraft(p.problemId, { text: ta.value }));
    field.append(label, ta);
    const conf = confidenceField({ legend: 'How sure are you? (optional)' });
    conf.set(draft?.confidence ?? null);
    for (const r of conf.radios) r.addEventListener('change', () => persistDraft(p.problemId, { confidence: conf.value() }));
    form.append(field, conf.el);
    block.append(form);
    return block;
  }

  function setReviewCard(p, v) {
    const block = exerciseBlock(p, v, false);
    const a = s.answers.get(p.problemId);
    const you = h('div', 'you');
    you.append(h('p', 'you-label', 'You'));
    you.append(h('p', 'you-text', a?.text?.trim() ? a.text : 'No answer.'));
    if (typeof a?.confidence === 'number') you.append(h('p', 'you-meta', confidenceLabel(a.confidence)));
    block.append(you);
    if (p.result) block.append(resultBlock(p, p.result.verdict));
    else block.append(h('p', 'writing', 'Reviewing…'));
    return block;
  }

  // ----- the end of a session -----

  function summaryEl(v) {
    const sec = h('section', 'practice-summary');
    const id = nextId('sum');
    const title = h('h2', 'summary-title', 'Practice finished');
    title.id = id;
    title.tabIndex = -1;
    sec.setAttribute('aria-labelledby', id);
    sec.append(title);
    const done = v.problems.filter((p) => p.result);
    if (!done.length) {
      sec.append(h('p', 'summary-line', 'No problems were finished.'));
      return sec;
    }
    const count = (verdict) => done.filter((p) => p.result.verdict === verdict).length;
    const parts = [
      [count('correct'), 'correct'],
      [count('partial'), 'partly right'],
      [count('wrong'), 'not quite'],
      [count('dontknow'), 'don’t know'],
      [count('dismissed'), 'skipped'],
    ]
      .filter(([n]) => n)
      .map(([n, w]) => `${n} ${w}`);
    sec.append(h('p', 'summary-line', `${plural(done.length, 'problem')}: ${parts.join(', ')}.`));
    const extra = [];
    const hints = done.reduce((n, p) => n + p.hints, 0);
    if (hints) extra.push(`${plural(hints, 'hint')} used`);
    const retests = done.filter((p) => p.result.retestOf).length;
    if (retests) extra.push(`${plural(retests, 'retest')}`);
    if (extra.length) sec.append(h('p', 'you-meta', extra.join(' · ')));
    const ideas = [...new Set(done.flatMap((p) => p.result.targets.map((t) => t.title)))];
    if (ideas.length) sec.append(h('p', 'summary-ideas', `Ideas: ${ideas.join(', ')}`));
    const calibration = calibrationBlock(v.calibration);
    if (calibration) sec.append(calibration);
    return sec;
  }

  // ----- busy line, live reply, errors -----

  function busyText() {
    if (s.midway) return BUSY.midway;
    const st = s.view?.status;
    return WRITING.has(st ?? 'writing') ? BUSY[st ?? 'writing'] : BUSY.other;
  }

  function removeBusy() {
    clearTimeout(s?.slowTimer);
    s?.busyEl?.remove();
    if (s) {
      s.busyEl = null;
      s.busyKey = '';
    }
  }

  function renderBusy() {
    if (!s) return;
    if (!running() || s.live) {
      removeBusy();
      return;
    }
    const text = busyText();
    if (s.busyEl && s.busyKey === text) return;
    removeBusy();
    const p = h('p', 'writing', text);
    p.setAttribute('role', 'status');
    s.busyEl = p;
    s.busyKey = text;
    dom.live.prepend(p);
    if (!s.midway) {
      s.slowTimer = setTimeout(() => {
        if (s?.busyEl === p && p.isConnected) p.after(h('p', 'writing', BUSY.slow));
      }, SLOW_MS);
    }
  }

  /** A turn the coach owes: a failed one, or one an app restart cut short. */
  function owesReply(v) {
    if (!v || v.legacy || v.status === 'ended' || v.turnRunning || v.format === 'exam') return false;
    if (WRITING.has(v.status)) return true;
    if (v.status === 'set-review') return v.problems.some((p) => !p.result);
    const last = v.feed.at(-1);
    return last?.kind === 'learner' && last.intent !== 'dismiss' && last.intent !== 'set';
  }

  function currentError() {
    if (!s || running()) return null;
    return s.turnError ?? s.view?.lastTurnError ?? null;
  }

  function renderError() {
    if (!s) return;
    const err = currentError();
    const owed = !err && owesReply(s.view);
    const key = err ? `e:${err}` : owed ? 'owed' : '';
    if (key === s.errorKey) return;
    s.errorKey = key;
    s.errorEl?.remove();
    s.errorEl = null;
    if (!key) return;
    const el = h('div', 'turn-error');
    el.setAttribute('role', err ? 'alert' : 'status');
    el.append(h('p', 'turn-error-title', err ? 'The last reply failed.' : 'The coach hasn’t answered this yet.'));
    if (err) el.append(h('p', 'turn-error-text', err));
    const actions = h('div', 'actions');
    const retry = button(err ? 'Try again' : 'Get a reply', 'secondary');
    retry.addEventListener('click', () => act('practice_resume', {}, 'Couldn’t resume'));
    actions.append(retry);
    if (err && /model|connect|settings/i.test(err)) {
      const st = button('Settings', 'text');
      st.addEventListener('click', () => deps.openSettings({ to: 'model' }));
      actions.append(st);
    }
    el.append(actions);
    s.errorEl = el;
    dom.live.append(el);
  }

  // ----- the answer card (the composer lives here) -----

  function answerSpec(v) {
    if (!v || v.legacy || v.format === 'exam') return null;
    if (v.status === 'open') {
      const p = v.problems.find((x) => x.status === 'open');
      return p ? { key: `open:${p.problemId}`, kind: 'open', p } : null;
    }
    const last = v.problems.at(-1)?.problemId ?? '';
    if (v.status === 'concluded') return { key: `ask:${last}`, kind: 'ask', label: 'Ask about this problem' };
    if (v.status === 'set-review') return { key: 'ask:review', kind: 'ask', label: 'Ask about this review' };
    return null;
  }

  function buildCard(spec) {
    const card = h('section', `exercise answer-card${spec.kind === 'open' ? ' pending' : ''}`);
    const k = h('h2', 'kicker', spec.kind === 'open' ? 'Your answer' : spec.label);
    k.id = nextId('ans');
    card.setAttribute('aria-labelledby', k.id);
    card.append(k);
    const built = { el: card, spec, conf: null, hint: null, giveUp: null, slot: h('div', 'practice-composer') };
    if (spec.kind === 'open') {
      card.append(h('p', 'answer-help', 'Show your working, ask a question, or give your answer.'));
      built.conf = confidenceField({ legend: 'How sure are you?', hint: 'Pick one when you give an answer.' });
      card.append(built.conf.el);
    }
    card.append(built.slot);
    if (spec.kind === 'open') {
      const actions = h('div', 'actions');
      built.hint = button('Hint', 'secondary');
      built.hint.addEventListener('click', () => act('practice_hint', {}, 'Couldn’t get a hint'));
      built.giveUp = button('I give up — show me', 'text');
      built.giveUp.addEventListener('click', () => giveUp(built));
      actions.append(built.hint, built.giveUp);
      card.append(actions);
    }
    return built;
  }

  /** "I give up" counts as missed: the first click asks, the second (within a few seconds) does it. */
  function giveUp(card) {
    const b = card.giveUp;
    if (b.dataset.armed) {
      clearTimeout(s.armTimer);
      disarm(b);
      act('practice_dismiss', {}, 'Couldn’t skip the problem');
      return;
    }
    b.dataset.armed = '1';
    b.textContent = 'Sure? It counts as missed';
    clearTimeout(s.armTimer);
    s.armTimer = setTimeout(() => disarm(b), ARM_MS);
  }

  function disarm(b) {
    delete b.dataset.armed;
    b.textContent = 'I give up — show me';
  }

  function renderAnswer() {
    if (!s) return;
    const v = view();
    const spec = answerSpec(v);
    const key = spec?.key ?? '';
    if (key !== s.cardKey) {
      const hadFocus = dom.answer.contains(document.activeElement);
      s.card = null;
      s.cardKey = key;
      composerHost.unmount();
      dom.answer.textContent = '';
      // A new problem (or none) starts from an empty box: nothing typed for the last one carries over.
      composerHost.reset();
      if (spec) {
        s.card = buildCard(spec);
        dom.answer.append(s.card.el);
        composerHost.mount(s.card.slot, {
          sessionId: s.id,
          label: spec.kind === 'open' ? 'Your working or answer' : spec.label,
          placeholder: spec.kind === 'open' ? 'Working or answer…' : 'Ask the coach…',
          focus: hadFocus,
        });
      }
    }
    if (s.card?.hint) {
      const n = spec.p.hints;
      s.card.hint.textContent = n ? `Hint (${n} used)` : 'Hint';
    }
  }

  // ----- the desk -----

  /** The desk's one state: a note, at most one primary, one quiet button. */
  function deskState(v) {
    const can = v?.can ?? {};
    const ok = (flag) => !(s.acting || !flag);
    if (!v) return { note: 'Starting…' };
    // exam format: the paper owns its own controls (ui/exam.js); no Hint, Skip or Next here.
    if (v.format === 'exam') return { note: '' };
    if (v.legacy) {
      return { note: 'Made with an older tutor. Read-only.', primary: { label: 'Start a new practice session', act: 'new', on: true } };
    }
    switch (v.status) {
      case 'writing':
        return { note: BUSY.writing };
      case 'set-writing':
        return { note: BUSY['set-writing'] };
      case 'open': {
        const p = v.problems.find((x) => x.status === 'open');
        const n = p ? p.index + 1 : v.problems.length;
        return { note: `Problem ${n}${p?.hints ? ` · ${plural(p.hints, 'hint')} used` : ''}` };
      }
      case 'concluded': {
        const n = v.problems.at(-1) ? v.problems.at(-1).index + 1 : 0;
        if (v.wrapUp) {
          const left = v.retestsLeft;
          return left
            ? {
                note: `Retesting what you missed: ${left} left.`,
                primary: { label: 'Next retest', act: 'next', on: ok(can.next) },
                secondary: { label: 'Skip retests and end', act: 'skip', on: ok(can.end) },
              }
            : { note: 'Retests done.', primary: { label: 'End practice', act: 'end', on: ok(can.end) } };
        }
        return {
          note: `Problem ${n} done.`,
          primary: { label: 'Next problem', act: 'next', on: ok(can.next) },
          secondary: { label: 'End practice', act: 'end', on: ok(can.end) },
        };
      }
      case 'set-open': {
        const answered = v.problems.filter((p) => {
          const d = s.drafts.get(p.problemId);
          return d?.text?.trim();
        }).length;
        const size = v.problems.length;
        const rest = size - answered;
        return {
          note: `${answered} of ${size} answered.${rest ? ' Unanswered ones count as “don’t know”.' : ''}`,
          primary: { label: 'Hand in the set', act: 'handin', on: ok(can.submitSet) },
        };
      }
      case 'set-review': {
        const reviewing = v.problems.some((p) => !p.result);
        return { note: reviewing ? 'The coach is reviewing your answers…' : 'Review.', primary: { label: 'End practice', act: 'end', on: ok(can.end) } };
      }
      case 'ended':
        return {
          note: 'Practice ended.',
          primary: { label: 'Practise again', act: 'again', on: true },
          secondary: { label: 'Back to sessions', act: 'back', on: true },
        };
      default:
        return { note: '' };
    }
  }

  function renderControls() {
    if (!s) return;
    const d = deskState(view());
    if (dom.note.textContent !== d.note) dom.note.textContent = d.note;
    for (const [el, spec] of [
      [dom.primary, d.primary],
      [dom.secondary, d.secondary],
    ]) {
      el.hidden = !spec;
      if (!spec) continue;
      if (el.textContent !== spec.label) el.textContent = spec.label;
      el.dataset.act = spec.act;
      el.disabled = !spec.on;
    }
    dom.stop.hidden = !running();
    if (s.card?.hint) {
      const can = view()?.can ?? {};
      s.card.hint.disabled = s.acting || !can.hint;
      s.card.giveUp.disabled = s.acting || !can.dismiss;
    }
  }

  // ----- doing things -----

  /** Runs one command that may run a coach turn; its failures read in the head unless the turn error already says them. */
  async function act(name, args = {}, failure = 'Couldn’t do that') {
    if (!s || s.acting) return;
    const mine = s;
    s.acting = true;
    dom.status.textContent = '';
    renderControls();
    let failed = null;
    try {
      await cmd(name, { sessionId: mine.id, ...args });
    } catch (e) {
      failed = e;
    }
    if (s !== mine) return;
    s.acting = false;
    await refresh();
    if (s === mine && failed !== null && !currentError()) dom.status.textContent = `${failure}: ${failed}`;
    if (s === mine) renderControls();
  }

  const learnerCount = (v) => v?.feed.filter((f) => f.kind === 'learner').length ?? 0;

  /** The composer's send. Returns after the coach's turn; a refused send gives the text back. */
  async function send(text, attachments = []) {
    if (!s?.view) return;
    if (s.acting) {
      composerHost.restore(text, attachments, '');
      return;
    }
    const mine = s;
    const conf = s.card?.conf ?? null;
    const confidence = conf?.value() ?? null;
    const before = learnerCount(s.view);
    conf?.clear();
    s.acting = true;
    dom.status.textContent = '';
    renderControls();
    let failed = null;
    try {
      await cmd('practice_submit', {
        sessionId: mine.id,
        text,
        attachments,
        ...(confidence != null ? { confidence } : {}),
      });
    } catch (e) {
      failed = e;
    }
    if (s !== mine) return;
    s.acting = false;
    await refresh();
    if (s === mine && failed !== null && !currentError()) {
      if (learnerCount(s.view) > before) dom.status.textContent = `Couldn’t send: ${failed}`;
      else {
        // Nothing was recorded: give the text and the confidence back with the reason.
        composerHost.restore(text, attachments, String(failed));
        if (confidence != null) s.card?.conf?.set(confidence);
      }
    }
    if (s === mine) renderControls();
  }

  function stop() {
    if (s) cmd('cancel_reply', { sessionId: s.id }).catch((e) => (dom.status.textContent = `Couldn’t stop: ${e}`));
  }

  function handIn() {
    const v = view();
    if (!v || v.status !== 'set-open') return;
    const answers = [];
    for (const p of v.problems) {
      const d = s.drafts.get(p.problemId);
      if (!d?.text?.trim() && d?.confidence == null) continue;
      answers.push({ problemId: p.problemId, text: d.text ?? '', ...(d.confidence != null ? { confidence: d.confidence } : {}) });
    }
    for (const a of answers) s.answers.set(a.problemId, { text: a.text, confidence: a.confidence ?? null });
    const mine = s;
    act('practice_submit_set', { answers }, 'Couldn’t hand in the set').then(() => {
      // Handed in: the local copies are no longer needed.
      if (mine.view?.status && mine.view.status !== 'set-open') {
        mine.drafts.clear();
        saveDrafts(mine.id, mine.drafts);
      }
    });
  }

  function onDesk(actName) {
    const v = view();
    switch (actName) {
      case 'next':
        act('practice_next', {}, 'Couldn’t start the next problem');
        break;
      case 'end':
        act('practice_end', {}, 'Couldn’t end the session');
        break;
      case 'skip':
        act('practice_end', { skipRetests: true }, 'Couldn’t end the session');
        break;
      case 'handin':
        handIn();
        break;
      case 'again':
        deps.start({ targets: (v?.picked ?? []).map((p) => p.id), format: v?.format === 'set' ? 'set' : 'stream' });
        break;
      case 'new':
        deps.start({ targets: [], format: practiceFormat() });
        break;
      case 'back':
        deps.onBack();
        break;
    }
  }

  // ----- focus, scroll, announcements -----

  /** Focus is nowhere useful: gone, on the page body, or parked on the heading while the coach wrote. */
  const lostFocus = () => {
    const a = document.activeElement;
    return !a || a === document.body || a === dom.title || !a.isConnected || a.disabled || Boolean(a.closest('[hidden]')) || !root.contains(a);
  };

  function alignUnderHead(el) {
    const box = dom.scroll;
    const top = box.scrollTop + el.getBoundingClientRect().top - box.getBoundingClientRect().top - ALIGN_GAP;
    box.scrollTo({ top: Math.max(0, top), behavior: 'instant' });
  }

  const focusQuiet = (el) => el?.focus({ preventScroll: true });

  /** The learner's next action on this screen. */
  function nextFocus() {
    const v = view();
    if (!v) return dom.title;
    if (v.legacy) return dom.primary;
    if (v.status === 'open' && !running()) return document.getElementById('prompt');
    if (v.status === 'set-open') return dom.flow.querySelector('[data-first]');
    if (v.status === 'ended') return dom.flow.querySelector('.summary-title');
    if (!dom.primary.hidden && !dom.primary.disabled) return dom.primary;
    return dom.title;
  }

  /** Session open: the last problem (or result) sits under the head and focus lands on the next action. */
  function resume() {
    const blocks = dom.flow.querySelectorAll('.problem, .concluded');
    const v = view();
    const target = v?.status === 'ended' ? dom.flow.querySelector('.practice-summary') : blocks[blocks.length - 1];
    if (target) alignUnderHead(target);
    focusQuiet(nextFocus());
  }

  function transition(prev, v, turnEnded) {
    const was = prev?.status;
    let said = false;
    const say = (text) => {
      announce(text);
      said = true;
    };
    if (v.status !== was) {
      if (v.status === 'open' && (was === 'writing' || was == null || was === 'concluded')) {
        const p = v.problems.find((x) => x.status === 'open');
        const card = p && dom.flow.querySelector(`.problem[data-problem="${CSS.escape(p.problemId)}"]`);
        if (card) alignUnderHead(card);
        say('Problem ready.');
        if (lostFocus()) focusQuiet(nextFocus());
      } else if (v.status === 'concluded' && was === 'open') {
        const p = v.problems.at(-1);
        say(`Feedback: ${(VERDICTS[p?.result?.verdict] ?? [, 'concluded'])[1]}.`);
        if (lostFocus() || dom.answer.contains(document.activeElement)) {
          const verdict = dom.flow.querySelector(`.concluded[data-problem="${CSS.escape(p?.problemId ?? '')}"] .verdict`);
          focusQuiet(verdict ?? nextFocus());
        }
      } else if (v.status === 'set-open') {
        const first = dom.flow.querySelector('.problem');
        if (first) alignUnderHead(first);
        say('The set is ready.');
        if (lostFocus()) focusQuiet(nextFocus());
      } else if (v.status === 'set-review') {
        say('Handed in. The coach is reviewing.');
        if (lostFocus()) focusQuiet(dom.flow.querySelector('.problem'));
      } else if (v.status === 'ended') {
        say('Practice ended.');
        const sum = dom.flow.querySelector('.practice-summary');
        if (sum) alignUnderHead(sum);
        if (lostFocus() || [dom.primary, dom.secondary].includes(document.activeElement)) focusQuiet(nextFocus());
      } else if (lostFocus()) {
        focusQuiet(nextFocus());
      }
    } else if (v.wrapUp && !prev?.wrapUp) {
      say(`Retesting what you missed: ${v.retestsLeft} left.`);
      // The button that was End now reads "Skip retests and end": focus goes to Next retest.
      if (document.activeElement === dom.secondary) focusQuiet(dom.primary);
    }
    if (turnEnded) {
      if (!s.turnError && !said) announce('Reply finished.');
      maybeNudge(v);
    }
  }

  /** Break nudge (ui-design §7.8): a UI-only line at a turn boundary, after the newest flow item. */
  function maybeNudge() {
    if (!takeNudge()) return;
    const last = [...s.nodes.keys()].filter((k) => !k.startsWith('nudge:')).at(-1);
    s.nudges.push({ key: `nudge:${performance.now()}`, after: last, text: NUDGE_TEXT });
    renderBody();
  }

  // ----- wiring -----

  dom.stop.addEventListener('click', stop);
  dom.primary.addEventListener('click', () => onDesk(dom.primary.dataset.act));
  dom.secondary.addEventListener('click', () => onDesk(dom.secondary.dataset.act));
  $('#practice-back').addEventListener('click', () => deps.onBack());

  return {
    open,
    close,
    event,
    refresh,
    send,
    stop,
    setTitle,
    /** A line in the head's status (a start that was refused, for instance). */
    say: (text) => {
      dom.status.textContent = text ?? '';
    },
    isOpen: (id) => Boolean(s && (id == null || s.id === id)),
    id: () => s?.id ?? null,
    isRunning: running,
    /** Send is the quiet button while Next is the one primary. */
    sendQuiet: () => Boolean(s?.view && (s.view.status === 'concluded' || s.view.status === 'set-review')),
  };
}
