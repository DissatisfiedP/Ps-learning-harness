// Learn-session blocks: the running head's phase position, question and
// choice blocks, section openers, the plan document (await approval), the
// plan sheet's contents, job lines, notices and dividers. Each builder returns
// a DOM element; callers own the backend calls through the handlers they pass.

import { artifactFigure } from './artifacts.js';
import { enhanceLive } from './content.js';
import { calibrationBlock, fieldGuide } from './fieldguide.js';
import { escapeHtml, renderMarkdown } from './markdown.js';
import { getPlugin } from './pluginstore.js';
import { attachPanZoom, planDiagram, progressOf, sectionOf } from './plan-diagram.js';

let uid = 0;
const nextId = (prefix) => `${prefix}-${++uid}`;

function h(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = text;
  return el;
}

function button(text, className, type = 'button') {
  const b = h('button', `btn ${className}`, text);
  b.type = type;
  return b;
}

function textareaField(label, { placeholder = '', rows = 3, value = '', className = '' } = {}) {
  const id = nextId('f');
  const wrap = h('div', 'field');
  const l = h('label', '', label);
  l.htmlFor = id;
  const ta = h('textarea', `input ${className}`.trim());
  ta.id = id;
  ta.rows = rows;
  ta.placeholder = placeholder;
  ta.value = value;
  wrap.append(l, ta);
  return { wrap, input: ta };
}

function prose(markdown, className = 'prose') {
  const div = h('div', className);
  div.innerHTML = renderMarkdown(markdown ?? '');
  enhanceLive(div);
  return div;
}

/** Status line for a form: neutral text, `--danger` for system failures. */
function statusLine() {
  const p = h('p', 'form-status');
  p.setAttribute('role', 'status');
  p.tabIndex = -1;
  const set = (text, error = false) => {
    p.textContent = text;
    p.classList.toggle('error', error);
  };
  return { el: p, set };
}

/** Ctrl/Cmd+Enter submits `form` from any of its fields. */
function submitOnModEnter(form) {
  form.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.isComposing) {
      e.preventDefault();
      form.requestSubmit();
    }
  });
}

/** A "You" note inside an answered block. Text with math is rendered as
 *  markdown (KaTeX), because answers and options carry formulas. */
const MATHY = /\$[^\s$]|\$\$|\\\(/;

function richLine(tag, cls, text) {
  const el = h(tag, cls);
  if (MATHY.test(text)) {
    el.innerHTML = renderMarkdown(text);
    enhanceLive(el);
  } else {
    el.textContent = text;
  }
  return el;
}

function youBlock(lines) {
  const you = h('div', 'you');
  you.append(h('p', 'you-label', 'You'));
  for (const [text, cls] of lines) if (text) you.append(richLine('p', cls, text));
  return you;
}

// ---------- running head: phase position ----------

const PHASES = ['measure', 'plan', 'teach', 'close'];

function phaseIndex(st) {
  if (st.ended || st.phase === 'close') return 3;
  if (st.phase === 'teach') return 2;
  if (st.phase === 'plan' || st.phase === 'awaitApproval') return 1;
  return 0;
}

/** Section position while teaching: `{ n, m }` (1-based), or null. With no
 *  section open the position is the next unfinished one — where teaching
 *  resumes — never a fake "last of all". */
export function sectionPosition(st) {
  const sections = st?.plan?.sections ?? [];
  if (!sections.length) return null;
  const done = new Set(st?.completedSections ?? []);
  let i = sections.findIndex((s) => s.id === st?.currentSection);
  if (i < 0) i = sections.findIndex((s) => !done.has(s.id));
  return { n: (i >= 0 ? i : sections.length - 1) + 1, m: sections.length };
}

function labelFor(phase, st, { planning, planFailed, current }) {
  switch (phase) {
    case 'measure':
      return 'Measure';
    case 'plan':
      if (!current) return 'Plan';
      if (planning) return 'Planning…';
      if (planFailed) return 'Plan · failed';
      return st.phase === 'awaitApproval' ? 'Plan · review' : 'Plan';
    case 'teach': {
      const pos = current ? sectionPosition(st) : null;
      const at = pos ? `Teach ${pos.n}/${pos.m}` : 'Teach';
      // The hunt is out: the head says so next to the section, nothing more
      // (ui-design §7.7.4 — no reminder, no timer).
      return current && st.hunt?.state === 'out' ? `${at} · Hunt` : at;
    }
    default:
      return 'Close';
  }
}

/** Where a Learn session stands, in the head's words ("Teach 2/5", "Plan · review"). */
export function placeText(st) {
  return labelFor(PHASES[phaseIndex(st)], st, { current: true });
}

/**
 * Updates `nav` (the head's "Session progress") in place: an ordered list of
 * the four phases, plus a status that says "Planning…" once when the planner
 * starts. `error` replaces the list with "Status unavailable".
 */
export function renderPhase(nav, st, { planning = false, planFailed = false, error = null } = {}) {
  let ol = nav.querySelector('ol');
  let live = nav.querySelector('.phase-live');
  let unavailable = nav.querySelector('.phase-error');
  if (!ol) {
    ol = h('ol');
    for (const id of PHASES) {
      const li = h('li');
      li.dataset.phase = id;
      ol.append(li);
    }
    live = h('span', 'sr-only phase-live');
    live.setAttribute('role', 'status');
    unavailable = h('span', 'phase-error');
    unavailable.hidden = true;
    nav.replaceChildren(ol, unavailable, live);
  }
  ol.hidden = Boolean(error);
  unavailable.hidden = !error;
  if (error) {
    unavailable.innerHTML = `Status unavailable<span class="sr-only">: ${escapeHtml(error)}</span>`;
    return;
  }
  const current = phaseIndex(st);
  [...ol.children].forEach((li, i) => {
    li.className = i < current ? 'past' : i === current ? 'current' : 'future';
    li.textContent = labelFor(PHASES[i], st, { planning, planFailed, current: i === current });
    if (i === current) li.setAttribute('aria-current', 'step');
    else li.removeAttribute('aria-current');
  });
  const say = planning ? 'Planning…' : '';
  if (live.textContent !== say) live.textContent = say;
}

// ---------- question block ----------

const ROLE_LABEL = { probe: 'Quick question', predict: 'Predict first', check: 'Check your understanding' };
const VERDICT = {
  correct: { glyph: '✓', text: 'Correct' },
  partial: { glyph: '◐', text: 'Partly right' },
  wrong: { glyph: '✗', text: 'Not quite' },
};
const CONFIDENCE = [
  [10, 'Guessing'],
  [30, 'Unsure'],
  [50, 'Even odds'],
  [70, 'Fairly sure'],
  [90, 'Certain'],
];

export const verdictText = (verdict) => VERDICT[verdict]?.text ?? verdict;

function confidenceLine(a) {
  // With no typed answer the answer line already says "I don't know."
  if (a.dontKnow) return a.answer ? 'I don’t know' : '';
  if (typeof a.confidence !== 'number') return '';
  const label = CONFIDENCE.find(([v]) => v === a.confidence)?.[1];
  return label ? `${label} (≈${a.confidence}%)` : `${a.confidence}% sure`;
}

/** Container shared by pending questions and choices. */
function exerciseShell(kicker, pending) {
  const block = h('section', `exercise${pending ? ' pending' : ''}`);
  const id = nextId('ex');
  const k = h('h2', 'kicker', kicker);
  k.id = id;
  block.setAttribute('aria-labelledby', id);
  block.append(k);
  return block;
}

// The verdict alone: the learner's answer and confidence sit right above it,
// and the tutor's reaction follows in its own message. The `verdict_record`
// note is the tutor's diagnosis for the planner and later turns, not shown.
function verdictLine(item) {
  const v = VERDICT[item.verdict];
  const p = h('p', 'verdict');
  p.tabIndex = -1;
  p.dataset.refocus = '';
  const glyph = h('span', 'glyph', `${v?.glyph ?? '•'} `);
  glyph.setAttribute('aria-hidden', 'true');
  p.append(glyph, document.createTextNode(`${v?.text ?? item.verdict}.`));
  return p;
}

function questionForm(item, onAnswer) {
  const args = item.args;
  const form = h('form', 'ex-form');
  form.noValidate = true;
  let answerInput = null;
  const options = [];
  if (args.format === 'choice' && Array.isArray(args.options)) {
    const fs = h('fieldset', 'rows');
    fs.append(h('legend', 'sr-only', 'Choose one'));
    const name = nextId('opt');
    for (const opt of args.options) {
      const label = h('label', 'row');
      const r = h('input');
      r.type = 'radio';
      r.name = name;
      r.value = opt;
      label.append(r, richLine('span', 'row-text', opt));
      fs.append(label);
      options.push(r);
    }
    options[0]?.setAttribute('data-first', '');
    form.append(fs);
  } else {
    const f = textareaField('Your answer', { rows: 3, placeholder: 'Your best guess is fine.' });
    answerInput = f.input;
    answerInput.dataset.first = '';
    form.append(f.wrap);
  }
  const thinking = textareaField('Your reasoning (optional)', { rows: 2, placeholder: 'How did you get there?' });
  form.append(thinking.wrap);

  const conf = h('fieldset', 'confidence');
  conf.append(h('legend', '', 'How sure are you?'));
  const confName = nextId('conf');
  const confRadios = [];
  for (const [value, label] of CONFIDENCE) {
    const l = h('label', 'seg');
    const r = h('input');
    r.type = 'radio';
    r.name = confName;
    r.value = String(value);
    const text = h('span', 'seg-text');
    text.append(h('span', 'seg-label', label), h('span', 'seg-pct', `≈${value}%`));
    l.append(r, text);
    conf.append(l);
    confRadios.push(r);
  }
  form.append(conf);

  const actions = h('div', 'actions');
  const submit = button('Submit', 'primary', 'submit');
  const idk = button('I don’t know — show me', 'text');
  actions.append(submit, idk);
  const status = statusLine();
  form.append(actions, status.el);

  const controls = () => [...form.querySelectorAll('input, textarea, button')];
  const send = async (dontKnow) => {
    const answer = answerInput ? answerInput.value.trim() : (options.find((r) => r.checked)?.value ?? '');
    const chosen = confRadios.find((r) => r.checked);
    if (!dontKnow) {
      if (!answer) {
        status.set('Write an answer or pick “I don’t know”.');
        (answerInput ?? options[0])?.focus({ preventScroll: true });
        return;
      }
      if (!chosen) {
        status.set('How sure are you? Pick one.');
        confRadios[0].focus({ preventScroll: true });
        return;
      }
    }
    const payload = { answer, thinking: thinking.input.value.trim(), dontKnow };
    if (!dontKnow) payload.confidence = Number(chosen.value);
    controls().forEach((c) => (c.disabled = true));
    status.set('Sending…');
    try {
      await onAnswer(item.callId, payload);
      status.set('Sent');
      status.el.focus({ preventScroll: true });
    } catch (err) {
      controls().forEach((c) => (c.disabled = false));
      status.set(String(err), true);
    }
  };
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    send(false);
  });
  idk.addEventListener('click', () => send(true));
  submitOnModEnter(form);
  return form;
}

/** The plugin's view as the answer surface of a pending question. */
function viewAnswer(item, { onAnswer, ctx }) {
  const view = item.args.view;
  const figure = h('div', 'view-answer-figure');
  const artifact = ctx?.artifacts?.get(view.artifactId);
  if (!artifact) {
    figure.append(h('p', 'waiting', 'Figure unavailable on this device.'));
    return figure;
  }
  figure.append(
    artifactFigure(artifact, view.task || 'Interactive figure', {
      ...ctx,
      answer: true,
      onSubmit: (answer) => onAnswer(item.callId, { answer }),
    }),
  );
  const plugin = getPlugin(artifact.pluginId);
  if (plugin && plugin.onPlatform === false) {
    figure.append(h('p', 'waiting', 'This figure’s plugin doesn’t run on this device.'));
  } else if (!plugin?.hasView || plugin.enabled === false) {
    // The fallback picture can't take an answer; say why instead of waiting silently.
    figure.append(h('p', 'waiting', 'Plugin missing or off. Fix in Settings → Plugins.'));
  }
  return figure;
}

function viewAnswerSummary(answer) {
  return typeof answer === 'string' && answer.trim() ? answer : 'Answer submitted in the figure.';
}

/** Block for a `question_ask` call. Never collapsed while pending. */
export function questionCard(item, { onAnswer, readOnly, ctx = {} }) {
  if (item.failed) return hiddenItem();
  const args = item.args;
  const pending = !item.answer && !readOnly;
  const block = exerciseShell(ROLE_LABEL[args.role] ?? 'Question', pending);
  block.append(prose(args.prompt, 'prose prompt'));
  if (args.view) {
    block.append(h('p', 'view-task', args.view.task));
    if (item.answer) {
      // The plugin's grade goes to the tutor only: its detail can name the
      // target, which the learner never sees (W7). The verdict follows.
      block.append(youBlock([[viewAnswerSummary(item.answer.answer), 'you-text']]));
      if (item.verdict) {
        block.append(verdictLine(item));
        if (args.explanation) block.append(prose(args.explanation, 'prose feedback'));
      } else {
        const wait = h('p', 'waiting', 'Waiting for feedback.');
        wait.tabIndex = -1;
        wait.dataset.refocus = '';
        block.append(wait);
      }
    } else if (readOnly) {
      block.append(h('p', 'waiting', 'Not answered.'));
    } else {
      block.append(viewAnswer(item, { onAnswer, ctx }));
    }
    return block;
  }
  if (item.answer) {
    const a = item.answer;
    const answerText = a.answer || (a.dontKnow ? 'I don’t know.' : '');
    block.append(
      youBlock([
        [answerText, 'you-text'],
        [a.thinking ? `Reasoning: ${a.thinking}` : '', 'you-meta'],
        [confidenceLine(a), 'you-meta'],
      ]),
    );
    if (item.verdict) {
      block.append(verdictLine(item));
      if (args.explanation) block.append(prose(args.explanation, 'prose feedback'));
    } else {
      const wait = h('p', 'waiting', 'Waiting for feedback.');
      wait.tabIndex = -1;
      wait.dataset.refocus = '';
      block.append(wait);
    }
  } else if (readOnly) {
    block.append(h('p', 'waiting', 'Not answered.'));
  } else {
    block.append(questionForm(item, onAnswer));
  }
  return block;
}

function hiddenItem() {
  const el = h('div', 'hidden-item');
  el.hidden = true;
  return el;
}

// ---------- choice block ----------

/** Block for a `choice_ask` call. */
export function choiceCard(item, { onAnswer, readOnly }) {
  if (item.failed) return hiddenItem();
  const args = item.args;
  const pending = !item.answer && !readOnly;
  const block = exerciseShell('Your choice', pending);
  block.append(prose(args.question, 'prose prompt'));
  if (item.answer) {
    const you = youBlock([[item.answer.answer ?? '', 'you-text']]);
    you.tabIndex = -1;
    you.dataset.refocus = '';
    block.append(you);
    return block;
  }
  if (readOnly) {
    block.append(h('p', 'waiting', 'Not answered.'));
    return block;
  }
  const form = h('form', 'ex-form');
  form.noValidate = true;
  const fs = h('fieldset', 'rows');
  fs.append(h('legend', 'sr-only', 'Choose one'));
  const name = nextId('choice');
  const radios = [];
  for (const opt of args.options ?? []) {
    const label = h('label', 'row');
    const r = h('input');
    r.type = 'radio';
    r.name = name;
    r.value = opt;
    label.append(r, h('span', '', opt));
    if (args.recommended && opt === args.recommended) label.append(h('span', 'suggested', '(suggested)'));
    fs.append(label);
    radios.push(r);
  }
  const otherRow = h('div', 'row other');
  const otherLabel = h('label', 'other-pick');
  const otherRadio = h('input');
  otherRadio.type = 'radio';
  otherRadio.name = name;
  otherRadio.value = '';
  otherLabel.append(otherRadio, h('span', '', 'Other'));
  const otherId = nextId('other');
  const otherTextLabel = h('label', 'sr-only', 'Other answer');
  otherTextLabel.htmlFor = otherId;
  const otherText = h('input', 'input');
  otherText.type = 'text';
  otherText.id = otherId;
  otherText.placeholder = 'Something else…';
  otherText.addEventListener('input', () => (otherRadio.checked = Boolean(otherText.value) || otherRadio.checked));
  otherRadio.addEventListener('change', () => otherRadio.checked && otherText.focus({ preventScroll: true }));
  otherRow.append(otherLabel, otherTextLabel, otherText);
  fs.append(otherRow);
  (radios[0] ?? otherRadio).dataset.first = '';
  form.append(fs);

  const actions = h('div', 'actions');
  actions.append(button('Continue', 'primary', 'submit'));
  const status = statusLine();
  form.append(actions, status.el);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const answer = otherRadio.checked ? otherText.value.trim() : (radios.find((r) => r.checked)?.value ?? '');
    if (!answer) {
      status.set(otherRadio.checked ? 'Write your own answer.' : 'Pick an option.');
      (otherRadio.checked ? otherText : radios[0] ?? otherRadio).focus({ preventScroll: true });
      return;
    }
    const controls = [...form.querySelectorAll('input, button')];
    controls.forEach((c) => (c.disabled = true));
    status.set('Sending…');
    try {
      await onAnswer(item.callId, { answer });
      status.set('Sent');
      status.el.focus({ preventScroll: true });
    } catch (err) {
      controls.forEach((c) => (c.disabled = false));
      status.set(String(err), true);
    }
  });
  submitOnModEnter(form);
  block.append(form);
  return block;
}

// ---------- section openers ----------

/** Title of a section opener, for the announcer. */
export function sectionTitle(item, fallbackPlan) {
  if (item.close) return 'Wrapping up';
  const plan = item.plan ?? fallbackPlan;
  return plan?.sections?.find((s) => s.id === item.sectionId)?.title ?? 'New section';
}

/** "Section 2 of 5 / The power rule / Goal: …" — or "Wrapping up". */
export function sectionOpener(item, fallbackPlan) {
  const el = h('div', 'opener');
  const plan = item.plan ?? fallbackPlan;
  const sections = plan?.sections ?? [];
  const i = item.close ? -1 : sections.findIndex((s) => s.id === item.sectionId);
  if (i >= 0) {
    const known = sections[i].assumedKnown ? ' · Quick check' : '';
    el.append(h('p', 'small-caps opener-kicker', `Section ${i + 1} of ${sections.length}${known}`));
  }
  el.append(h('h2', 'opener-title', sectionTitle(item, fallbackPlan)));
  const exit = i >= 0 ? sections[i].exit : null;
  if (exit) el.append(h('p', 'opener-goal', `Goal: ${exit}`));
  return el;
}

// ---------- plan ----------

// The four levels, in the learner's words (docs/learning-system.md §2).
const LEVELS = [
  ['explain', 'Explain', 'Say it, and why it holds'],
  ['use', 'Use', 'Apply it to a new case'],
  ['analyze', 'Analyze', 'Take it apart, diagnose, judge'],
  ['design', 'Design', 'Build something that meets constraints'],
];
const LEVEL_LABEL = new Map(LEVELS.map(([id, label]) => [id, label]));
const levelLabel = (id) => (id ? LEVEL_LABEL.get(id) ?? capitalize(id) : '');

const DEPTH_CHOICES = [
  ['pass', 'Just pass'],
  ['solid', 'Solid'],
  ['deep', 'Deep'],
];
const EXPLORE_CHOICES = [
  ['off', 'Off'],
  ['suggest', 'Suggest'],
  ['on', 'On'],
];
// The plan's style vocabulary is the section's (`socratic` / `expository`);
// the labels are the learner's.
const STYLE_CHOICES = [
  ['socratic', 'Let me reason it out'],
  ['expository', 'Explain it to me'],
];

/**
 * The learner's unsent plan edits. Held by the session (`s.planEdits`) so a
 * re-render keeps them; `open` is which "⋯" disclosure is showing, `drawer` the
 * section whose side drawer is open (a card rebuilt underneath it keeps it),
 * `answered` the planner's question once answered, `hunts` the local view of a
 * hunt toggle whose `set_hunt` call is in flight.
 */
export function newPlanEdits() {
  return { open: new Set(), drawer: null, plan: {}, sections: {}, comment: '', answered: null, hunts: {} };
}

const planFields = ['depth', 'explore', 'minutesPerSitting', 'style'];
const sectionFields = ['targetLevel', 'keep', 'knownNote', 'learnerNote', 'plugins'];

/** Whether anything is waiting to be sent. A comment alone is not an edit. */
function hasPlanEdits(edits) {
  if (!edits) return false;
  if (planFields.some((k) => edits.plan?.[k] !== undefined)) return true;
  return Object.values(edits.sections ?? {}).some((e) => sectionFields.some((k) => e[k] !== undefined));
}

/**
 * The `edit_plan` payload: plan-level fields at the top, then one entry per
 * edited section, absent fields omitted —
 * `{ depth?, explore?, minutesPerSitting?, style?, sections?: [{ sectionId,
 * targetLevel?, keep?, knownNote?, learnerNote?, plugins? }] }`.
 * `sectionId` is what `agent.rs::describe_plan_edits` reads (`id` would also
 * be accepted); `plugins` is the list the learner kept for that section and is
 * only present when they touched its tools. The backend refuses an empty
 * object, which `hasPlanEdits` above prevents.
 */
function planEditsPayload(edits) {
  const out = {};
  for (const k of planFields) if (edits.plan?.[k] !== undefined) out[k] = edits.plan[k];
  const sections = [];
  for (const [sectionId, edit] of Object.entries(edits.sections ?? {})) {
    const item = { sectionId };
    for (const k of sectionFields) if (edit[k] !== undefined) item[k] = edit[k];
    if (Object.keys(item).length > 1) sections.push(item);
  }
  if (sections.length) out.sections = sections;
  return out;
}

/**
 * The words a section's box carries (ui-design §7.7.1, §7.7.3): its state in
 * one line of short labels, never a paragraph — "Current", "Done", "You know
 * this", the aim (or the learner's own "Analyze only" / "Not needed"), "Hunt
 * on", "Needs install". Defaults that the learner didn't choose are not
 * summarized.
 */
function sectionBadges(section, edit, { status = 'open', known = false } = {}) {
  const badges = [];
  if (status === 'done') badges.push('Done');
  else if (status === 'current') badges.push('Current');
  if (known && status !== 'done') badges.push('You know this');
  if (edit?.keep === false) {
    badges.push('Not needed');
  } else {
    const level = edit?.targetLevel ?? section.targetLevel;
    if (level) badges.push(edit?.targetLevel ? `${levelLabel(level)} only` : `aim: ${levelLabel(level)}`);
  }
  const huntOn = edit?.hunt === undefined ? Boolean(section.hunt?.enabled) : edit.hunt;
  if (edit?.keep !== false && section.hunt?.available && huntOn) badges.push('Hunt on');
  if ((section.needsInstall ?? []).length) badges.push('Needs install');
  return badges;
}

/** A "⋯" disclosure: native controls, inline under its row, hidden by default. */
function optionsDisclosure({ label, open, onToggle, controls }) {
  const wrap = h('div', 'plan-options');
  const id = nextId('opt');
  const toggle = button('⋯', 'text plan-options-toggle');
  toggle.setAttribute('aria-label', label);
  toggle.setAttribute('aria-expanded', String(Boolean(open)));
  toggle.setAttribute('aria-controls', id);
  const panel = h('div', 'plan-options-panel');
  panel.id = id;
  panel.hidden = !open;
  panel.append(...controls);
  toggle.addEventListener('click', () => {
    const next = panel.hidden;
    panel.hidden = !next;
    toggle.setAttribute('aria-expanded', String(next));
    onToggle(next);
    if (next) panel.querySelector('input, textarea, button')?.focus({ preventScroll: true });
  });
  wrap.append(toggle, panel);
  return { wrap, panel };
}

/** A labelled row of native radios: `[[value, label, hint?]]`. */
function choiceRows(label, name, options, value, onPick) {
  const fs = h('fieldset', 'rows plan-choice');
  fs.append(h('legend', '', label));
  for (const [v, text, hint] of options) {
    const row = h('label', 'row');
    const r = h('input');
    r.type = 'radio';
    r.name = name;
    r.value = v;
    r.checked = value === v;
    r.addEventListener('change', () => r.checked && onPick(v));
    const t = h('span', 'row-text');
    t.append(h('span', 'row-label', text));
    if (hint) t.append(h('span', 'row-hint', hint));
    row.append(r, t);
    fs.append(row);
  }
  return fs;
}

/** Sitting length in minutes; empty or below 1 means "no preference". */
function minutesField(value, onInput) {
  const id = nextId('min');
  const wrap = h('div', 'field plan-minutes');
  const label = h('label', '', 'Sitting length');
  label.htmlFor = id;
  const input = h('input', 'input');
  input.type = 'number';
  input.id = id;
  input.min = '5';
  input.max = '240';
  input.step = '5';
  input.inputMode = 'numeric';
  input.placeholder = 'minutes';
  if (typeof value === 'number') input.value = String(value);
  input.addEventListener('input', () => {
    const n = Number(input.value);
    onInput(Number.isFinite(n) && n >= 1 ? Math.round(n) : undefined);
  });
  wrap.append(label, input);
  return wrap;
}

const capitalize = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);

function changeForm({ draft, onDraft, onRequest, onCancel }) {
  const form = h('form', 'change-form');
  form.noValidate = true;
  const f = textareaField('What should change, and why?', { rows: 3, value: draft ?? '', className: 'resizable' });
  f.input.addEventListener('input', () => onDraft?.(f.input.value));
  form.append(f.wrap);
  const actions = h('div', 'actions');
  const send = button('Send to planner', 'secondary', 'submit');
  const cancel = button('Cancel', 'text');
  cancel.addEventListener('click', onCancel);
  actions.append(send, cancel);
  const status = statusLine();
  form.append(actions, status.el);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const text = f.input.value.trim();
    if (!text) {
      status.set('Describe the change first.');
      f.input.focus({ preventScroll: true });
      return;
    }
    send.disabled = true;
    status.set('Sending…');
    try {
      await onRequest(text);
      f.input.value = '';
      onDraft?.('');
      status.set('Sent. Revising the plan…');
    } catch (err) {
      status.set(String(err), true);
    } finally {
      send.disabled = false;
    }
  });
  submitOnModEnter(form);
  return { form, input: f.input };
}

/** A change form behind a reveal button ("Suggest a change" / "Change plan"). */
function changeDisclosure(revealText, revealClass, { formOpen, draft, onDraft, onFormOpen, onRequest }) {
  const wrap = h('div', 'change');
  const reveal = button(revealText, revealClass);
  const { form, input } = changeForm({
    draft,
    onDraft,
    onRequest,
    onCancel: () => {
      setOpen(false);
      reveal.focus({ preventScroll: true });
    },
  });
  const setOpen = (open) => {
    form.hidden = !open;
    reveal.setAttribute('aria-expanded', String(open));
    onFormOpen?.(open);
  };
  reveal.addEventListener('click', () => {
    const open = form.hidden;
    setOpen(open);
    if (open) input.focus({ preventScroll: true });
  });
  form.hidden = !formOpen;
  reveal.setAttribute('aria-expanded', String(Boolean(formOpen)));
  return { wrap, reveal, form };
}

function coversLine(section, nodes) {
  const names = (section.nodeIds ?? []).map((id) => {
    const n = nodes.get(id);
    if (!n) return id;
    return n.kind ? `${n.title ?? id} (${n.kind})` : (n.title ?? id);
  });
  return names.length ? `Covers: ${names.join(', ')}` : '';
}

/**
 * The planner's one question on the card (`plan_ask`, ui-design §7.7.3): the
 * same markup as a choice block, with an "other" field, above the sections.
 * Answering locks the block until the next plan version replaces the card.
 */
function plannerQuestionBlock(question, edits, onAnswerQuestion) {
  const answered = edits.answered;
  const block = exerciseShell('The planner asks', answered == null);
  block.append(prose(question.question, 'prose prompt'));
  if (answered != null) {
    const you = youBlock([[answered, 'you-text']]);
    you.tabIndex = -1;
    you.dataset.refocus = '';
    block.append(you, h('p', 'waiting', 'Waiting for the planner.'));
    return block;
  }
  const form = h('form', 'ex-form');
  form.noValidate = true;
  const fs = h('fieldset', 'rows');
  fs.append(h('legend', 'sr-only', 'Choose one'));
  const name = nextId('ask');
  const radios = [];
  for (const opt of question.options ?? []) {
    const label = h('label', 'row');
    const r = h('input');
    r.type = 'radio';
    r.name = name;
    r.value = opt;
    label.append(r, h('span', 'row-text', opt));
    fs.append(label);
    radios.push(r);
  }
  const otherRow = h('div', 'row other');
  const otherLabel = h('label', 'other-pick');
  const otherRadio = h('input');
  otherRadio.type = 'radio';
  otherRadio.name = name;
  otherRadio.value = '';
  otherLabel.append(otherRadio, h('span', '', 'Other'));
  const otherId = nextId('other');
  const otherTextLabel = h('label', 'sr-only', 'Other answer');
  otherTextLabel.htmlFor = otherId;
  const otherText = h('input', 'input');
  otherText.type = 'text';
  otherText.id = otherId;
  otherText.placeholder = 'Something else…';
  otherText.addEventListener('input', () => (otherRadio.checked = Boolean(otherText.value) || otherRadio.checked));
  otherRadio.addEventListener('change', () => otherRadio.checked && otherText.focus({ preventScroll: true }));
  otherRow.append(otherLabel, otherTextLabel, otherText);
  fs.append(otherRow);
  (radios[0] ?? otherRadio).dataset.first = '';
  form.append(fs);

  const actions = h('div', 'actions');
  actions.append(button('Answer', 'primary', 'submit'));
  const status = statusLine();
  form.append(actions, status.el);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const answer = otherRadio.checked ? otherText.value.trim() : (radios.find((r) => r.checked)?.value ?? '');
    if (!answer) {
      status.set(otherRadio.checked ? 'Write your own answer.' : 'Pick an option.');
      (otherRadio.checked ? otherText : radios[0] ?? otherRadio).focus({ preventScroll: true });
      return;
    }
    const controls = [...form.querySelectorAll('input, button')];
    controls.forEach((c) => (c.disabled = true));
    status.set('Sending…');
    try {
      await onAnswerQuestion(answer);
      edits.answered = answer;
      // The card keeps the answer until the revised plan replaces it.
      const locked = plannerQuestionBlock(question, edits, onAnswerQuestion);
      block.replaceWith(locked);
      locked.querySelector('[data-refocus]')?.focus({ preventScroll: true });
    } catch (err) {
      controls.forEach((c) => (c.disabled = false));
      status.set(String(err), true);
    }
  });
  submitOnModEnter(form);
  block.append(form);
  return block;
}

/**
 * The plan card, flowchart first (ui-design §7.2.9, §7.7.1–§7.7.3). Owner
 * direction, 2026-11-07: the diagram *is* the card — the long reading list of
 * editable section blocks is gone. Each box carries its section's state as a
 * line of badges and a "⋯" inside its own corner; that opens the side drawer
 * with the section's description and the actions (How far, Include, the parts
 * already known, Tools, Hunt, a note). Every action writes into `edits`, sent
 * as one `edit_plan` by "Send changes"; the hunt toggle is its own `set_hunt`;
 * Approve is disabled while anything is pending.
 */
export function planCard(plan, {
  edits,
  onSendEdits,
  onDiscard,
  onHuntToggle,
  question = null,
  onAnswerQuestion,
  knownRefuted = [],
  draft,
  formOpen,
  onDraft,
  onFormOpen,
  onApprove,
  onRequest,
  onExpand = null,
  installed = new Set(),
  labels = new Map(),
  busy = false,
  onInstall = null,
  state = null,
}) {
  const doc = h('section', 'plan-doc');
  const headId = nextId('plan');
  doc.setAttribute('aria-labelledby', headId);
  doc.append(h('p', 'small-caps', 'Proposed plan'));

  // The corner control: the same drawing, the whole screen (§7.2.9 overlay).
  const expand = button('Expand', 'text plan-expand');
  expand.setAttribute('aria-label', 'Expand the plan to the full screen');
  expand.title = 'Expand to full screen';
  expand.addEventListener('click', () => onExpand?.());
  doc.append(expand);

  const title = h('h2', 'plan-title', plan.goal || 'Proposed plan');
  title.id = headId;
  title.tabIndex = -1;

  const refuted = new Set(knownRefuted ?? []);
  const sections = plan.sections ?? [];
  const nodes = new Map((plan.nodes ?? []).map((n) => [n.id, n]));
  const sectionById = new Map(sections.map((s) => [s.id, s]));
  const { sectionStatus, nodeStatus } = progressOf(state);
  const drawerId = nextId('drawer');
  let openSectionId = edits.drawer ?? null;

  const noteField = textareaField('Comment for the planner', {
    rows: 2,
    placeholder: 'Optional.',
    value: edits.comment ?? '',
  });
  noteField.input.addEventListener('input', () => {
    edits.comment = noteField.input.value;
    refresh();
  });

  const actions = h('div', 'actions');
  const approve = button('Approve plan', 'primary');
  // One primary action at a time: "Send changes" carries the click while edits
  // are pending, and Approve stays visible but disabled until they are gone.
  const send = button('Send changes', 'primary');
  send.hidden = true;
  const discard = button('Discard', 'text');
  discard.hidden = true;
  const change = changeDisclosure('Suggest a change', 'text', { formOpen, draft, onDraft, onFormOpen, onRequest });
  actions.append(approve, send, discard, change.reveal);
  const status = statusLine();
  status.el.id = 'plan-edit-status';
  approve.setAttribute('aria-describedby', 'plan-edit-status');
  send.setAttribute('aria-describedby', 'plan-edit-status');

  const questionPending = Boolean(question) && edits.answered == null;


  // ---- plan-level controls ----
  const setPlan = (key, value) => {
    if (value === undefined || value === null) delete edits.plan[key];
    else edits.plan[key] = value;
    refresh();
  };
  const planControls = optionsDisclosure({
    label: 'Plan options',
    open: edits.open.has('plan'),
    onToggle: (open) => (open ? edits.open.add('plan') : edits.open.delete('plan')),
    controls: [
      choiceRows('Depth', 'plan-depth', DEPTH_CHOICES, edits.plan.depth ?? plan.depth ?? '', (v) => setPlan('depth', v)),
      choiceRows('Explore', 'plan-explore', EXPLORE_CHOICES, edits.plan.explore ?? plan.explore ?? '', (v) => setPlan('explore', v)),
      minutesField(edits.plan.minutesPerSitting ?? plan.minutesPerSitting, (v) => setPlan('minutesPerSitting', v)),
      choiceRows('Style', 'plan-style', STYLE_CHOICES, edits.plan.style ?? plan.style ?? '', (v) => setPlan('style', v)),
    ],
  });

  const headRow = h('div', 'plan-row');
  headRow.append(title, planControls.wrap);
  doc.append(headRow, planControls.panel);
  if (plan.approach) doc.append(prose(plan.approach));
  if (question) doc.append(plannerQuestionBlock(question, edits, onAnswerQuestion));

  // ---- the drawing: the card's primary surface ----
  const fig = h('figure', 'plan-figure plan-figure-topics');
  const frame = h('div', 'plan-diagram-frame');
  fig.append(
    frame,
    h(
      'figcaption',
      'plan-diagram-caption',
      'One box per topic; an arrow joins two topics wherever an idea needs an idea from the other. Choose ⋯ in a box for its details and actions.',
    ),
  );
  fig.hidden = !sections.length;
  doc.append(fig);

  // ---- the drawer: one section's long description and its actions ----
  const drawer = h('aside', 'plan-node-drawer');
  drawer.id = drawerId;
  drawer.hidden = true;
  drawer.tabIndex = -1;
  doc.append(drawer);

  /**
   * What a box draws beyond its title: its section's state in words, and the
   * "⋯" that opens this drawer (plan-diagram.js places and announces it).
   * Nothing is "current" while the plan is still a proposal.
   */
  function annotate(id) {
    const section = sectionById.get(id);
    if (!section) return null;
    const known = Boolean(section.assumedKnown) && !refuted.has(id);
    return {
      known,
      badges: sectionBadges(section, edits.sections[id] ?? {}, { status: boxStatus(id), known }),
      action: {
        label: `Details and options for ${section.title ?? id}`,
        controls: drawerId,
        expanded: openSectionId === id,
      },
    };
  }

  let drawnWidth = 0;
  // Nothing is "current" or "done" while the plan is still a proposal: the
  // boxes show the plan's own state, not where teaching would start.
  const teaching = Boolean(state) && state.phase !== 'awaitApproval' && state.phase !== 'plan' && state.phase !== 'measure';
  const boxStatus = (id) => (teaching ? sectionStatus(id) : 'open');

  /** Lays the diagram out at the card's own width and wires its "⋯"s. */
  function draw() {
    // The frame's inline padding and a scrollbar leave the drawing a few px
    // inside it; laying out at that width keeps every label at its own size.
    const width = Math.max(320, Math.round(frame.clientWidth || drawnWidth || 560) - 16);
    const { svg, width: artW, height: artH } = planDiagram(plan, {
      level: 'topics',
      // The card's drawing is the page there, so its boxes are drawn larger
      // than the sheet's overview (ui-design §7.2.9).
      scale: 1.3,
      maxW: width,
      sectionStatus: boxStatus,
      nodeStatus,
      annotate,
    });
    svg.setAttribute('width', String(Math.round(artW)));
    svg.setAttribute('height', String(Math.round(artH)));
    frame.replaceChildren(svg);
    for (const act of svg.querySelectorAll('.plan-node-action')) wireAction(act);
    drawnWidth = Math.round(frame.clientWidth || width);
  }

  if (typeof ResizeObserver === 'function' && sections.length) {
    // The column's measure follows the window (styles.css, §5.4), so the
    // drawing is laid out again when the card actually changes width.
    const ro = new ResizeObserver(() => {
      if (!frame.isConnected) {
        ro.disconnect();
        return;
      }
      const w = Math.round(frame.clientWidth);
      if (!w || Math.abs(w - drawnWidth) < 24) return;
      draw();
    });
    ro.observe(frame);
  }

  const actionEl = (id) =>
    [...doc.querySelectorAll('.plan-node-action')].find((a) => a.dataset.sectionId === id) ?? null;

  /** `aria-expanded` follows the one open drawer. */
  function syncExpanded() {
    for (const act of doc.querySelectorAll('.plan-node-action')) {
      act.setAttribute('aria-expanded', String(act.dataset.sectionId === openSectionId));
    }
  }

  function closeDrawer({ restore = false } = {}) {
    if (openSectionId === null) return;
    const opener = restore ? actionEl(openSectionId) : null;
    openSectionId = null;
    edits.drawer = null;
    drawer.hidden = true;
    drawer.replaceChildren();
    drawer.removeAttribute('aria-labelledby');
    syncExpanded();
    opener?.focus?.({ preventScroll: true });
  }

  function openDrawer(id) {
    if (openSectionId === id) {
      closeDrawer({ restore: true });
      return;
    }
    const section = sectionById.get(id);
    if (!section) return;
    openSectionId = id;
    edits.drawer = id;
    fillDrawer(section);
    drawer.hidden = false;
    syncExpanded();
    // Focus lands in the drawer but is never trapped there (§9.1): Tab walks
    // out the way it came and Escape puts focus back on the box's "⋯".
    drawer.querySelector('.plan-node-drawer-title')?.focus({ preventScroll: true });
  }

  function wireAction(act) {
    const id = act.dataset.sectionId;
    act.addEventListener('click', () => openDrawer(id));
    act.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
        e.preventDefault();
        openDrawer(id);
      }
    });
  }

  /** Everything the old card's section block said, and every control it had. */
  function fillDrawer(section) {
    const edit = (edits.sections[section.id] ??= {});
    const head = h('div', 'plan-node-drawer-head');
    const heading = h('h3', 'plan-node-drawer-title', section.title ?? section.id);
    heading.id = `${drawerId}-title`;
    heading.tabIndex = -1;
    drawer.setAttribute('aria-labelledby', heading.id);
    const close = button('Close', 'text plan-node-drawer-close');
    close.setAttribute('aria-label', 'Close section details');
    close.addEventListener('click', () => closeDrawer({ restore: true }));
    head.append(heading, close);

    const body = h('div', 'plan-node-drawer-body');
    const how = [capitalize(section.mode), section.style].filter(Boolean).join(' · ');
    const aim = section.targetLevel ? `Aim: ${levelLabel(section.targetLevel)}` : '';
    if (how || aim) body.append(h('p', 'plan-meta', [how, aim].filter(Boolean).join(' · ')));
    if (section.exit) body.append(h('p', 'plan-line', `Goal: ${section.exit}`));
    const covers = coversLine(section, nodes);
    if (covers) body.append(h('p', 'plan-line', covers));
    if ((section.notes ?? '').trim()) body.append(prose(section.notes, 'prose plan-node-notes'));
    if (Boolean(section.assumedKnown) && !refuted.has(section.id)) {
      body.append(h('p', 'plan-line known-line', 'You said you know this. One quick check when we get here.'));
    }
    if (section.desktopOnly) body.append(h('p', 'plan-line', 'Desktop only'));
    body.append(...sectionControls(section, edit));
    drawer.replaceChildren(head, body);
  }

  /** The section's actions, exactly as the card has always offered them. */
  function sectionControls(section, edit) {
    const controls = [];
    if (section.plugins?.length) {
      const fs = h('fieldset', 'tools');
      const legend = h('legend', '');
      legend.innerHTML = `Tools<span class="sr-only"> for ${escapeHtml(section.title ?? section.id)}</span>`;
      fs.append(legend);
      // `needsInstall` (W9) is the code-computed set of plugins the planner
      // proposed that weren't installed at plan_write — all registry-
      // installable and on this platform. The live `installed` set wins for a
      // plugin installed since the plan was written (it becomes a checkbox);
      // the field tells the card which missing plugins are install offers, so
      // a stale or foreign missing id is still shown but never offered as one.
      const proposed = new Set(section.needsInstall ?? []);
      const boxes = [];
      for (const plugin of section.plugins) {
        const label = labels.get(plugin) || plugin;
        if (!installed.has(plugin)) {
          const missing = h('span', 'tool missing');
          if (proposed.has(plugin) || section.needsInstall == null) {
            // Offer consent; never install as a side effect of approving (W9).
            const chip = button(`${label} — install`, 'text install-chip');
            chip.addEventListener('click', () => onInstall?.(plugin, chip));
            missing.append(chip);
          } else {
            missing.append(h('span', 'plan-line', `${label} — not installed`));
          }
          fs.append(missing);
          continue;
        }
        const kept = edit.plugins ?? section.plugins;
        const l = h('label', 'tool');
        const c = h('input');
        c.type = 'checkbox';
        c.value = plugin;
        c.checked = kept.includes(plugin);
        c.addEventListener('change', () => {
          const next = boxes.filter((b) => b.checked).map((b) => b.value);
          const same = next.length === section.plugins.filter((p) => installed.has(p)).length;
          if (same) delete edit.plugins;
          else edit.plugins = next;
          refresh({ figure: true });
        });
        l.append(c, h('span', '', label));
        boxes.push(c);
        fs.append(l);
      }
      controls.push(fs);
      if (section.plugins.some((p) => !installed.has(p))) {
        controls.push(h('p', 'plan-line', 'Not installed: skipped if approved.'));
      }
    }
    controls.push(
      choiceRows('How far', `sec-level-${section.id}`, LEVELS, edit.targetLevel ?? section.targetLevel ?? '', (v) => {
        edit.targetLevel = v;
        refresh({ figure: true });
      }),
      choiceRows(
        'Include',
        `sec-keep-${section.id}`,
        [['keep', 'Keep'], ['drop', "Don't need this"]],
        edit.keep === false ? 'drop' : 'keep',
        (v) => {
          if (v === 'drop') edit.keep = false;
          else delete edit.keep;
          refresh({ figure: true });
        },
      ),
    );
    const known = textareaField('Parts I already know…', {
      rows: 2,
      value: edit.knownNote ?? section.knownNote ?? '',
      placeholder: 'e.g. I already know KVL, just not loaded dividers.',
    });
    known.input.addEventListener('input', () => {
      if (known.input.value.trim()) edit.knownNote = known.input.value;
      else delete edit.knownNote;
      refresh();
    });
    controls.push(known.wrap);
    if (section.hunt?.available) {
      const huntWrap = h('div', 'field');
      const label = h('label', 'tool');
      const c = h('input');
      c.type = 'checkbox';
      c.checked = edit.hunt === undefined ? Boolean(section.hunt.enabled) : edit.hunt;
      c.addEventListener('change', () => {
        edit.hunt = c.checked;
        refresh({ figure: true });
        // The learner's toggle is immediate: one op, no planner round trip.
        Promise.resolve(onHuntToggle?.(section.id, c.checked)).catch((err) => status.set(String(err), true));
      });
      label.append(c, h('span', '', 'Hunt'));
      const why = h('p', 'help', section.hunt.reason || 'Find something out for yourself before this section.');
      huntWrap.append(label, why);
      controls.push(huntWrap);
    }
    const note = textareaField('Note for the planner', {
      rows: 2,
      value: edit.learnerNote ?? section.learnerNote ?? '',
      placeholder: 'Anything else the planner should know.',
    });
    note.input.addEventListener('input', () => {
      if (note.input.value.trim()) edit.learnerNote = note.input.value;
      else delete edit.learnerNote;
      refresh();
    });
    controls.push(note.wrap);
    return controls;
  }

  // Escape closes the drawer from anywhere in the card; the card is not a
  // dialog, so nothing here holds focus (ui-design §9.1).
  doc.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && openSectionId !== null) {
      e.stopPropagation();
      closeDrawer({ restore: true });
    }
  });
  // One box per section is the card now; the actions live in the drawer the
  // box's "⋯" opens (§7.7.1).

  /**
   * The action row, and — when asked — the boxes' own state words. Nothing
   * re-renders the whole card here: that would drop the caret in a textarea
   * and fold the drawer the learner is working in.
   */
  function refresh({ figure = false } = {}) {
    const pending = hasPlanEdits(edits);
    if (figure && sections.length) draw();
    // One planner job at a time: the backend refuses edits while it runs.
    approve.disabled = busy || pending;
    approve.hidden = false;
    send.hidden = !pending;
    send.disabled = busy;
    discard.hidden = !pending || questionPending;
    noteField.wrap.hidden = !pending || questionPending;
    actions.hidden = questionPending;
    if (busy && !status.el.textContent) {
      status.set(pending ? 'Revising. Send changes when it arrives.' : 'Revising. Approve when it arrives.');
    }
  }

  /** Clears the collected edits after they were sent (or discarded). */
  function resetEdits() {
    edits.plan = {};
    for (const edit of Object.values(edits.sections)) for (const k of Object.keys(edit)) delete edit[k];
    edits.comment = '';
    noteField.input.value = '';
    refresh({ figure: true });
  }

  send.addEventListener('click', async () => {
    if (!hasPlanEdits(edits)) return;
    const payload = planEditsPayload(edits);
    send.disabled = true;
    discard.disabled = true;
    status.set('Sending…');
    try {
      await onSendEdits(payload, (edits.comment ?? '').trim());
      resetEdits();
      status.set('Sent. Revising the plan…');
    } catch (err) {
      send.disabled = false;
      discard.disabled = false;
      status.set(String(err), true);
    }
  });

  approve.addEventListener('click', async () => {
    if (busy || hasPlanEdits(edits) || questionPending) return;
    approve.disabled = true;
    status.set('Approving…');
    try {
      await onApprove();
    } catch (err) {
      approve.disabled = false;
      status.set(String(err), true);
    }
  });

  discard.addEventListener('click', () => {
    discard.disabled = false;
    resetEdits();
    // The app rebuilds the card so every control shows the plan's own values
    // again (the collected edits are gone); focus lands on the new heading.
    onDiscard?.();
  });

  doc.append(noteField.wrap, actions, status.el, change.form);
  if (sections.length) draw();
  // A card rebuilt under an open drawer (a state refresh, a plan revision)
  // keeps it open on the same section — the learner did not change sections.
  if (openSectionId !== null) {
    const section = sectionById.get(openSectionId);
    if (section) {
      fillDrawer(section);
      drawer.hidden = false;
      syncExpanded();
    } else {
      openSectionId = null;
      edits.drawer = null;
    }
  }
  refresh();
  return doc;
}


/** The plan list: sections with their status words and their claims. */
function sectionList(st) {
  const plan = st.plan;
  const nodes = new Map((plan.nodes ?? []).map((n) => [n.id, n]));
  const sections = plan.sections ?? [];
  const done = new Set(st.completedSections ?? []);
  // Same rule as the diagram (plan-diagram.js::progressOf): the open section,
  // else the next unfinished one — never "the last one" by accident.
  const currentId =
    st.currentSection ?? (st.ended ? null : sections.find((s) => !done.has(s.id))?.id ?? null);
  const ol = h('ol', 'sheet-sections');
  for (const s of sections) {
    const isDone = done.has(s.id);
    const current = !isDone && currentId === s.id;
    const li = h('li', current ? 'current' : '');
    if (current) li.setAttribute('aria-current', 'step');
    const t = h('p', 'sheet-section-title');
    if (isDone || current) {
      const g = h('span', 'glyph', isDone ? '✓ ' : '• ');
      g.setAttribute('aria-hidden', 'true');
      t.append(g);
    }
    t.append(document.createTextNode(s.title ?? s.id));
    if (isDone) t.append(h('span', 'sheet-status', ' — Done'));
    if (current) t.append(h('span', 'sheet-status', ' — Current'));
    const aimText = s.targetLevel ? `Aim: ${levelLabel(s.targetLevel)}` : '';
    if (s.assumedKnown) {
      const refuted = (st.knownRefuted ?? []).includes(s.id);
      const word = refuted ? 'Checked: needs teaching after all' : isDone ? 'Known · checked ✓' : 'Known · not checked yet';
      li.classList.add(refuted ? 'refuted' : 'known');
      t.append(h('span', 'sheet-status', ` — ${[word, aimText].filter(Boolean).join(' · ')}`));
    } else if (aimText) {
      t.append(h('span', 'sheet-status', ` — ${aimText}`));
    }
    li.append(t);
    const ids = s.nodeIds ?? [];
    if (ids.length) {
      const ul = h('ul', 'sheet-nodes');
      for (const id of ids) {
        const n = nodes.get(id) ?? { id, title: id };
        const checked = (st.checks?.[id] ?? []).some((c) => c.verdict === 'correct');
        const last = st.lastNodeId === id;
        const item = h('li', last ? 'last' : '');
        if (last) item.setAttribute('aria-current', 'true');
        if (checked) {
          const g = h('span', 'glyph', '✓ ');
          g.setAttribute('aria-hidden', 'true');
          item.append(g);
        }
        item.append(document.createTextNode(n.title ?? id));
        if (checked) item.append(h('span', 'sr-only', ' (checked)'));
        ul.append(item);
      }
      li.append(ul);
    }
    ol.append(li);
  }
  return ol;
}

/** "Section 5 of 9 · Exponential and logarithmic rules" — where teaching is. */
function positionLine(st) {
  const sections = st.plan?.sections ?? [];
  // Expanded from the card while the plan is still a proposal: nothing is
  // being taught yet, so the bar says how big it is, not where we are.
  if (st.phase === 'awaitApproval') return sections.length ? `${sections.length} sections · not approved yet` : '';
  const pos = sectionPosition(st);
  if (!pos) return '';
  const current = sections.find((s) => s.id === st.currentSection);
  if (current) return `Section ${pos.n} of ${pos.m} · ${current.title ?? current.id}`;
  if (st.ended) return `${pos.m} sections · finished`;
  const next = sections.find((s) => !(st.completedSections ?? []).includes(s.id));
  return next ? `Section ${pos.n} of ${pos.m} · ${next.title ?? next.id}` : `${pos.m} sections`;
}

/**
 * The plan overlay: the plan takes the whole screen and the session blurs
 * behind it (owner direction, 2026-11-07 — before this it was a 420 px rail with
 * a scrolling diagram in it, which made the one surface the learner opens on
 * purpose the smallest one in the app).
 *
 * Structure: a bar (where we are, level toggle, sections drawer, change form,
 * close), the diagram at natural size with pan/zoom over the whole stage, a
 * hint line, zoom controls. The diagram is the page; the list is a drawer.
 *
 * Level defaults to topics — nine boxes are legible at a glance and the current
 * topic carries the gold mark. `ideas` shows every claim in the plan.
 */
export function planOverlay(st, {
  headingId,
  level = 'topics',
  drawerOpen = false,
  formOpen,
  draft,
  onDraft,
  onFormOpen,
  onRequest,
  onClose,
  onLevel,
  onDrawer,
}) {
  const plan = st.plan;
  const root = h('div', 'plan-overlay-inner');
  const headId = headingId ?? nextId('plan');
  const views = new Map(); // level -> { svg, pan, width, height }

  const bar = h('div', 'plan-bar');
  const where = h('p', 'plan-where');
  const title = h('h2', 'plan-overlay-title', 'Plan');
  title.id = headId;
  title.tabIndex = -1;
  where.append(title);
  const pos = positionLine(st);
  if (pos) where.append(h('span', 'plan-where-text', ` — ${pos}`));

  const actions = h('div', 'plan-bar-actions');
  const levels = h('div', 'plan-levels');
  levels.setAttribute('role', 'group');
  levels.setAttribute('aria-label', 'Detail');
  const levelButtons = new Map();
  for (const [id, label] of [['topics', 'Topics'], ['ideas', 'Ideas']]) {
    const b = button(label, 'text plan-level', 'button');
    b.setAttribute('aria-pressed', String(level === id));
    b.addEventListener('click', () => {
      if (levelButtons.get(id)?.getAttribute('aria-pressed') === 'true') return;
      level = id;
      for (const [k, btn] of levelButtons) btn.setAttribute('aria-pressed', String(k === id));
      onLevel?.(id);
      show();
    });
    levelButtons.set(id, b);
    levels.append(b);
  }
  const drawerButton = button('Sections', 'text plan-drawer-toggle', 'button');
  drawerButton.setAttribute('aria-expanded', String(drawerOpen));
  const close = button('Close', 'text plan-close', 'button');
  close.addEventListener('click', onClose);
  actions.append(levels, drawerButton, close);
  bar.append(where, actions);

  // ---- the diagram stage (pan/zoom over the whole screen) ----
  const stage = h('div', 'plan-stage');
  stage.tabIndex = 0;
  stage.setAttribute('role', 'group');
  stage.setAttribute('aria-label', 'Plan diagram. Drag to pan, scroll to zoom, arrow keys to move.');
  const view = h('div', 'plan-view');
  stage.append(view);

  const zoomBox = h('div', 'plan-zoom');
  const zoomOut = button('−', 'text plan-zoom-btn', 'button');
  const zoomIn = button('+', 'text plan-zoom-btn', 'button');
  const zoomFit = button('Fit', 'text plan-zoom-btn', 'button');
  zoomOut.setAttribute('aria-label', 'Zoom out');
  zoomIn.setAttribute('aria-label', 'Zoom in');
  zoomFit.setAttribute('aria-label', 'Fit the plan to the screen');
  zoomBox.append(zoomOut, zoomIn, zoomFit);

  const hint = h('p', 'plan-hint', 'Drag to pan · scroll to zoom');

  let current = null; // { pan, level }
  function show() {
    const art = views.get(level) ?? makeView(level);
    view.replaceChildren(art.svg);
    view.dataset.width = String(art.width);
    view.dataset.height = String(art.height);
    current = art;
    art.pan.fit();
    // A 49-claim plan does not fit legibly, so open where teaching is: frame
    // the current section's boxes at a readable scale instead of the whole DAG
    // at 50 %. The whole shape is one "Fit" away.
    const focusId = currentSectionId();
    if (focusId && art.pan.scale() < 0.75) {
      const rect = boxesOf(focusId);
      if (rect) art.pan.focus(rect, 0.95);
    }
  }

  /** The section teaching is at (the diagram's own rule, plan-diagram.js). */
  function currentSectionId() {
    const done = new Set(st.completedSections ?? []);
    const sections = plan.sections ?? [];
    return st.currentSection ?? (st.ended ? null : sections.find((s) => !done.has(s.id))?.id ?? null);
  }

  /** Bounding box of one section's boxes in art coordinates, or null. */
  function boxesOf(sectionId) {
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (const g of view.querySelectorAll('.plan-node')) {
      const item = plan.nodes?.find((n) => n.id === g.dataset.nodeId);
      const nodeSection = item ? sectionOf(plan, item.id) : null;
      if (nodeSection !== sectionId) continue;
      const m = /translate\(([-\d.]+), ([-\d.]+)\)/.exec(g.getAttribute('transform') ?? '');
      const rect = g.querySelector('rect');
      if (!m || !rect) continue;
      const bx = Number(m[1]);
      const by = Number(m[2]);
      const bw = Number(rect.getAttribute('width'));
      const bh = Number(rect.getAttribute('height'));
      x0 = Math.min(x0, bx);
      y0 = Math.min(y0, by);
      x1 = Math.max(x1, bx + bw);
      y1 = Math.max(y1, by + bh);
    }
    return Number.isFinite(x0) ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } : null;
  }

  function makeView(which) {
    const { sectionStatus, nodeStatus } = progressOf(st);
    const art = planDiagram(plan, { level: which, maxW: which === 'ideas' ? 900 : 1200, sectionStatus, nodeStatus });
    art.svg.setAttribute('width', String(Math.round(art.width)));
    art.svg.setAttribute('height', String(Math.round(art.height)));
    // Focus order: the overlay's autofocus target is the heading, so the
    // diagram's boxes are reached by Tab from there, in layer order.
    const entry = { ...art, pan: attachPanZoom(stage, view) };
    views.set(which, entry);
    return entry;
  }

  zoomIn.addEventListener('click', () => current?.pan.zoomIn());
  zoomOut.addEventListener('click', () => current?.pan.zoomOut());
  zoomFit.addEventListener('click', () => current?.pan.fit());

  // ---- the drawer: section list, tools in use, change form ----
  const drawer = h('aside', 'plan-drawer');
  drawer.hidden = !drawerOpen;
  const drawerHead = h('div', 'plan-drawer-head');
  drawerHead.append(h('h3', 'plan-drawer-title', 'Sections'));
  const drawerClose = button('Hide', 'text', 'button');
  drawerClose.addEventListener('click', () => {
    drawer.hidden = true;
    drawerButton.setAttribute('aria-expanded', 'false');
    drawerButton.focus({ preventScroll: true });
    onDrawer?.(false);
  });
  drawerHead.append(drawerClose);
  drawer.append(drawerHead);
  if (plan.goal) drawer.append(h('p', 'sheet-goal', plan.goal));
  drawer.append(sectionList(st));
  if (st.activePlugins?.length) drawer.append(h('p', 'sheet-tools', `Tools in use: ${st.activePlugins.join(', ')}`));
  if (!st.ended) {
    const change = changeDisclosure('Change plan', 'secondary', { formOpen, draft, onDraft, onFormOpen, onRequest });
    change.wrap.append(change.reveal, change.form);
    drawer.append(change.wrap);
  }
  drawerButton.addEventListener('click', () => {
    const open = drawer.hidden;
    drawer.hidden = !open;
    drawerButton.setAttribute('aria-expanded', String(open));
    onDrawer?.(open);
    if (open) drawerClose.focus({ preventScroll: true });
  });

  root.append(bar, stage, hint, zoomBox, drawer);
  // First paint after the dialog is laid out (its box is 0×0 before showModal).
  queueMicrotask(show);
  root.show = show;
  return root;
}

// ---------- job lines, notices, dividers ----------

function briefText(item) {
  if (item.brief) return item.brief;
  const r = item.result;
  if (typeof r === 'string') return r;
  if (r && typeof r === 'object') return r.brief ?? r.content ?? r.summary ?? null;
  return null;
}

/** One line per background job, updated in place. Never announced. */
export function jobChip(item, { onCancel }) {
  const running = item.state === 'queued' || item.state === 'running';
  const research = item.jobKind !== 'plan';
  const q = item.question ?? '';
  const brief = research && item.state === 'done' ? briefText(item) : null;
  if (brief) {
    const d = h('details', 'job');
    d.append(h('summary', '', q ? `Research notes — ${q}` : 'Research notes'), prose(brief, 'prose brief'));
    return d;
  }
  const line = h('p', 'job');
  const subject = q ? ` — ${q}` : '';
  let text;
  if (research) {
    if (running) text = `Research${subject} · running`;
    else if (item.state === 'done') text = `Research${subject} · done`;
    else if (item.state === 'failed') text = `Research failed${subject}`;
    else text = `Research cancelled${subject}`;
  } else {
    if (running) text = 'Planner — drafting';
    else if (item.state === 'done') text = 'Planner — plan ready';
    else if (item.state === 'failed') text = 'Planner — failed';
    else text = 'Planner — cancelled';
  }
  line.append(document.createTextNode(text));
  if (item.state === 'failed' && item.error) {
    line.append(document.createTextNode(': '));
    line.append(h('span', 'danger', item.error));
  }
  if (running && item.jobId && onCancel) {
    const b = button('Cancel', 'text');
    b.setAttribute('aria-label', research ? `Cancel research${q ? `: ${q}` : ''}` : 'Cancel planning');
    b.addEventListener('click', () => {
      b.disabled = true;
      onCancel(item.jobId).catch(() => (b.disabled = false));
    });
    line.append(' ', b);
  }
  return line;
}

/** "Plan update  Plan revised: …" — or a planner failure in `--danger`. */
export function noticeEl(item) {
  const el = h('p', `notice${item.error ? ' danger' : ''}`);
  if (item.error) {
    el.setAttribute('role', 'status');
    el.textContent = item.text;
    return el;
  }
  el.append(h('span', 'small-caps notice-label', 'Plan update'), document.createTextNode(` ${item.text}`));
  return el;
}

export function dividerEl(text) {
  const el = h('div', 'divider');
  el.setAttribute('role', 'separator');
  el.setAttribute('aria-label', text);
  el.append(h('span', 'small-caps', text));
  return el;
}

// ---------- hunt card (ui-design §7.7.4) ----------

const HUNT_KINDS = {
  forum: 'A forum thread',
  datasheet: 'A datasheet',
  lecture: 'A lecture',
  video: 'A hobbyist video',
  paper: 'A paper',
  docs: 'The docs',
  person: 'Someone to ask',
};

/** One place to look: what it is, where, and why it's worth the trip. */
function huntPlace(entry) {
  const li = h('li', 'hunt-place');
  li.append(h('span', 'hunt-kind', HUNT_KINDS[entry?.kind] ?? 'Somewhere to look'));
  if (entry?.venue) li.append(h('span', 'hunt-venue', `: ${entry.venue}`));
  if (entry?.why) li.append(h('span', 'hunt-why', ` — ${entry.why}`));
  return li;
}

/** What the learner brought back, as the flow's "You" note. */
function returnedLines(returned) {
  const lines = [];
  if (returned?.notes) lines.push([returned.notes, 'you-text']);
  for (const link of returned?.links ?? []) lines.push([link, 'you-meta']);
  for (const file of returned?.files ?? []) {
    // The tutor gets file names; page reading does not exist yet.
    lines.push([`${file} (not readable yet; the tutor has your description)`, 'you-meta']);
  }
  return lines;
}

/**
 * The hunt: a question, where to look, what to bring back, and a return form
 * with nothing required. Shown at the start of a section whose hunt the
 * learner turned on; "Skip hunt" is always there, and the desk stays usable
 * while it's out. Once teaching has left the hunt's section the card stays in
 * the flow as history: it shows what was asked and can't be returned.
 */
export function huntCard(item, { onReturn, onSkip, readOnly = false, current = null }) {
  const args = item.args ?? {};
  const hunt = item.hunt ?? null;
  const state = hunt?.state ?? item.state ?? 'out';
  const returned = hunt?.returned ?? item.returned ?? null;
  const huntId = item.huntId ?? item.callId ?? args.callId ?? null;
  // The card's own section, from the state when there is one and from the
  // `hunt_send` arguments otherwise.
  const section = hunt?.sectionId ?? item.sectionId ?? null;
  const movedOn = Boolean(section) && section !== current;

  const block = h('section', 'hunt');
  const id = nextId('hunt');
  block.setAttribute('aria-labelledby', id);
  const kicker = h('h2', 'small-caps hunt-kicker', 'Hunt');
  kicker.id = id;
  block.append(kicker);
  block.append(prose(args.question, 'prose prompt'));
  if (Array.isArray(args.lookAt) && args.lookAt.length) {
    block.append(h('p', 'hunt-head', 'Where to look'));
    const ul = h('ul', 'hunt-list');
    for (const entry of args.lookAt) ul.append(huntPlace(entry));
    block.append(ul);
  }
  if (Array.isArray(args.bringBack) && args.bringBack.length) {
    block.append(h('p', 'hunt-head', 'Bring back'));
    const ul = h('ul', 'hunt-list');
    for (const what of args.bringBack) ul.append(h('li', '', what));
    block.append(ul);
  }

  if (state === 'skipped') {
    block.append(h('p', 'waiting', 'Skipped. The section starts normally.'));
    return block;
  }
  if (state === 'returned') {
    const lines = returnedLines(returned);
    if (lines.length) {
      const you = youBlock(lines);
      you.tabIndex = -1;
      you.dataset.refocus = '';
      block.append(you);
    }
    block.append(h('p', 'waiting', hunt?.debriefed ? 'Debriefed. Teaching continues.' : 'Debrief in progress.'));
    return block;
  }
  if (movedOn) {
    block.append(h('p', 'waiting', 'The section has moved on. This hunt is closed.'));
    return block;
  }
  if (readOnly) {
    block.append(h('p', 'waiting', 'Not answered.'));
    return block;
  }

  block.append(h('p', 'hunt-note', 'No deadline. Come back whenever you’re ready.'));

  const form = h('form', 'hunt-form');
  form.noValidate = true;
  const notes = textareaField('What you found', { rows: 3, placeholder: 'Your notes, in your own words.' });
  form.append(notes.wrap);

  const links = [];
  const linkList = h('div', 'hunt-link-list');
  const addLink = button('+ Add link', 'text');
  const addLinkRow = () => {
    const inputId = nextId('link');
    const input = h('input', 'input');
    input.type = 'url';
    input.id = inputId;
    input.placeholder = 'https://…';
    input.inputMode = 'url';
    input.spellcheck = false;
    input.setAttribute('aria-label', 'Link');
    linkList.append(input);
    links.push(input);
    return input;
  };
  addLinkRow();
  addLink.addEventListener('click', () => addLinkRow().focus({ preventScroll: true }));
  const linkField = h('div', 'field');
  linkField.append(h('p', 'hunt-head', 'Links'), linkList, addLink);

  const filesId = nextId('files');
  const filesLabel = h('label', '', 'Files');
  filesLabel.htmlFor = filesId;
  const files = h('input', 'hunt-files-input');
  files.type = 'file';
  files.multiple = true;
  files.id = filesId;
  const filesField = h('div', 'field');
  filesField.append(filesLabel, files, h('p', 'help', 'Unreadable files still reach the tutor by name.'));
  form.append(linkField, filesField);

  const isEmpty = () =>
    !notes.input.value.trim() && !links.some((i) => i.value.trim()) && !(files.files?.length ?? 0);

  const actions = h('div', 'actions');
  const back = button('I’m back', 'primary', 'submit');
  const skip = button('Skip hunt', 'text');
  const confirmRow = h('div', 'hunt-confirm');
  confirmRow.hidden = true;
  const sendAnyway = button('Send', 'secondary');
  const skipInstead = button('Skip', 'text');
  confirmRow.append(
    h('p', 'hunt-confirm-text', 'Send with nothing? You can also skip.'),
    (() => {
      const row = h('div', 'actions');
      row.append(sendAnyway, skipInstead);
      return row;
    })(),
  );
  actions.append(back, skip);
  const status = statusLine();
  form.append(actions, confirmRow, status.el);

  const controls = () => [...form.querySelectorAll('input, textarea, button')];
  const lock = (on) => controls().forEach((c) => (c.disabled = on));

  const skipHunt = async () => {
    confirmRow.hidden = true;
    skip.disabled = true;
    status.set('Skipping…');
    try {
      await onSkip(huntId);
      status.set('Skipped.');
    } catch (err) {
      skip.disabled = false;
      status.set(String(err), true);
    }
  };
  skip.addEventListener('click', skipHunt);
  skipInstead.addEventListener('click', skipHunt);

  const send = async () => {
    const payload = {};
    const note = notes.input.value.trim();
    if (note) payload.notes = note;
    const linkValues = links.map((i) => i.value.trim()).filter(Boolean);
    if (linkValues.length) payload.links = linkValues;
    const names = [...(files.files ?? [])].map((f) => f.name).filter(Boolean);
    if (names.length) payload.files = names;
    confirmRow.hidden = true;
    lock(true);
    status.set('Sending…');
    try {
      await onReturn(huntId, payload);
      status.set('Sent. The tutor has it.');
      status.el.focus({ preventScroll: true });
    } catch (err) {
      lock(false);
      status.set(String(err), true);
    }
  };
  sendAnyway.addEventListener('click', send);
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    if (isEmpty()) {
      confirmRow.hidden = false;
      sendAnyway.focus({ preventScroll: true });
      return;
    }
    send();
  });
  submitOnModEnter(form);
  block.append(form);
  return block;
}

// ---------- the close: field guide, confidence, frontier (§7.7.5) ----------

/**
 * The close's reference material, after the close text: where the field lives,
 * how the learner's confidence matched their results, and whatever this
 * session parked for later. Null when there is nothing to show, so an older
 * session's close renders exactly as before.
 */
export function closeExtras({ venues = [], depth = '', calibration = null, frontier = [], onStartFrontier = null } = {}) {
  const wrap = h('div', 'close-extras');
  const guide = fieldGuide(venues, { depth });
  if (guide) wrap.append(guide);
  const confidence = calibrationBlock(calibration);
  if (confidence) wrap.append(confidence);
  if (frontier.length) {
    wrap.append(h('p', 'saved-line', `Saved for later: ${frontier.map((f) => f.topic).join(', ')}`));
    const row = h('div', 'actions saved-actions');
    for (const f of frontier) {
      const b = button('Start a session', 'text');
      b.setAttribute('aria-label', `Start a session on ${f.topic}`);
      b.addEventListener('click', () => onStartFrontier?.(f.id, b));
      row.append(b);
    }
    wrap.append(row);
  }
  return wrap.childElementCount ? wrap : null;
}

