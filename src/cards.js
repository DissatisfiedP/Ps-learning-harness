// Learn-session views (M6): phase strip, question card, choice card, plan
// card, plan side panel, job chips, plan notices. Each builder returns a DOM
// element; callers own the backend calls through the handlers they pass in.

import { escapeHtml, renderMarkdown } from './markdown.js';

let uid = 0;
const nextId = (prefix) => `${prefix}-${++uid}`;

function h(tag, className, html) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (html !== undefined) el.innerHTML = html;
  return el;
}

function button(text, className = 'btn', type = 'button') {
  const b = h('button', className);
  b.type = type;
  b.textContent = text;
  return b;
}

function textareaField(label, { placeholder = '', rows = 3, value = '' } = {}) {
  const id = nextId('f');
  const wrap = h('div', 'field');
  wrap.innerHTML = `<label for="${id}">${escapeHtml(label)}</label>`;
  const ta = h('textarea', 'input');
  ta.id = id;
  ta.rows = rows;
  ta.placeholder = placeholder;
  ta.value = value;
  wrap.append(ta);
  return { wrap, input: ta };
}

// ---------- phase strip ----------

const PHASES = [
  { id: 'measure', label: 'Measure' },
  { id: 'plan', label: 'Plan' },
  { id: 'teach', label: 'Teach' },
  { id: 'close', label: 'Close' },
];

function phaseIndex(st) {
  if (st.ended || st.phase === 'close') return 3;
  if (st.phase === 'teach') return 2;
  if (st.phase === 'plan' || st.phase === 'awaitApproval') return 1;
  return 0;
}

/** Fills `nav` with Measure · Plan · Teach (section n/m) · Close. */
export function renderPhaseStrip(nav, st, planning) {
  const current = phaseIndex(st);
  const sections = st.plan?.sections ?? [];
  const n = sections.findIndex((s) => s.id === st.currentSection);
  const ol = h('ol');
  PHASES.forEach((p, i) => {
    const li = h('li', i < current ? 'done' : i === current ? 'current' : '');
    let label = p.label;
    if (p.id === 'teach' && sections.length && i <= current) {
      label += ` (section ${n >= 0 ? n + 1 : sections.length}/${sections.length})`;
    }
    if (p.id === 'plan' && st.phase === 'awaitApproval') label += ' (awaiting approval)';
    li.textContent = label;
    if (i === current) li.setAttribute('aria-current', 'step');
    ol.append(li);
  });
  nav.replaceChildren(ol);
  if (planning) {
    const p = h('span', 'planning', '<span class="spinner" aria-hidden="true"></span>Planning…');
    p.setAttribute('role', 'status');
    nav.append(p);
  }
}

// ---------- question card ----------

const ROLE_LABEL = { probe: 'Quick question', predict: 'Predict first', check: 'Check your understanding' };
const VERDICT_LABEL = { correct: 'Correct', partial: 'Partly right', wrong: 'Not quite' };

function answerSummary(a, format) {
  const dl = h('dl', 'qanswer');
  const row = (term, value, prose = false) => {
    if (value === undefined || value === null || value === '') return;
    dl.append(h('dt', '', escapeHtml(term)));
    const dd = h('dd', prose ? 'pre' : '');
    dd.textContent = String(value);
    dl.append(dd);
  };
  row(format === 'choice' ? 'You chose' : 'Your answer', a.dontKnow && !a.answer ? 'I don’t know' : a.answer, true);
  row('Your thinking', a.thinking, true);
  if (a.dontKnow) row('Confidence', 'I don’t know');
  else if (typeof a.confidence === 'number') row('Confidence', `${a.confidence}%`);
  return dl;
}

function verdictBlock(item) {
  const box = h('div', `verdict v-${item.verdict}`);
  box.append(h('strong', '', escapeHtml(VERDICT_LABEL[item.verdict] ?? item.verdict)));
  if (item.note) box.append(h('p', 'verdict-note', escapeHtml(item.note)));
  if (item.args.explanation) box.append(h('div', 'prose', renderMarkdown(item.args.explanation)));
  return box;
}

function questionForm(item, onAnswer) {
  const args = item.args;
  const form = h('form', 'qform');
  form.noValidate = true;
  let answerInput = null;
  let radios = [];
  if (args.format === 'choice' && Array.isArray(args.options)) {
    const fs = h('fieldset', 'options');
    fs.append(h('legend', '', 'Choose one'));
    const name = nextId('opt');
    for (const opt of args.options) {
      const label = h('label', 'opt');
      const r = h('input');
      r.type = 'radio';
      r.name = name;
      r.value = opt;
      label.append(r, h('span', '', escapeHtml(opt)));
      fs.append(label);
      radios.push(r);
    }
    form.append(fs);
  } else {
    const f = textareaField('Your answer', { rows: 3 });
    answerInput = f.input;
    form.append(f.wrap);
  }
  const thinking = textareaField('Your thinking', { rows: 2, placeholder: 'How did you get there? Optional.' });
  form.append(thinking.wrap);

  const confId = nextId('conf');
  const conf = h('div', 'confidence');
  conf.innerHTML = `<label for="${confId}">Confidence</label><input type="range" id="${confId}" min="0" max="100" step="5" value="50" /><output for="${confId}">50%</output>`;
  const slider = conf.querySelector('input');
  const out = conf.querySelector('output');
  slider.addEventListener('input', () => (out.textContent = `${slider.value}%`));
  form.append(conf);

  const actions = h('div', 'qactions');
  const idk = button('I don’t know', 'btn idk');
  idk.setAttribute('aria-pressed', 'false');
  const submit = button('Submit', 'btn primary', 'submit');
  const status = h('span', 'qstatus');
  status.setAttribute('role', 'status');
  actions.append(idk, submit, status);
  form.append(actions);

  idk.addEventListener('click', () => {
    const on = idk.getAttribute('aria-pressed') !== 'true';
    idk.setAttribute('aria-pressed', String(on));
    conf.hidden = on;
    status.textContent = '';
  });

  const controls = () => [...form.querySelectorAll('input, textarea, button')];
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const dontKnow = idk.getAttribute('aria-pressed') === 'true';
    const answer = answerInput ? answerInput.value.trim() : (radios.find((r) => r.checked)?.value ?? '');
    if (!answer && !dontKnow) {
      status.textContent = 'Answer first, or choose “I don’t know”.';
      (answerInput ?? radios[0])?.focus();
      return;
    }
    const payload = { answer, thinking: thinking.input.value.trim(), dontKnow };
    if (!dontKnow) payload.confidence = Number(slider.value);
    controls().forEach((c) => (c.disabled = true));
    status.textContent = 'Sending…';
    try {
      await onAnswer(item.callId, payload);
      status.textContent = 'Sent';
    } catch (err) {
      controls().forEach((c) => (c.disabled = false));
      status.textContent = err;
    }
  });
  return form;
}

/** Card for a `question_ask` call. */
export function questionCard(item, { onAnswer, readOnly }) {
  const args = item.args;
  if (item.failed) return h('div', 'hidden-item');
  const role = args.role ?? 'probe';
  const collapsed = role === 'check';
  const card = h(collapsed ? 'details' : 'section', `card qcard role-${role}`);
  const head = h(collapsed ? 'summary' : 'div', 'card-head');
  head.append(h('span', 'card-kicker', escapeHtml(ROLE_LABEL[role] ?? 'Question')));
  if (item.answer) head.append(h('span', `badge${item.verdict ? ` v-${item.verdict}` : ''}`, escapeHtml(item.verdict ? VERDICT_LABEL[item.verdict] ?? item.verdict : 'Answered')));
  card.append(head);
  if (!collapsed) card.setAttribute('aria-label', ROLE_LABEL[role] ?? 'Question');
  const body = h('div', 'card-body');
  body.append(h('div', 'prose', renderMarkdown(args.prompt ?? '')));
  if (item.answer) {
    body.append(answerSummary(item.answer, args.format));
    if (item.verdict) body.append(verdictBlock(item));
  } else if (readOnly) {
    body.append(h('p', 'muted', 'Not answered.'));
  } else {
    body.append(questionForm(item, onAnswer));
  }
  card.append(body);
  return card;
}

// ---------- choice card ----------

/** Card for a `choice_ask` call. */
export function choiceCard(item, { onAnswer, readOnly }) {
  const args = item.args;
  if (item.failed) return h('div', 'hidden-item');
  const card = h('section', 'card ccard');
  card.setAttribute('aria-label', 'Choice');
  card.append(h('div', 'card-head', '<span class="card-kicker">Your call</span>'));
  const body = h('div', 'card-body');
  body.append(h('div', 'prose', renderMarkdown(args.question ?? '')));
  card.append(body);
  if (item.answer) {
    const p = h('p', 'chosen');
    p.append(h('span', 'muted-label', 'You chose: '), document.createTextNode(item.answer.answer ?? ''));
    body.append(p);
    return card;
  }
  if (readOnly) {
    body.append(h('p', 'muted', 'Not answered.'));
    return card;
  }
  const form = h('form', 'qform');
  form.noValidate = true;
  const fs = h('fieldset', 'options');
  fs.append(h('legend', '', 'Options'));
  const name = nextId('choice');
  const radios = [];
  for (const opt of args.options ?? []) {
    const label = h('label', 'opt');
    const r = h('input');
    r.type = 'radio';
    r.name = name;
    r.value = opt;
    label.append(r, h('span', '', escapeHtml(opt)));
    if (args.recommended && opt === args.recommended) label.append(h('span', 'rec', '(recommended)'));
    fs.append(label);
    radios.push(r);
  }
  const otherLabel = h('label', 'opt other');
  const otherRadio = h('input');
  otherRadio.type = 'radio';
  otherRadio.name = name;
  otherRadio.value = '';
  const otherId = nextId('other');
  otherLabel.append(otherRadio, h('span', '', 'Other'));
  const otherText = h('input', 'input');
  otherText.type = 'text';
  otherText.id = otherId;
  otherText.placeholder = 'Something else…';
  const otherTextLabel = h('label', 'sr-only', 'Other answer');
  otherTextLabel.htmlFor = otherId;
  otherText.addEventListener('input', () => (otherRadio.checked = Boolean(otherText.value) || otherRadio.checked));
  otherRadio.addEventListener('change', () => otherRadio.checked && otherText.focus());
  fs.append(otherLabel, otherTextLabel, otherText);
  form.append(fs);

  const actions = h('div', 'qactions');
  const submit = button('Submit', 'btn primary', 'submit');
  const status = h('span', 'qstatus');
  status.setAttribute('role', 'status');
  actions.append(submit, status);
  form.append(actions);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const answer = otherRadio.checked ? otherText.value.trim() : (radios.find((r) => r.checked)?.value ?? '');
    if (!answer) {
      status.textContent = otherRadio.checked ? 'Write your own answer.' : 'Pick an option.';
      (otherRadio.checked ? otherText : radios[0] ?? otherRadio).focus();
      return;
    }
    const controls = [...form.querySelectorAll('input, button')];
    controls.forEach((c) => (c.disabled = true));
    status.textContent = 'Sending…';
    try {
      await onAnswer(item.callId, { answer });
      status.textContent = 'Sent';
    } catch (err) {
      controls.forEach((c) => (c.disabled = false));
      status.textContent = err;
    }
  });
  body.append(form);
  return card;
}

// ---------- plan ----------

const pluginKey = (sectionId, plugin) => `${sectionId}\u0000${plugin}`;

/** The plan card's removed-plugin set as `approve`'s `removedPlugins`. */
export function removedPluginList(removed) {
  return [...removed].map((key) => {
    const [sectionId, plugin] = key.split('\u0000');
    return { sectionId, plugin };
  });
}

function nodeLabel(node, { highlight, done }) {
  const li = h('li', `node${highlight ? ' current' : ''}${done ? ' done' : ''}`);
  if (highlight) li.setAttribute('aria-current', 'true');
  li.append(h('span', 'node-title', escapeHtml(node.title ?? node.id)));
  if (node.kind) li.append(h('span', 'kind', escapeHtml(node.kind)));
  if (done) li.append(h('span', 'sr-only', '(checked)'));
  return li;
}

/**
 * Ordered section list. In the plan card plugins are removable chips; in the
 * side panel the current section and node are highlighted.
 */
function sectionList(plan, opts) {
  const nodes = new Map((plan.nodes ?? []).map((n) => [n.id, n]));
  const ol = h('ol', 'plan-sections');
  for (const s of plan.sections ?? []) {
    const isCurrent = opts.currentSection === s.id;
    const isDone = opts.completed?.includes(s.id);
    const li = h('li', `plan-section${isCurrent ? ' current' : ''}${isDone ? ' done' : ''}`);
    if (isCurrent) li.setAttribute('aria-current', 'step');
    const head = h('div', 'section-head');
    head.append(h('span', 'section-title', escapeHtml(s.title ?? s.id)));
    if (s.mode) head.append(h('span', 'badge mode', escapeHtml(s.mode)));
    if (s.desktopOnly) head.append(h('span', 'badge', 'Desktop only'));
    if (isDone) head.append(h('span', 'badge ok', 'Done'));
    li.append(head);
    if (opts.detailed) {
      if (s.style) li.append(h('p', 'section-line', `<span class="muted-label">Style</span> ${escapeHtml(s.style)}`));
      if (s.exit) li.append(h('p', 'section-line', `<span class="muted-label">Exit</span> ${escapeHtml(s.exit)}`));
    }
    if (opts.removable && s.plugins?.length) {
      const chips = h('div', 'plugin-chips');
      chips.setAttribute('aria-label', `Plugins for ${s.title ?? s.id}`);
      chips.setAttribute('role', 'group');
      for (const plugin of s.plugins) chips.append(pluginChip(s, plugin, opts));
      li.append(chips);
    }
    const nodeIds = s.nodeIds ?? [];
    if (nodeIds.length) {
      const ul = h('ul', 'plan-nodes');
      for (const id of nodeIds) {
        const node = nodes.get(id) ?? { id, title: id };
        const done = (opts.checks?.[id] ?? []).some((c) => c.verdict === 'correct');
        ul.append(nodeLabel(node, { highlight: opts.lastNodeId === id, done }));
      }
      li.append(ul);
    }
    ol.append(li);
  }
  return ol;
}

function pluginChip(section, plugin, opts) {
  const key = pluginKey(section.id, plugin);
  const chip = h('span', 'plugin-chip');
  const name = h('span', 'plugin-name');
  name.textContent = plugin;
  const b = h('button', 'chip-x');
  b.type = 'button';
  const sync = () => {
    const removed = opts.removed.has(key);
    chip.classList.toggle('removed', removed);
    b.textContent = removed ? 'Undo' : '×';
    b.setAttribute('aria-label', removed ? `Keep plugin ${plugin} in ${section.title ?? section.id}` : `Remove plugin ${plugin} from ${section.title ?? section.id}`);
    b.setAttribute('aria-pressed', String(removed));
  };
  b.addEventListener('click', () => {
    if (opts.removed.has(key)) opts.removed.delete(key);
    else opts.removed.add(key);
    sync();
  });
  sync();
  chip.append(name, b);
  return chip;
}

function changeForm({ label, submitText, draft, onDraft, onRequest, onCancel }) {
  const form = h('form', 'change-form');
  form.noValidate = true;
  const f = textareaField(label, { rows: 3, value: draft ?? '', placeholder: 'What should change, and why?' });
  f.input.addEventListener('input', () => onDraft?.(f.input.value));
  form.append(f.wrap);
  const actions = h('div', 'qactions');
  const send = button(submitText, 'btn', 'submit');
  actions.append(send);
  if (onCancel) {
    const cancel = button('Cancel', 'btn link');
    cancel.addEventListener('click', onCancel);
    actions.append(cancel);
  }
  const status = h('span', 'qstatus');
  status.setAttribute('role', 'status');
  actions.append(status);
  form.append(actions);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const text = f.input.value.trim();
    if (!text) {
      status.textContent = 'Describe the change first.';
      f.input.focus();
      return;
    }
    send.disabled = true;
    status.textContent = 'Sending…';
    try {
      await onRequest(text);
      f.input.value = '';
      onDraft?.('');
      status.textContent = 'Sent to the planner.';
    } catch (err) {
      status.textContent = err;
    } finally {
      send.disabled = false;
    }
  });
  return { form, input: f.input };
}

/** The plan card shown while the plan awaits approval. */
export function planCard(plan, { removed, draft, onDraft, onApprove, onRequest }) {
  const card = h('section', 'card plan-card');
  const headId = nextId('plan');
  card.setAttribute('aria-labelledby', headId);
  card.append(h('div', 'card-head', `<h2 class="card-title" id="${headId}">Proposed plan</h2>`));
  const body = h('div', 'card-body');
  if (plan.goal) body.append(h('p', 'plan-goal', `<span class="muted-label">Goal</span> ${escapeHtml(plan.goal)}`));
  if (plan.approach) body.append(h('div', 'prose plan-approach', renderMarkdown(plan.approach)));
  body.append(sectionList(plan, { detailed: true, removable: true, removed }));

  const actions = h('div', 'plan-actions');
  const approve = button('Approve plan', 'btn primary');
  const status = h('span', 'qstatus');
  status.setAttribute('role', 'status');
  approve.addEventListener('click', async () => {
    approve.disabled = true;
    status.textContent = 'Approving…';
    try {
      await onApprove();
      status.textContent = '';
    } catch (err) {
      approve.disabled = false;
      status.textContent = err;
    }
  });
  actions.append(approve, status);
  body.append(actions);
  body.append(changeForm({ label: 'Request a change', submitText: 'Send request', draft, onDraft, onRequest }).form);
  card.append(body);
  return card;
}

/** Contents of the plan side panel during and after teaching. */
export function planPanel(st, { formOpen, draft, onDraft, onRequest, onToggleForm, onClose }) {
  const plan = st.plan;
  const frag = document.createDocumentFragment();
  const head = h('div', 'panel-head');
  head.append(h('h2', '', 'Plan'));
  const close = h('button', 'icon-btn');
  close.type = 'button';
  close.setAttribute('aria-label', 'Hide plan');
  close.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>';
  close.addEventListener('click', onClose);
  head.append(close);
  frag.append(head);
  const body = h('div', 'panel-body');
  if (plan.goal) body.append(h('p', 'plan-goal', escapeHtml(plan.goal)));
  body.append(
    sectionList(plan, {
      currentSection: st.currentSection,
      completed: st.completedSections,
      lastNodeId: st.lastNodeId,
      checks: st.checks,
    }),
  );
  if (st.activePlugins?.length) {
    const p = h('p', 'section-line');
    p.innerHTML = `<span class="muted-label">Tools</span> ${st.activePlugins.map(escapeHtml).join(', ')}`;
    body.append(p);
  }
  if (!st.ended) {
    if (formOpen) {
      const { form } = changeForm({
        label: 'Change plan',
        submitText: 'Send to planner',
        draft,
        onDraft,
        onRequest,
        onCancel: () => onToggleForm(false),
      });
      body.append(form);
    } else {
      const b = button('Change plan', 'btn');
      b.addEventListener('click', () => onToggleForm(true));
      body.append(b);
    }
  }
  frag.append(body);
  return frag;
}

// ---------- job chips & notices ----------

function briefText(item) {
  if (item.brief) return item.brief;
  const r = item.result;
  if (typeof r === 'string') return r;
  if (r && typeof r === 'object') return r.brief ?? r.content ?? r.summary ?? null;
  return null;
}

/** One chip per background job, updated in place as its state changes. */
export function jobChip(item, { onCancel }) {
  const running = item.state === 'queued' || item.state === 'running';
  const research = item.jobKind !== 'plan';
  const subject = item.question ? `: ${item.question}` : '';
  let label;
  if (research) {
    label = running ? `Researching${subject}` : item.state === 'done' ? `Researched${subject}` : item.state === 'failed' ? `Research failed${subject}` : `Research cancelled${subject}`;
  } else {
    label = running ? 'Planning…' : item.state === 'done' ? 'Plan ready' : item.state === 'failed' ? 'Planning failed' : 'Planning cancelled';
  }
  const brief = research && !running ? briefText(item) : null;
  if (brief) {
    const d = h('details', 'job-chip done');
    const s = h('summary');
    s.append(h('span', 'job-dot', ''), h('span', 'job-label', escapeHtml(label)));
    d.append(s, h('div', 'prose job-brief', renderMarkdown(brief)));
    return d;
  }
  const chip = h('div', `job-chip state-${item.state}`);
  chip.setAttribute('role', 'status');
  if (running) chip.append(h('span', 'spinner', ''));
  else chip.append(h('span', 'job-dot', ''));
  chip.append(h('span', 'job-label', escapeHtml(label)));
  if (item.error) chip.append(h('span', 'job-error', escapeHtml(item.error)));
  if (running && item.jobId && onCancel) {
    const b = button('Cancel', 'btn link small');
    b.setAttribute('aria-label', research ? `Cancel research${subject}` : 'Cancel planning');
    b.addEventListener('click', () => {
      b.disabled = true;
      onCancel(item.jobId).catch(() => (b.disabled = false));
    });
    chip.append(b);
  }
  return chip;
}

export function noticeEl(text) {
  const el = h('p', 'plan-notice');
  el.textContent = text;
  return el;
}

export function dividerEl(text) {
  const el = h('div', 'divider');
  el.setAttribute('role', 'separator');
  el.append(h('span', '', escapeHtml(text)));
  return el;
}
