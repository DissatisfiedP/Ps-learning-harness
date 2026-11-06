// Learn-session blocks: the running head's phase position, question and
// choice blocks, section openers, the plan document (await approval), the
// plan sheet's contents, job lines, notices and dividers. Each builder returns
// a DOM element; callers own the backend calls through the handlers they pass.

import { escapeHtml, renderMarkdown } from './markdown.js';

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

/** A "You" note inside an answered block. */
function youBlock(lines) {
  const you = h('div', 'you');
  you.append(h('p', 'you-label', 'You'));
  for (const [text, cls] of lines) if (text) you.append(h('p', cls, text));
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

/** Section position while teaching: `{ n, m }` (1-based), or null. */
export function sectionPosition(st) {
  const sections = st?.plan?.sections ?? [];
  if (!sections.length) return null;
  const i = sections.findIndex((s) => s.id === st.currentSection);
  return { n: i >= 0 ? i + 1 : sections.length, m: sections.length };
}

function labelFor(phase, st, { planning, planFailed, current }) {
  switch (phase) {
    case 'measure':
      return 'Measure';
    case 'plan':
      if (!current) return 'Plan';
      if (planning) return 'Planning…';
      if (planFailed) return 'Plan · failed';
      return st.phase === 'awaitApproval' ? 'Plan · awaiting approval' : 'Plan';
    case 'teach': {
      const pos = current ? sectionPosition(st) : null;
      return pos ? `Teach ${pos.n}/${pos.m}` : 'Teach';
    }
    default:
      return 'Close';
  }
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

function confidenceLabel(n) {
  return CONFIDENCE.find(([v]) => v === n)?.[1] ?? `${n}% sure`;
}

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

function verdictLine(item) {
  const a = item.answer;
  const v = VERDICT[item.verdict];
  const p = h('p', 'verdict');
  p.tabIndex = -1;
  p.dataset.refocus = '';
  const glyph = h('span', 'glyph', `${v?.glyph ?? '•'} `);
  glyph.setAttribute('aria-hidden', 'true');
  const sure = a.dontKnow
    ? ' You said you didn’t know.'
    : typeof a.confidence === 'number'
      ? ` You were ${confidenceLabel(a.confidence)}.`
      : '';
  p.append(glyph, document.createTextNode(`${v?.text ?? item.verdict}.${sure}`));
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
      label.append(r, h('span', '', opt));
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
        status.set('Write an answer, or choose “I don’t know”.');
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

/** Block for a `question_ask` call. Never collapsed while pending. */
export function questionCard(item, { onAnswer, readOnly }) {
  if (item.failed) return hiddenItem();
  const args = item.args;
  const pending = !item.answer && !readOnly;
  const block = exerciseShell(ROLE_LABEL[args.role] ?? 'Question', pending);
  block.append(prose(args.prompt, 'prose prompt'));
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
      if (item.note) block.append(prose(item.note, 'prose feedback'));
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
  if (i >= 0) el.append(h('p', 'small-caps opener-kicker', `Section ${i + 1} of ${sections.length}`));
  el.append(h('h2', 'opener-title', sectionTitle(item, fallbackPlan)));
  const exit = i >= 0 ? sections[i].exit : null;
  if (exit) el.append(h('p', 'opener-goal', `Goal: ${exit}`));
  return el;
}

// ---------- plan ----------

const pluginKey = (sectionId, plugin) => `${sectionId}\u0000${plugin}`;

/** The plan document's unchecked tools as `approve`'s `removedPlugins`. */
export function removedPluginList(removed) {
  return [...removed].map((key) => {
    const [sectionId, plugin] = key.split('\u0000');
    return { sectionId, plugin };
  });
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
      status.set('Sent. The planner is revising the plan.');
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
 * The plan document shown while the plan awaits approval. Tools are
 * checkboxes, checked by default; unchecked ones are collected in `removed`.
 */
export function planCard(plan, { removed, draft, formOpen, onDraft, onFormOpen, onApprove, onRequest }) {
  const doc = h('section', 'plan-doc');
  const headId = nextId('plan');
  doc.setAttribute('aria-labelledby', headId);
  doc.append(h('p', 'small-caps', 'Proposed plan'));
  const title = h('h2', 'plan-title', plan.goal || 'Proposed plan');
  title.id = headId;
  title.tabIndex = -1;
  doc.append(title);
  if (plan.approach) doc.append(prose(plan.approach));

  const nodes = new Map((plan.nodes ?? []).map((n) => [n.id, n]));
  const ol = h('ol', 'plan-sections');
  for (const s of plan.sections ?? []) {
    const li = h('li');
    li.append(h('p', 'plan-section-title', s.title ?? s.id));
    const how = [capitalize(s.mode), s.style].filter(Boolean).join(' · ');
    if (how) li.append(h('p', 'plan-meta', how));
    if (s.exit) li.append(h('p', 'plan-line', `Goal: ${s.exit}`));
    const covers = coversLine(s, nodes);
    if (covers) li.append(h('p', 'plan-line', covers));
    if (s.plugins?.length) {
      const fs = h('fieldset', 'tools');
      const legend = h('legend', '');
      legend.innerHTML = `Tools<span class="sr-only"> for ${escapeHtml(s.title ?? s.id)}</span>`;
      fs.append(legend);
      for (const plugin of s.plugins) {
        const key = pluginKey(s.id, plugin);
        const l = h('label', 'tool');
        const c = h('input');
        c.type = 'checkbox';
        c.checked = !removed.has(key);
        c.addEventListener('change', () => (c.checked ? removed.delete(key) : removed.add(key)));
        l.append(c, h('span', '', plugin));
        fs.append(l);
      }
      li.append(fs);
    }
    if (s.desktopOnly) li.append(h('p', 'plan-line', 'Desktop only'));
    ol.append(li);
  }
  doc.append(ol);

  const actions = h('div', 'actions');
  const approve = button('Approve plan', 'primary');
  const change = changeDisclosure('Suggest a change', 'text', { formOpen, draft, onDraft, onFormOpen, onRequest });
  actions.append(approve, change.reveal);
  const status = statusLine();
  approve.addEventListener('click', async () => {
    approve.disabled = true;
    status.set('Approving…');
    try {
      await onApprove();
    } catch (err) {
      approve.disabled = false;
      status.set(String(err), true);
    }
  });
  doc.append(actions, status.el, change.form);
  return doc;
}

/** Contents of the plan sheet (teach, close or ended). */
export function planSheet(st, { headingId, formOpen, draft, onDraft, onFormOpen, onRequest, onClose }) {
  const plan = st.plan;
  const frag = document.createDocumentFragment();
  const head = h('div', 'sheet-head');
  const title = h('h2', '', 'Plan');
  title.id = headingId;
  title.tabIndex = -1;
  const close = button('Close', 'text');
  close.addEventListener('click', onClose);
  head.append(title, close);
  frag.append(head);
  if (plan.goal) frag.append(h('p', 'sheet-goal', plan.goal));

  const nodes = new Map((plan.nodes ?? []).map((n) => [n.id, n]));
  const ol = h('ol', 'sheet-sections');
  for (const s of plan.sections ?? []) {
    const done = st.completedSections?.includes(s.id);
    const current = !done && st.currentSection === s.id;
    const li = h('li', current ? 'current' : '');
    if (current) li.setAttribute('aria-current', 'step');
    const t = h('p', 'sheet-section-title');
    if (done || current) {
      const g = h('span', 'glyph', done ? '✓ ' : '• ');
      g.setAttribute('aria-hidden', 'true');
      t.append(g);
    }
    t.append(document.createTextNode(s.title ?? s.id));
    if (done) t.append(h('span', 'sheet-status', ' — Done'));
    if (current) t.append(h('span', 'sheet-status', ' — Current'));
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
  frag.append(ol);
  if (st.activePlugins?.length) frag.append(h('p', 'sheet-tools', `Tools in use: ${st.activePlugins.join(', ')}`));
  if (!st.ended) {
    const change = changeDisclosure('Change plan', 'secondary', { formOpen, draft, onDraft, onFormOpen, onRequest });
    change.wrap.append(change.reveal, change.form);
    frag.append(change.wrap);
  }
  return frag;
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
