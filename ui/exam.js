// Exam mode (architecture §4.14; the backend contract is docs/ui-design.md §11c).
//
// Two pieces, both drawn only from what the backend returns:
//
// - `createExamPanel`: the exam dashboard sheet of an exam goal's Learn
//   session. One read, `exam_plan`: days left, the day's sitting, the
//   expected-score estimate (always labelled an estimate), coverage per topic,
//   the ideas in priority order, and "Sit a timed paper" with its time budget.
// - `createExamPaper`: the paper on the Practice screen (`practice_state`
//   with format "exam"). The app keeps the clock: the countdown here only
//   reads the deadline the backend wrote, on the backend's clock (`exam.now`),
//   never the device's. There are no hints, no skipping and no feedback until
//   the paper is handed in; at the deadline the backend hands in what was
//   saved (`practice_exam_save`), and the review shows marks per problem.

import { confidenceField, confidenceLabel } from './cards.js';
import { confirmDialog } from './dialog.js';
import { assistantEl } from './reply.js';

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

const pct = (x) => `${Math.round((x ?? 0) * 100)}%`;
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

/** `YYYY-MM-DD…` as a readable date; the day is the exam's, whatever the time zone. */
function dateText(date) {
  const day = /^\d{4}-\d{2}-\d{2}/.test(date ?? '') ? date.slice(0, 10) : null;
  if (!day) return date ?? '';
  return new Date(`${day}T00:00:00Z`).toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' });
}

function daysText(left) {
  if (left == null) return 'No exam date set';
  if (left < 0) return 'The exam date has passed';
  if (left === 0) return 'The exam is today';
  return `${plural(left, 'day')} left`;
}

function clockText(sec) {
  const s = Math.max(0, Math.floor(sec));
  const m = Math.floor(s / 60);
  const rest = String(s % 60).padStart(2, '0');
  if (m >= 60) return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, '0')}:${rest}`;
  return `${m}:${rest}`;
}

function spoken(sec) {
  const m = Math.floor(sec / 60);
  return m >= 1 ? plural(m, 'minute') : plural(Math.max(0, Math.floor(sec)), 'second');
}

// ---------- the dashboard ----------

const MODE_LABEL = { teach: 'Teach', practice: 'Practise' };
const IDEAS_SHOWN = 8;

/**
 * @param {object} deps
 * @param {HTMLButtonElement} deps.toggle   the head's "Exam" button
 * @param {HTMLDialogElement} deps.dialog
 * @param {(name: string, args?: object) => Promise<any>} deps.cmd
 * @param {(opts: { goalSessionId: string, budgetMin: number, report: (text: string) => void }) => Promise<boolean>} deps.start
 *        starts a paper; resolves true when the Practice screen opened
 */
export function createExamPanel({ toggle, dialog, cmd, start }) {
  let sessionId = null;
  let seq = 0;
  let plan = null;
  let showAll = false;

  function sync(session, st) {
    const isExam = Boolean(session?.id && st?.goal?.purpose === 'exam' && st.plan);
    if (!isExam) {
      if (sessionId) {
        sessionId = null;
        plan = null;
      }
      toggle.hidden = true;
      if (dialog.open) dialog.close();
      return;
    }
    if (sessionId !== session.id) {
      sessionId = session.id;
      plan = null;
      showAll = false;
    }
    toggle.hidden = false;
  }

  function close() {
    seq++;
    sessionId = null;
    plan = null;
    toggle.hidden = true;
    if (dialog.open) dialog.close();
  }

  async function open() {
    if (!sessionId) return;
    const mine = sessionId;
    const ask = ++seq;
    dialog.replaceChildren(sheet(h('p', 'help', 'Reading the plan…'), 'Exam'));
    if (!dialog.open) dialog.showModal();
    let data;
    try {
      data = await cmd('exam_plan', { sessionId: mine });
    } catch (e) {
      if (ask !== seq || mine !== sessionId) return;
      const err = h('p', 'field-error', `Couldn’t read the exam plan: ${e}`);
      err.setAttribute('role', 'alert');
      dialog.replaceChildren(sheet(err, 'Exam'));
      return;
    }
    if (ask !== seq || mine !== sessionId) return;
    plan = data;
    render();
    dialog.querySelector('#exam-heading')?.focus({ preventScroll: true });
  }

  /** The sheet's frame: head with Close, then `body`. */
  function sheet(body, title) {
    const col = h('div', 'settings-col exam-col');
    const head = h('div', 'settings-head');
    const hd = h('h2', '', title);
    hd.id = 'exam-heading';
    hd.tabIndex = -1;
    const close = button('Close', 'text');
    close.addEventListener('click', () => dialog.close());
    head.append(hd, close);
    col.append(head, body);
    return col;
  }

  function section(title, ...children) {
    const sec = h('section', 'settings-section exam-section');
    const hd = h('h2', '', title);
    hd.id = nextId('exam-sec');
    sec.setAttribute('aria-labelledby', hd.id);
    sec.append(hd, ...children);
    return sec;
  }

  function todayBlock(p) {
    const t = p.today;
    const byId = new Map(p.nodes.map((n) => [n.id, n]));
    const names = t.nodeIds.map((id) => byId.get(id)?.title ?? id);
    const box = h('div', 'exam-today');
    const lead = {
      teach: `Teach ${names[0] ?? 'an idea'}.`,
      paper: 'Sit a timed paper.',
      practice: `Practise ${names.join(', ')}.`,
      over: 'The exam date has passed.',
      none: 'Nothing is ranked yet.',
    }[t.kind];
    box.append(h('p', 'exam-today-lead', lead), h('p', 'help', t.reason));
    return box;
  }

  function estimateBlock(p) {
    const sim = p.simulation;
    const box = h('div', 'exam-estimate');
    const line = h('p', 'exam-estimate-line');
    line.append(h('span', 'exam-figure', `≈ ${pct(sim.expectedNow)}`), document.createTextNode(' expected now'));
    if (sim.expectedExam != null) {
      line.append(document.createTextNode(' · '), h('span', 'exam-figure', `≈ ${pct(sim.expectedExam)}`), document.createTextNode(' on the exam day if you stop now'));
    }
    box.append(h('p', 'exam-label', 'Estimate'), line, h('p', 'help', sim.note));
    if (sim.unseen) box.append(h('p', 'help', `${plural(sim.unseen, 'weighted idea')} ${sim.unseen === 1 ? 'has' : 'have'} no answers yet and count as 0.`));
    if (sim.raise.length) {
      box.append(h('h3', 'exam-sub', 'What would raise it most'));
      const ul = h('ul', 'exam-list');
      for (const r of sim.raise) {
        const li = h('li');
        li.append(h('span', 'exam-idea', r.title), document.createTextNode(` · ${MODE_LABEL[r.mode]} · about +${(r.gain * 100).toFixed(1)} points on the exam day`));
        ul.append(li);
      }
      box.append(ul);
    }
    return box;
  }

  function coverageBlock(p) {
    const topics = p.simulation.topics;
    const total = topics.reduce((n, t) => n + t.weight, 0) || 1;
    const ul = h('ul', 'exam-bars');
    for (const t of topics) {
      const li = h('li', 'exam-bar-row');
      const share = pct(t.weight / total);
      const label = h('p', 'exam-bar-label');
      label.append(h('span', 'exam-idea', t.title), document.createTextNode(` · ${share} of the exam · ${plural(t.nodes, 'idea')}`));
      const meter = h('div', 'exam-meter');
      const speech = `${t.title}: expected ${pct(t.expectedNow)} now${t.expectedExam != null ? `, ${pct(t.expectedExam)} on the exam day` : ''}, ${share} of the exam`;
      meter.setAttribute('role', 'img');
      meter.setAttribute('aria-label', speech);
      const fill = h('span', 'exam-meter-fill');
      fill.style.inlineSize = pct(t.expectedNow);
      meter.append(fill);
      if (t.expectedExam != null) {
        const mark = h('span', 'exam-meter-day');
        mark.style.insetInlineStart = pct(t.expectedExam);
        meter.append(mark);
      }
      const nums = h('p', 'you-meta', `${pct(t.expectedNow)} now${t.expectedExam != null ? ` · ${pct(t.expectedExam)} on the exam day` : ''}`);
      li.append(label, meter, nums);
      ul.append(li);
    }
    const sec = [ul];
    if (topics.length) sec.push(h('p', 'help', 'The bar is the expected score now; the mark on it is the exam day.'));
    return sec;
  }

  function ideasBlock(p) {
    const shown = showAll ? p.nodes : p.nodes.slice(0, IDEAS_SHOWN);
    const ol = h('ol', 'exam-ideas');
    for (const n of shown) {
      const li = h('li', 'exam-idea-row');
      const head = h('p', 'exam-idea-head');
      head.append(h('span', 'exam-idea', n.title), document.createTextNode(` · ${n.homeTitle}`));
      const chip = h('span', `exam-chip ${n.mode}`, MODE_LABEL[n.mode]);
      head.append(document.createTextNode(' '), chip);
      const recall = n.evidence ? `recall ${pct(n.retrievability)}` : 'no answers yet';
      const terms = h('p', 'you-meta', `weight ${n.weight.toFixed(2)} · ${recall} · ${plural(n.layerGap, 'layer')} not shown · priority ${n.priority.toFixed(2)}`);
      li.append(head, terms, h('p', 'help', n.reason));
      ol.append(li);
    }
    const out = [ol];
    if (p.nodes.length > IDEAS_SHOWN) {
      const more = button(showAll ? 'Show fewer' : `Show all ${p.nodes.length}`, 'text');
      more.addEventListener('click', () => {
        showAll = !showAll;
        render();
        dialog.querySelector('.exam-ideas')?.scrollIntoView({ block: 'start' });
      });
      out.push(more);
    }
    if (p.unmapped.length) {
      out.push(h('p', 'help', `${plural(p.unmapped.length, 'idea')} of the plan ${p.unmapped.length === 1 ? 'isn’t' : 'aren’t'} in your knowledge graph yet (${p.unmapped.map((u) => u.title).join(', ')}), so ${p.unmapped.length === 1 ? 'it isn’t' : 'they aren’t'} ranked.`));
    }
    out.push(h('p', 'help', 'Priority = weight × (1 − recall) × (layers not yet shown + 1). It is recomputed from your answers every time you open this.'));
    return out;
  }

  function mixBlock(p) {
    const m = p.mix;
    const box = h('div', 'exam-mix');
    if (m.sittings == null) {
      box.append(h('p', '', `${plural(m.teachNodes, 'idea')} need teaching; ${plural(m.practiceNodes, 'idea')} go through practice. Set the exam date to plan the sittings.`));
    } else {
      box.append(
        h('p', '', `${plural(m.sittings, 'sitting')} at one a day: about ${m.teachSittings} teaching, ${m.practiceSittings} practice, and ${plural(m.papers ?? 0, 'timed paper')} among the practice.`),
        h('p', 'help', `Teaching is kept for the ${plural(m.teachNodes, 'idea')} that keep${m.teachNodes === 1 ? 's' : ''} producing structural misses; everything else goes through practice.`),
      );
    }
    return box;
  }

  function papersBlock(p) {
    if (!p.papers.length) {
      return h('p', 'help', 'No past papers are in your reference libraries. The coach writes in past-paper style from the exam’s name and syllabus, and marks with a scheme of its own.');
    }
    const withScheme = p.papers.filter((x) => x.markScheme).length;
    const box = h('div');
    box.append(h('p', '', `${plural(p.papers.length, 'past paper')} in your libraries, ${withScheme} with a mark scheme.`));
    const ul = h('ul', 'exam-list');
    for (const x of p.papers.slice(0, 6)) ul.append(h('li', '', `${x.title}${x.markScheme ? ' (mark scheme found)' : ''}`));
    box.append(ul);
    return box;
  }

  function paperForm(p) {
    const form = h('form', 'exam-start');
    const field = h('div', 'field');
    const label = h('label', '', 'Time budget (minutes)');
    const input = h('input', 'input');
    input.type = 'number';
    input.inputMode = 'numeric';
    input.min = String(p.paper.minBudgetMin);
    input.max = String(p.paper.maxBudgetMin);
    input.step = '5';
    input.value = String(p.paper.defaultBudgetMin);
    input.id = nextId('exam-budget');
    label.htmlFor = input.id;
    field.append(label, input);
    const help = h('p', 'help');
    const status = h('p', 'field-error');
    status.setAttribute('role', 'status');
    const go = button('Start the paper', 'primary');
    go.type = 'submit';
    const size = (m) => Math.max(2, Math.min(10, Math.ceil(m / p.paper.minutesPerProblem)));
    const refresh = () => {
      const m = Number(input.value);
      const ok = Number.isInteger(m) && m >= p.paper.minBudgetMin && m <= p.paper.maxBudgetMin;
      help.textContent = ok
        ? `About ${plural(size(m), 'problem')}, ${p.paper.minutesPerProblem} minutes each, on your highest-priority ideas. The clock starts when the whole paper is written and can’t be paused. There are no hints; at the deadline what you saved is handed in.`
        : `Between ${p.paper.minBudgetMin} and ${p.paper.maxBudgetMin} minutes.`;
      go.disabled = !ok || p.paper.targets === 0 || go.dataset.busy === '1';
    };
    input.addEventListener('input', refresh);
    refresh();
    if (p.paper.targets === 0) {
      status.textContent = 'Nothing can go on a paper yet: every ranked idea needs teaching first, or the plan has no weighted ideas in the graph.';
    }
    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      if (go.disabled) return;
      go.dataset.busy = '1';
      refresh();
      status.textContent = 'Writing the paper…';
      const opened = await start({
        goalSessionId: p.sessionId,
        budgetMin: Number(input.value),
        report: (text) => (status.textContent = String(text ?? '')),
      });
      delete go.dataset.busy;
      if (opened) {
        if (dialog.open) dialog.close();
        return;
      }
      refresh();
    });
    form.append(field, help, go, status);
    return form;
  }

  function render() {
    if (!plan) return;
    const p = plan;
    const focus = dialog.contains(document.activeElement) ? document.activeElement : null;
    const body = h('div', 'exam-body');
    const when = p.exam.date ? `${dateText(p.exam.date)} · ` : '';
    const top = h('div', 'exam-top');
    top.append(h('p', 'exam-days', daysText(p.daysLeft)), h('p', 'you-meta', `${when}${p.exam.name}`));
    body.append(
      top,
      section('Today', todayBlock(p)),
      section('Expected score', estimateBlock(p)),
      section('Coverage by topic', ...coverageBlock(p)),
      section('Ideas in priority order', ...ideasBlock(p)),
      section('Teaching and practice', mixBlock(p)),
      section('Past papers', papersBlock(p)),
      section('Sit a timed paper', paperForm(p)),
    );
    dialog.replaceChildren(sheet(body, p.exam.name));
    if (focus?.id) dialog.querySelector(`#${CSS.escape(focus.id)}`)?.focus({ preventScroll: true });
  }

  toggle.addEventListener('click', () => open());
  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) dialog.close();
  });
  dialog.addEventListener('close', () => {
    if (!toggle.hidden) toggle.focus({ preventScroll: true });
  });

  return { sync, close, open };
}

// ---------- the paper ----------

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
const SAVE_MS = 1000;
const WARN_AT = [600, 300, 60];
/** After the clock reaches 0 the backend hands in by itself; if nothing has changed by then, ask. */
const NUDGE_MS = 9000;
/** A resume offer waits this long for a turn that is about to start. */
const OFFER_MS = 2500;

/**
 * @param {object} deps
 * @param {HTMLElement} deps.slot
 * @param {(name: string, args?: object) => Promise<any>} deps.cmd
 * @param {(text: string) => void} deps.announce
 * @param {(markdown: string, className?: string) => HTMLElement} deps.prose  markdown + KaTeX + figures
 * @param {() => Map<string, { text: string, confidence: number|null }>} deps.handedIn  the answers of the hand-in (op log)
 * @param {() => object} deps.ctx    artifact context for coach messages
 * @param {() => Promise<void>} deps.refresh   re-read `practice_state`
 * @param {() => void} deps.onBack
 */
export function createExamPaper({ slot, cmd, announce, prose, handedIn, ctx, refresh, onBack }) {
  let st = null; // { id, drafts, view, key, offset, tick, saveTimer, nudge, warned, expired, acting, saveEl, clockEl, barEl, flushed }

  function destroy() {
    if (!st) return;
    clearInterval(st.tick);
    clearTimeout(st.saveTimer);
    clearTimeout(st.nudge);
    st = null;
    slot.replaceChildren();
  }

  function reset(id) {
    destroy();
    st = {
      id,
      drafts: new Map(),
      seeded: false,
      view: null,
      key: '',
      offset: 0,
      tick: 0,
      saveTimer: 0,
      nudge: 0,
      warned: new Set(),
      expired: false,
      acting: false,
      statusEl: null,
      saveEl: null,
      clockEl: null,
      barEl: null,
      handBtn: null,
      stage: null,
    };
  }

  /** @param {object|null} view `practice_state`; anything but an exam paper clears the slot. */
  function update(view, sessionId) {
    if (!view || view.legacy || view.format !== 'exam' || !view.exam) {
      destroy();
      return;
    }
    if (!st || st.id !== sessionId) reset(sessionId);
    st.view = view;
    // The backend's clock, not the device's.
    st.offset = Date.parse(view.exam.now) - Date.now();
    if (!st.seeded) {
      st.seeded = true;
      for (const a of view.exam.draft ?? []) st.drafts.set(a.problemId, { text: a.text ?? '', confidence: a.confidence ?? null });
    }
    const key = JSON.stringify([
      view.status,
      view.turnRunning,
      view.lastTurnError,
      view.problems.map((p) => [p.problemId, Boolean(p.statement), p.result?.verdict ?? null, p.result?.marksAwarded ?? null]),
      view.exam.submitted,
      view.exam.score,
      view.feed.filter((f) => f.kind === 'coach').map((f) => f.opId),
      view.can,
      st.acting,
    ]);
    if (key !== st.key) {
      st.key = key;
      build();
    }
    tick();
    clearInterval(st.tick);
    if (view.status === 'set-open' && view.exam.deadline) st.tick = setInterval(tick, 1000);
  }

  const remaining = () => {
    const d = Date.parse(st.view.exam.deadline);
    return Number.isFinite(d) ? (d - (Date.now() + st.offset)) / 1000 : null;
  };

  // ----- the clock -----

  function tick() {
    if (!st || st.view.status !== 'set-open' || !st.clockEl) return;
    const left = remaining();
    if (left == null) return;
    st.clockEl.textContent = clockText(left);
    st.barEl.classList.toggle('low', left <= 300);
    for (const at of WARN_AT) {
      if (left <= at && !st.warned.has(at)) {
        st.warned.add(at);
        if (left > 0) announce(`${spoken(at)} left.`);
      }
    }
    if (left <= 0 && !st.expired) timeUp();
  }

  /** The deadline: save what is typed (the backend still takes it for a moment), lock the boxes, wait for the hand-in. */
  function timeUp() {
    st.expired = true;
    announce('Time is up. Handing in what you saved.');
    saveNow();
    lock();
    const mine = st;
    st.statusEl.textContent = 'Time is up. Your saved answers are being handed in.';
    const poll = async () => {
      if (st !== mine) return;
      await refresh();
      if (st !== mine || mine.view.status !== 'set-open') return;
      mine.nudge = setTimeout(async () => {
        if (st !== mine || mine.view.status !== 'set-open') return;
        // The backend's timer did not survive (a restart): resuming hands in the saved paper.
        try {
          await cmd('practice_resume', { sessionId: mine.id });
        } catch (e) {
          if (st === mine) mine.statusEl.textContent = `Couldn’t hand in: ${e}`;
        }
        if (st === mine) poll();
      }, mine.view.exam.closed ? 500 : NUDGE_MS);
    };
    poll();
  }

  function lock() {
    for (const el of slot.querySelectorAll('textarea, input[type="radio"]')) el.disabled = true;
    if (st.handBtn) st.handBtn.disabled = true;
  }

  // ----- drafts -----

  const draftAnswers = () => {
    const out = [];
    for (const p of st.view.problems) {
      const d = st.drafts.get(p.problemId);
      if (!d || (!d.text?.trim() && d.confidence == null)) continue;
      out.push({ problemId: p.problemId, text: d.text ?? '', ...(d.confidence != null ? { confidence: d.confidence } : {}) });
    }
    return out;
  };

  function changed(problemId, patch) {
    st.drafts.set(problemId, { text: '', confidence: null, ...st.drafts.get(problemId), ...patch });
    updateCount();
    clearTimeout(st.saveTimer);
    const mine = st;
    st.saveTimer = setTimeout(() => mine === st && saveNow(), SAVE_MS);
    if (st.saveEl) st.saveEl.textContent = '';
  }

  async function saveNow() {
    if (!st || st.view.status !== 'set-open') return;
    clearTimeout(st.saveTimer);
    const mine = st;
    try {
      await cmd('practice_exam_save', { sessionId: mine.id, answers: draftAnswers() });
      if (st === mine && mine.saveEl) mine.saveEl.textContent = 'Saved.';
    } catch (e) {
      // After the deadline the backend refuses; that is not the learner's to fix.
      if (st === mine && mine.saveEl && !mine.expired) mine.saveEl.textContent = `Couldn’t save: ${e}`;
    }
  }

  function updateCount() {
    const el = slot.querySelector('.exam-count');
    if (!el) return;
    const n = draftAnswers().filter((a) => a.text.trim()).length;
    el.textContent = `${n} of ${st.view.problems.length} answered.`;
  }

  // ----- building -----

  function build() {
    const v = st.view;
    const hadFocus = slot.contains(document.activeElement) ? document.activeElement : null;
    const focusKey = hadFocus?.dataset?.focus ?? null;
    st.statusEl = h('p', 'exam-status');
    st.statusEl.setAttribute('role', 'status');
    st.saveEl = null;
    st.clockEl = null;
    st.barEl = null;
    st.handBtn = null;
    const out = [];
    const posed = v.problems.filter((p) => p.statement);
    if (v.status === 'set-open') out.push(openPaper(v));
    else if (v.status === 'set-review' || v.status === 'ended') {
      const review = h('div', 'exam-paper');
      review.append(...reviewPaper(v));
      out.push(review);
    } else if (!posed.length || v.status === 'set-writing') out.push(writing(v));
    slot.replaceChildren(...out);
    if (st.expired || (v.status === 'set-open' && remaining() != null && remaining() <= 0)) lock();
    if (focusKey) slot.querySelector(`[data-focus="${CSS.escape(focusKey)}"]`)?.focus({ preventScroll: true });
    // A new stage of the paper: focus goes to its next action when it was nowhere useful.
    if (v.status !== st.stage) {
      st.stage = v.status;
      const a = document.activeElement;
      if (!a || a === document.body || !a.isConnected || a.id === 'practice-title') {
        slot.querySelector(v.status === 'set-open' ? '[data-first]' : '[data-focus="result"]')?.focus({ preventScroll: true });
      }
    }
  }

  /** An offer to resume a turn, shown only once the state has stood still: the turn may be about to start. */
  function offerLater(parent, label) {
    const mine = st;
    const key = st.key;
    setTimeout(() => {
      if (st !== mine || st.key !== key || st.view.turnRunning || st.view.lastTurnError) return;
      const go = button(label, 'secondary');
      go.addEventListener('click', () => act('practice_resume', {}, 'Couldn’t continue'));
      parent.prepend(go);
    }, OFFER_MS);
  }

  function writing(v) {
    const box = h('section', 'exam-paper');
    box.append(h('p', 'help', 'The paper is being written. The clock starts when the whole paper is posed.'));
    if (!v.turnRunning && !v.lastTurnError) offerLater(box, 'Continue writing the paper');
    box.append(st.statusEl);
    return box;
  }

  function kicker(p, v) {
    let t = `Problem ${p.index + 1} of ${v.problems.length}`;
    if (p.marks) t += ` · ${plural(p.marks, 'mark')}`;
    return t;
  }

  function problemBlock(p, v, pending) {
    const block = h('section', `exercise problem${pending ? ' pending' : ''}`);
    block.dataset.problem = p.problemId;
    const k = h('h2', 'kicker', kicker(p, v));
    k.id = nextId('exam-prob');
    block.setAttribute('aria-labelledby', k.id);
    block.append(k, prose(p.statement, 'prose statement'));
    return block;
  }

  function openPaper(v) {
    const paper = h('div', 'exam-paper');
    const total = v.problems.reduce((n, p) => n + (p.marks ?? 0), 0);

    const bar = h('div', 'exam-bar');
    bar.setAttribute('role', 'group');
    bar.setAttribute('aria-label', 'Exam clock');
    const clock = h('span', 'exam-clock', clockText(v.exam.budgetSec));
    clock.setAttribute('role', 'timer');
    const label = h('span', 'exam-clock-label', `left of ${spokenBudget(v.exam.budgetSec)}${total ? ` · ${plural(total, 'mark')}` : ''}`);
    const hand = button('Hand in the paper', 'primary');
    hand.addEventListener('click', () => handIn());
    bar.append(clock, label, hand);
    st.clockEl = clock;
    st.barEl = bar;
    st.handBtn = hand;
    hand.disabled = !v.can.submitSet || st.acting;

    paper.append(bar);
    v.problems.forEach((p, i) => {
      if (!p.statement) return;
      const block = problemBlock(p, v, true);
      const form = h('div', 'ex-form');
      const draft = st.drafts.get(p.problemId);
      const field = h('div', 'field');
      const lab = h('label', '', `Your answer to problem ${p.index + 1}`);
      const ta = h('textarea', 'input');
      ta.id = nextId('exam-answer');
      ta.dataset.focus = `answer:${p.problemId}`;
      lab.htmlFor = ta.id;
      ta.rows = 5;
      ta.placeholder = 'Working and answer. Leave it empty if you don’t know.';
      ta.value = draft?.text ?? '';
      ta.addEventListener('input', () => changed(p.problemId, { text: ta.value }));
      if (i === 0) ta.dataset.first = '';
      field.append(lab, ta);
      const conf = confidenceField({ legend: 'How sure are you? (optional)' });
      conf.set(draft?.confidence ?? null);
      for (const r of conf.radios) r.addEventListener('change', () => changed(p.problemId, { confidence: conf.value() }));
      form.append(field, conf.el);
      block.append(form);
      paper.append(block);
    });
    const foot = h('div', 'exam-foot');
    const count = h('p', 'exam-count');
    st.saveEl = h('p', 'you-meta exam-saved');
    st.saveEl.setAttribute('role', 'status');
    foot.append(count, st.saveEl, st.statusEl);
    paper.append(foot);
    queueMicrotask(updateCount);
    return paper;
  }

  function spokenBudget(sec) {
    const m = Math.round(sec / 60);
    return m >= 90 && m % 60 === 0 ? plural(m / 60, 'hour') : plural(m, 'minute');
  }

  function reviewPaper(v) {
    const out = [];
    const ex = v.exam;
    const sum = h('section', 'exam-score');
    const title = h('h2', 'summary-title', 'Result');
    title.id = nextId('exam-result');
    title.tabIndex = -1;
    title.dataset.focus = 'result';
    sum.setAttribute('aria-labelledby', title.id);
    sum.append(title);
    if (ex.score) {
      sum.append(h('p', 'exam-score-line', `${ex.score.awarded} of ${ex.score.outOf} marks (${pct(ex.score.awarded / ex.score.outOf)})`));
    } else {
      sum.append(h('p', 'summary-line', 'The coach is marking the paper…'));
    }
    const when = ex.autoSubmitted
      ? 'Time ran out: the app handed in your last saved answers.'
      : ex.elapsedSec != null
        ? `Handed in after ${Math.max(1, Math.round(ex.elapsedSec / 60))} of ${spokenBudget(ex.budgetSec)}.`
        : 'Handed in.';
    sum.append(h('p', 'you-meta', when));
    out.push(sum);

    const answers = handedIn();
    for (const p of v.problems) {
      if (!p.statement) continue;
      const block = problemBlock(p, v, false);
      const a = answers.get(p.problemId);
      const you = h('div', 'you');
      you.append(h('p', 'you-label', 'You'), h('p', 'you-text', a?.text?.trim() ? a.text : 'No answer.'));
      if (typeof a?.confidence === 'number') you.append(h('p', 'you-meta', confidenceLabel(a.confidence)));
      block.append(you);
      block.append(p.result ? resultBlock(p) : h('p', 'writing', 'Marking…'));
      out.push(block);
    }
    for (const item of v.feed) {
      if (item.kind !== 'coach') continue;
      out.push(assistantEl({ content: item.text, thinking: item.thinking }, { showThinking: true, artifacts: [], ctx: ctx() }));
    }

    const unmarked = v.problems.some((p) => p.statement && !p.result);
    const actions = h('div', 'exam-actions');
    if (unmarked && !v.turnRunning && !v.lastTurnError && v.status !== 'ended') offerLater(actions, 'Continue the marking');
    if (v.status === 'ended') {
      const back = button('Back to sessions', 'primary');
      back.addEventListener('click', () => onBack());
      actions.append(back);
    } else {
      const end = button('Finish', 'primary');
      end.disabled = !v.can.end || st.acting;
      end.addEventListener('click', () => act('practice_end', {}, 'Couldn’t finish'));
      actions.append(end);
    }
    out.push(actions, st.statusEl);
    return out;
  }

  function resultBlock(p) {
    const r = p.result;
    const wrap = h('div', 'result');
    const [glyph, text] = VERDICTS[r.verdict] ?? ['•', r.verdict];
    const verdict = h('p', 'verdict');
    const g = h('span', 'glyph', `${glyph} `);
    g.setAttribute('aria-hidden', 'true');
    verdict.append(g, document.createTextNode(`${text}.`));
    wrap.append(verdict);
    if (p.marks != null) wrap.append(h('p', 'exam-marks', `${r.marksAwarded ?? 0} of ${plural(p.marks, 'mark')}`));
    const meta = [];
    if (r.targets?.length) meta.push(`Tested: ${r.targets.map((t) => t.title).join(', ')}`);
    meta.push(`Rated ${r.rating}`);
    wrap.append(h('p', 'you-meta result-meta', meta.join(' · ')));
    if (r.missClass && MISS[r.missClass]) wrap.append(h('p', 'result-miss', `What went wrong: ${MISS[r.missClass]}`));
    if (r.misconception) wrap.append(h('p', 'result-miss', `Misconception: ${r.misconception}`));
    wrap.append(revealEl(r.reveal, p));
    return wrap;
  }

  function revealEl(rev, p) {
    const d = h('details', 'reveal');
    d.open = true;
    d.append(h('summary', '', 'Mark scheme and worked solution'));
    const body = h('div', 'reveal-body');
    if (rev.rubric?.length) {
      body.append(h('p', 'reveal-label', 'Mark scheme'));
      const ul = h('ul', 'reveal-list');
      for (const c of rev.rubric) {
        const li = h('li');
        li.append(prose(c.criterion, 'prose'));
        if (c.essential) li.append(h('span', 'reveal-key', 'Method'));
        ul.append(li);
      }
      body.append(ul);
    }
    if (rev.solution?.length) {
      body.append(h('p', 'reveal-label', 'Worked solution'));
      const ol = h('ol', 'reveal-moves');
      for (const m of rev.solution) {
        const li = h('li');
        if (m.decisionPoint) li.append(h('span', 'reveal-key', 'Key step'));
        li.append(prose(m.move, 'prose'));
        ol.append(li);
      }
      body.append(ol);
    }
    if (rev.finalAnswer) body.append(h('p', 'reveal-label', 'Answer'), prose(rev.finalAnswer, 'prose'));
    if (p.answerCheck === 'skipped') body.append(h('p', 'reveal-note', 'Not checked by a script (Python with SymPy isn’t available here), so this answer is unverified.'));
    d.append(body);
    return d;
  }

  // ----- doing things -----

  async function handIn() {
    if (!st || st.acting || st.view.status !== 'set-open') return;
    const mine = st;
    const answers = draftAnswers();
    const empty = st.view.problems.length - answers.filter((a) => a.text.trim()).length;
    if (empty > 0 && remaining() > 0) {
      const ok = await confirmDialog({
        title: 'Hand in the paper?',
        text: `${plural(empty, 'problem')} ${empty === 1 ? 'has' : 'have'} no answer and will count as “don’t know”. You can’t reopen the paper.`,
        confirmLabel: 'Hand in',
        cancelLabel: 'Keep working',
      });
      if (!ok || st !== mine) return;
    }
    clearTimeout(mine.saveTimer);
    for (const a of answers) handedIn().set(a.problemId, { text: a.text, confidence: a.confidence ?? null });
    await act('practice_submit_set', { answers }, 'Couldn’t hand in the paper');
  }

  async function act(name, args, failure) {
    if (!st || st.acting) return;
    const mine = st;
    mine.acting = true;
    if (mine.handBtn) mine.handBtn.disabled = true;
    mine.statusEl.textContent = '';
    let failed = null;
    try {
      await cmd(name, { sessionId: mine.id, ...args });
    } catch (e) {
      failed = e;
    }
    if (st !== mine) return;
    mine.acting = false;
    await refresh();
    if (st !== mine) return;
    if (failed !== null) mine.statusEl.textContent = `${failure}: ${failed}`;
    if (mine.handBtn) mine.handBtn.disabled = !mine.view.can.submitSet || mine.expired;
  }

  return { update, destroy };
}
