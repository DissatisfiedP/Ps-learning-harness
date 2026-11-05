// Learning Harness UI: sessions sidebar, home screen, conversation view.
//
// Learn sessions run on the agent runtime: one global event subscription
// delivers turn/stream/op events, and the conversation is rendered from the
// session's op log (see timeline.js). Practice keeps its per-call channel.

import { available, call, channel, message } from './api.js';
import { ensureKatex, escapeHtml } from './markdown.js';
import { initSettings, loadConfig, openSettings } from './settings.js';
import { LiveReply, assistantEl, userEl } from './reply.js';
import { affectsState, applyOp, createTimeline, isPending } from './timeline.js';
import { choiceCard, dividerEl, jobChip, noticeEl, planCard, planPanel, questionCard, removedPluginList, renderPhaseStrip } from './cards.js';

const $ = (sel) => document.querySelector(sel);

const els = {
  app: $('#app'),
  sessionList: $('#session-list'),
  home: $('#home'),
  conversation: $('#conversation'),
  messages: $('#messages'),
  timeline: $('#timeline'),
  planSlot: $('#plan-slot'),
  live: $('#live'),
  phaseStrip: $('#phase-strip'),
  planPanel: $('#plan-panel'),
  planToggle: $('#plan-toggle'),
  planScrim: $('#plan-scrim'),
  readonlyBar: $('#readonly-bar'),
  composer: $('#composer'),
  prompt: $('#prompt'),
  send: $('#send'),
  homeSlot: $('#home-composer-slot'),
  dockSlot: $('#dock-composer-slot'),
  title: $('#session-title'),
  modeChip: $('#session-mode'),
  hero: $('#hero'),
  heroSub: $('#hero-sub'),
  suggestions: $('#suggestions'),
  notice: $('#setup-notice'),
  modelPill: $('#model-pill'),
  modelPillText: $('#model-pill-text'),
  modelLabel: $('#model-label'),
  toast: $('#toast'),
};

const COPY = {
  learn: {
    hero: 'What do you want to learn?',
    sub: 'The tutor finds what you already know first, then teaches one step at a time.',
    placeholder: 'Describe a topic or a goal…',
    suggestions: ['Derivatives from first principles', 'How TCP handshakes work', 'Bayes’ theorem, intuitively'],
  },
  practice: {
    hero: 'What do you want to practise?',
    sub: 'Problems that hide the idea mid-solution, hints one rung at a time, honest grading.',
    placeholder: 'Name a topic, or paste a problem set…',
    suggestions: ['Chain rule problems', 'Big-O of recursive functions', 'Conditional probability'],
  },
};

const TRASH_SVG = '<svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/></svg>';

/** Items mounted when a session opens; older ones are prepended on scroll. */
const PAGE = 40;
const NARROW = window.matchMedia('(max-width: 1100px)');

const state = {
  mode: 'learn',
  sessions: [], // SessionSummary, newest first
  current: null, // open session view, see newView()
  practiceBusy: false,
  starting: false, // start_teaching is waiting for its `session` event
  loading: null, // { id, events, wasRunning } while load_session runs
  turns: new Map(), // sessionId -> Set of running agents ("tutor" | "planner")
  stick: true, // follow the stream while the view is at the bottom
  panelOpen: { wide: true, narrow: false },
  config: { providers: [], settings: {} },
};

function newView(summary) {
  return {
    id: summary.id ?? null,
    mode: summary.mode,
    title: summary.title ?? '',
    readOnly: Boolean(summary.readOnly),
    learn: summary.mode === 'learn',
    messages: [], // practice history (send_message path)
    tl: createTimeline(),
    start: 0, // index of the oldest mounted item
    live: null, // LiveReply while the tutor streams
    orphan: null, // finished stream whose message op has not arrived yet
    errorEl: null,
    st: null, // SessionState
    stTimer: 0,
    planKey: '',
    planVersion: null,
    removed: new Set(), // plugins removed on the plan card (see removedPluginList)
    planDraft: '',
    panelKey: '',
    panelDraft: '',
    panelFormOpen: false,
    userCount: 0,
    checkedUnanswered: false,
  };
}

// ---------- utilities ----------

let toastTimer;
function toast(text, isError = false) {
  els.toast.textContent = text;
  els.toast.classList.toggle('error', isError);
  els.toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (els.toast.hidden = true), isError ? 6000 : 2600);
}

/** Backend call whose rejection is normalised to a display string. */
async function cmd(name, args) {
  try {
    return await call(name, args);
  } catch (e) {
    throw message(e);
  }
}

function setNav(open) {
  els.app.classList.toggle('nav-open', open);
  $('#scrim').hidden = !open;
}

function nearBottom() {
  const m = els.messages;
  return m.scrollHeight - m.scrollTop - m.clientHeight < 120;
}

function scrollToBottom(force = false) {
  if (force || state.stick) {
    els.messages.scrollTo({ top: els.messages.scrollHeight, behavior: 'instant' });
    state.stick = true;
  }
}

function autoGrow() {
  els.prompt.style.height = 'auto';
  els.prompt.style.height = `${Math.min(els.prompt.scrollHeight, 240)}px`;
}

const running = (id, agent) => Boolean(id && state.turns.get(id)?.has(agent));

function isBusy() {
  const s = state.current;
  if (!s) return false;
  if (!s.learn) return state.practiceBusy;
  return running(s.id, 'tutor') || (state.starting && !s.id);
}

function syncBusy() {
  const busy = isBusy();
  els.composer.classList.toggle('busy', busy);
  els.send.setAttribute('aria-label', busy ? 'Stop' : 'Send');
  updateSendState();
}

function updateSendState() {
  els.send.disabled = isBusy() ? !state.current?.id : !els.prompt.value.trim();
}

// ---------- config / model ----------

function modelReady() {
  const { providers, settings } = state.config;
  const m = settings.roles?.tutor;
  return Boolean(m && providers.find((p) => p.id === m.provider)?.source);
}

function applyConfig(config) {
  state.config = config;
  const m = config.settings.roles?.tutor;
  const provider = config.providers.find((p) => p.id === m?.provider);
  const label = m ? `${m.model}${provider ? ` · ${provider.name}` : ''}` : 'Choose a model';
  els.modelPillText.textContent = label;
  els.modelLabel.textContent = m ? label : 'No model selected';
  els.modelPill.classList.toggle('ready', modelReady());
  els.notice.hidden = modelReady();
}

// ---------- sidebar ----------
//
// Rendered in full once at startup. After that each change (new session,
// retitle, a finished turn, a delete) moves or updates only its own entry.

const GROUPS = ['Today', 'Yesterday', 'Previous 7 days', 'Older'];
const sidebarEls = new Map(); // session id -> .session-item

function groupLabel(iso) {
  const day = 24 * 60 * 60 * 1000;
  const startOfToday = new Date();
  startOfToday.setHours(0, 0, 0, 0);
  const t = new Date(iso).getTime();
  if (t >= startOfToday.getTime()) return 'Today';
  if (t >= startOfToday.getTime() - day) return 'Yesterday';
  if (t >= startOfToday.getTime() - 7 * day) return 'Previous 7 days';
  return 'Older';
}

function sessionItemEl(s) {
  const item = document.createElement('div');
  item.className = `session-item${state.current?.id === s.id ? ' active' : ''}`;
  item.dataset.id = s.id;
  item.setAttribute('role', 'button');
  item.tabIndex = 0;
  item.innerHTML = `
    <span class="mode-dot"></span>
    <span class="title"></span>
    <button class="delete" type="button" aria-label="Delete session">${TRASH_SVG}</button>`;
  fillSessionItem(item, s);
  return item;
}

function fillSessionItem(item, s) {
  item.title = s.title;
  const dot = item.querySelector('.mode-dot');
  dot.className = `mode-dot ${s.mode}`;
  dot.title = s.mode === 'learn' ? 'Learn' : 'Practice';
  item.querySelector('.title').textContent = s.title;
}

function groupEl(label, create) {
  let g = els.sessionList.querySelector(`.session-group[data-group="${label}"]`);
  if (g || !create) return g;
  g = document.createElement('div');
  g.className = 'session-group';
  g.dataset.group = label;
  g.innerHTML = `<h4>${label}</h4>`;
  const order = GROUPS.indexOf(label);
  const after = [...els.sessionList.querySelectorAll('.session-group')].find((x) => GROUPS.indexOf(x.dataset.group) > order);
  els.sessionList.querySelector('.session-empty')?.remove();
  els.sessionList.insertBefore(g, after ?? null);
  return g;
}

function pruneGroups() {
  for (const g of els.sessionList.querySelectorAll('.session-group')) {
    if (!g.querySelector('.session-item')) g.remove();
  }
  if (!state.sessions.length && !els.sessionList.querySelector('.session-empty')) {
    els.sessionList.innerHTML = '<p class="session-empty">Your sessions will appear here.</p>';
  }
}

function renderSidebar() {
  const list = els.sessionList;
  sidebarEls.clear();
  list.textContent = '';
  if (!state.sessions.length) {
    list.innerHTML = '<p class="session-empty">Your sessions will appear here.</p>';
    return;
  }
  const frag = document.createDocumentFragment();
  let group = null;
  let container = null;
  for (const s of state.sessions) {
    const label = groupLabel(s.updatedAt);
    if (label !== group) {
      group = label;
      container = document.createElement('div');
      container.className = 'session-group';
      container.dataset.group = label;
      container.innerHTML = `<h4>${label}</h4>`;
      frag.append(container);
    }
    const item = sessionItemEl(s);
    sidebarEls.set(s.id, item);
    container.append(item);
  }
  list.append(frag);
}

const time = (iso) => Date.parse(iso) || 0;

/** Inserts or updates one session, keeping newest-first order. */
function upsertSession(summary) {
  const i = state.sessions.findIndex((x) => x.id === summary.id);
  const next = i >= 0 ? { ...state.sessions[i], ...summary } : { ...summary };
  next.updatedAt ??= next.createdAt ?? new Date().toISOString();
  if (i >= 0) state.sessions.splice(i, 1);
  const t = time(next.updatedAt);
  let j = state.sessions.findIndex((x) => time(x.updatedAt) <= t);
  if (j < 0) j = state.sessions.length;
  state.sessions.splice(j, 0, next);

  let item = sidebarEls.get(next.id);
  if (item) fillSessionItem(item, next);
  else {
    item = sessionItemEl(next);
    sidebarEls.set(next.id, item);
  }
  const label = groupLabel(next.updatedAt);
  const group = groupEl(label, true);
  const after = state.sessions[j + 1];
  const anchor = after && groupLabel(after.updatedAt) === label ? sidebarEls.get(after.id) : null;
  group.insertBefore(item, anchor ?? null);
  pruneGroups();
}

/** Marks a session as just updated (after a turn) without re-listing. */
function bumpSession(id) {
  const s = state.sessions.find((x) => x.id === id);
  if (s) upsertSession({ id, updatedAt: new Date().toISOString() });
}

function removeSession(id) {
  state.sessions = state.sessions.filter((x) => x.id !== id);
  sidebarEls.get(id)?.remove();
  sidebarEls.delete(id);
  pruneGroups();
}

function setActiveSession(id) {
  for (const el of els.sessionList.querySelectorAll('.session-item.active')) el.classList.remove('active');
  if (id) sidebarEls.get(id)?.classList.add('active');
}

async function deleteSession(id) {
  try {
    await call('delete_session', { id });
    if (state.current?.id === id) showHome();
    removeSession(id);
  } catch (e) {
    toast(message(e), true);
  }
}

function onSidebarClick(e) {
  const item = e.target.closest('.session-item');
  if (!item) return;
  const del = e.target.closest('.delete');
  if (!del) {
    openSession(item.dataset.id);
    return;
  }
  e.stopPropagation();
  // Two clicks: the first arms the button for a few seconds.
  if (!del.classList.contains('armed')) {
    del.classList.add('armed');
    del.setAttribute('aria-label', 'Click again to delete');
    del.title = 'Click again to delete';
    setTimeout(() => {
      del.classList.remove('armed');
      del.setAttribute('aria-label', 'Delete session');
      del.title = '';
    }, 3000);
    return;
  }
  deleteSession(item.dataset.id);
}

// ---------- views ----------

function setMode(mode) {
  state.mode = mode;
  for (const b of document.querySelectorAll('#mode-toggle button')) {
    b.setAttribute('aria-selected', String(b.dataset.mode === mode));
  }
  const copy = COPY[mode];
  els.hero.textContent = copy.hero;
  els.heroSub.textContent = copy.sub;
  els.prompt.placeholder = copy.placeholder;
  els.suggestions.innerHTML = '';
  for (const text of copy.suggestions) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'suggestion';
    b.textContent = text;
    b.addEventListener('click', () => {
      els.prompt.value = text;
      autoGrow();
      updateSendState();
      els.prompt.focus();
    });
    els.suggestions.append(b);
  }
}

/** Drops the open session's live state before another one replaces it. */
function closeCurrent() {
  const s = state.current;
  if (!s) return;
  s.live?.dispose();
  clearTimeout(s.stTimer);
  state.current = null;
}

function showHome() {
  closeCurrent();
  state.loading = null;
  els.home.hidden = false;
  els.conversation.hidden = true;
  els.homeSlot.append(els.composer);
  els.title.textContent = 'New session';
  els.modeChip.hidden = true;
  els.planToggle.hidden = true;
  setPanelVisible(false);
  setMode(state.mode);
  setActiveSession(null);
  syncBusy();
  els.prompt.focus();
}

function showConversation() {
  const s = state.current;
  els.home.hidden = true;
  els.conversation.hidden = false;
  els.title.textContent = s.title;
  els.modeChip.hidden = false;
  els.modeChip.textContent = s.mode === 'learn' ? 'Learn' : 'Practice';
  els.modeChip.classList.toggle('practice', s.mode === 'practice');
  els.dockSlot.hidden = s.readOnly;
  els.readonlyBar.hidden = !s.readOnly;
  if (!s.readOnly) els.dockSlot.append(els.composer);
  els.phaseStrip.hidden = !s.learn || s.readOnly;
  els.planToggle.hidden = true;
  setPanelVisible(false);
  els.prompt.placeholder = 'Reply…';
  setActiveSession(s.id);
  if (s.learn && !s.readOnly) renderPhase();
  syncBusy();
}

function setTitle(s, title) {
  s.title = title;
  if (state.current === s) els.title.textContent = title;
}

// ---------- timeline ----------

function renderItem(item) {
  const s = state.current;
  switch (item.kind) {
    case 'user':
      return userEl(item.msg.content);
    case 'assistant':
      return assistantEl(item.msg);
    case 'notice':
      return noticeEl(item.text);
    case 'divider':
      return dividerEl(item.text);
    case 'job':
      return jobChip(item, { onCancel: s.readOnly ? null : (jobId) => cmd('cancel_job', { sessionId: s.id, jobId }) });
    case 'question':
      return questionCard(item, { readOnly: s.readOnly, onAnswer: (callId, payload) => cmd('answer', { sessionId: s.id, callId, payload }) });
    case 'choice':
      return choiceCard(item, { readOnly: s.readOnly, onAnswer: (callId, payload) => cmd('answer', { sessionId: s.id, callId, payload }) });
    default:
      return document.createElement('div');
  }
}

function itemEl(item) {
  if (!item.el) item.el = renderItem(item);
  return item.el;
}

/** Mounts only the newest PAGE items; older ones arrive via prependOlder(). */
function mountTimeline() {
  const s = state.current;
  const items = s.tl.items;
  els.timeline.textContent = '';
  els.planSlot.textContent = '';
  els.live.textContent = '';
  s.start = Math.max(0, items.length - PAGE);
  const frag = document.createDocumentFragment();
  for (let i = s.start; i < items.length; i++) frag.append(itemEl(items[i]));
  els.timeline.append(frag);
}

function prependOlder() {
  const s = state.current;
  if (!s || s.start === 0) return;
  const m = els.messages;
  const top = m.scrollTop;
  const before = m.scrollHeight;
  const from = Math.max(0, s.start - PAGE);
  const frag = document.createDocumentFragment();
  for (let i = from; i < s.start; i++) frag.append(itemEl(s.tl.items[i]));
  s.start = from;
  els.timeline.prepend(frag);
  // Keep the visible content where it was (unless scroll anchoring already did).
  const target = top + (m.scrollHeight - before);
  if (Math.abs(m.scrollTop - target) > 1) m.scrollTo({ top: target, behavior: 'instant' });
}

/** Prepends older items until the view can scroll (short histories). */
function fillViewport() {
  const m = els.messages;
  while (state.current?.start > 0 && m.scrollHeight <= m.clientHeight + 200) prependOlder();
}

let scrollFrame = 0;
function onMessagesScroll() {
  if (scrollFrame) return;
  scrollFrame = requestAnimationFrame(() => {
    scrollFrame = 0;
    state.stick = nearBottom();
    if (els.messages.scrollTop < 400) prependOlder();
  });
}

function appendItem(item) {
  els.timeline.append(itemEl(item));
}

function rerenderItem(item) {
  if (!item.el) return; // not mounted yet; renders fresh when scrolled to
  const next = renderItem(item);
  // Keep a <details> card open if the learner had opened it.
  if (item.el.tagName === 'DETAILS' && next.tagName === 'DETAILS') next.open = item.el.open;
  item.el.replaceWith(next);
  item.el = next;
}

// ---------- live reply / errors ----------

function ensureLive() {
  const s = state.current;
  if (!s.live) {
    s.live = new LiveReply({ onPaint: () => scrollToBottom() });
    els.live.append(s.live.el);
    clearError();
    scrollToBottom();
  }
  return s.live;
}

function dropLive(s) {
  if (!s.live) return;
  s.live.dispose();
  s.live.el.remove();
  s.live = null;
}

function clearError() {
  const s = state.current;
  s?.errorEl?.remove();
  if (s) s.errorEl = null;
}

function showError(text, action, onRetry) {
  const s = state.current;
  clearError();
  const el = document.createElement('div');
  el.className = 'error-card';
  el.setAttribute('role', 'alert');
  el.innerHTML = `<div class="text"><strong>Reply failed</strong>${escapeHtml(text)}</div>`;
  const b = document.createElement('button');
  b.className = 'btn';
  b.type = 'button';
  b.textContent = action;
  b.addEventListener('click', () => {
    clearError();
    onRetry();
  });
  el.append(b);
  if (/model|connect|settings/i.test(text)) {
    const st = document.createElement('button');
    st.className = 'btn link';
    st.type = 'button';
    st.textContent = 'Settings';
    st.addEventListener('click', openSettings);
    el.append(st);
  }
  s.errorEl = el;
  els.live.append(el);
  scrollToBottom();
}

function showTurnError(text, action = 'Retry') {
  const s = state.current;
  showError(text, action, () => {
    cmd('resume_turn', { sessionId: s.id }).catch((err) => {
      if (state.current === s && !s.errorEl && !running(s.id, 'tutor')) showTurnError(err);
    });
  });
}

// ---------- Learn: runtime events ----------

function subscribe() {
  call('subscribe_events', { onEvent: channel(onRuntimeEvent) }).catch((e) => toast(message(e), true));
}

function onRuntimeEvent(ev) {
  const id = ev.sessionId;
  if (ev.kind === 'session') {
    onSessionSummary(ev.session);
    return;
  }
  if (ev.kind === 'turnStart') {
    if (!state.turns.has(id)) state.turns.set(id, new Set());
    state.turns.get(id).add(ev.agent);
  } else if (ev.kind === 'turnEnd') {
    state.turns.get(id)?.delete(ev.agent);
    if (ev.agent === 'tutor') bumpSession(id);
  }
  if (state.loading?.id === id) {
    state.loading.events.push(ev);
    return;
  }
  const s = state.current;
  if (s?.learn && s.id === id) sessionEvent(s, ev);
}

function onSessionSummary(summary) {
  const known = state.sessions.some((x) => x.id === summary.id);
  const s = state.current;
  if (state.starting && s?.learn && !s.id && !known && summary.mode === 'learn') {
    s.id = summary.id;
    upsertSession(summary);
    setActiveSession(s.id);
    setTitle(s, summary.title);
    syncBusy();
    return;
  }
  upsertSession(summary);
  if (s?.id === summary.id && summary.title) setTitle(s, summary.title);
}

function sessionEvent(s, ev) {
  switch (ev.kind) {
    case 'turnStart':
      if (ev.agent === 'tutor') {
        s.orphan = null; // a new turn: an unmatched earlier stream stays as shown
        ensureLive();
      }
      renderPhase();
      syncBusy();
      break;
    case 'turnEnd':
      if (ev.agent === 'tutor') endLive(s, ev.error);
      else if (ev.error) toast(`Planning failed: ${ev.error}`, true);
      renderPhase();
      syncBusy();
      updateComposerHint();
      break;
    case 'text': {
      const live = ensureLive();
      if (!live.midway) live.addText(ev.delta);
      break;
    }
    case 'thinking': {
      const live = ensureLive();
      if (!live.midway) live.addThinking(ev.delta);
      break;
    }
    case 'op':
      onOp(s, ev.op);
      break;
  }
}

function endLive(s, error) {
  const live = s.live;
  if (live && !error && live.hasContent && !live.midway) {
    // Streamed text whose message op has not arrived yet: keep showing it.
    s.orphan = { el: live.finalize({ content: live.text, thinking: live.thinking }), text: live.text };
    els.timeline.append(s.orphan.el);
    s.live = null;
  } else {
    dropLive(s);
  }
  if (error) showTurnError(error);
}

function onOp(s, op) {
  if (op.op === 'title' && op.title) setTitle(s, op.title);
  const change = applyOp(s.tl, op);
  if (change?.added) {
    const item = change.added;
    if (item.kind === 'assistant') {
      if (s.orphan && s.orphan.text === item.msg.content) {
        item.el = s.orphan.el;
        s.orphan = null;
        rerenderItem(item);
      } else if (s.live) {
        item.el = s.live.finalize(item.msg);
        s.live = null;
        appendItem(item);
      } else appendItem(item);
    } else {
      if (item.kind === 'user') {
        s.userCount++;
        clearError();
      }
      // A card ends the streamed segment; drop an empty typing bubble.
      if ((item.kind === 'question' || item.kind === 'choice') && s.live && !s.live.hasContent) dropLive(s);
      appendItem(item);
    }
    scrollToBottom(item.kind === 'user');
  } else if (change?.updated) {
    rerenderItem(change.updated);
  }
  if (affectsState(op)) scheduleState();
  updateComposerHint();
}

// ---------- Learn: session state, phase strip, plan ----------

function scheduleState() {
  const s = state.current;
  clearTimeout(s.stTimer);
  s.stTimer = setTimeout(() => refreshState(s), 50);
}

async function refreshState(s) {
  if (!s.learn || s.readOnly || !s.id) return;
  let st;
  try {
    st = await call('session_state', { sessionId: s.id });
  } catch (e) {
    if (state.current === s) toast(message(e), true);
    return;
  }
  if (state.current !== s) return;
  s.st = st;
  renderPhase();
  renderPlanCard();
  renderPanel();
  updateComposerHint();
  if (!s.checkedUnanswered) {
    s.checkedUnanswered = true;
    offerReplyIfUnanswered(s);
  }
}

/** After a crash or a failed turn, the last learner message may have no reply. */
function offerReplyIfUnanswered(s) {
  const st = s.st;
  if (running(s.id, 'tutor') || running(s.id, 'planner') || st.ended || st.pendingCall) return;
  if (st.phase !== 'measure' && st.phase !== 'teach') return;
  if (s.tl.items.at(-1)?.kind === 'user' && !s.errorEl) {
    showTurnError('The tutor has not answered this yet.', 'Get a reply');
  }
}

function renderPhase() {
  const s = state.current;
  if (!s?.learn || s.readOnly) return;
  renderPhaseStrip(els.phaseStrip, s.st ?? { phase: 'measure' }, running(s.id, 'planner'));
}

function renderPlanCard() {
  const s = state.current;
  const st = s.st;
  const show = st?.phase === 'awaitApproval' && st.plan;
  const key = show ? `v${st.planVersion}` : '';
  if (key === s.planKey) return;
  s.planKey = key;
  if (s.planVersion !== st?.planVersion) {
    s.planVersion = st?.planVersion;
    s.removed.clear();
  }
  if (!show) {
    els.planSlot.textContent = '';
    return;
  }
  const stick = state.stick;
  els.planSlot.replaceChildren(
    planCard(st.plan, {
      removed: s.removed,
      draft: s.planDraft,
      onDraft: (text) => (s.planDraft = text),
      onApprove: () =>
        cmd('approve', {
          sessionId: s.id,
          removedPlugins: removedPluginList(s.removed),
        }),
      onRequest: (text) => cmd('request_plan_change', { sessionId: s.id, text }),
    }),
  );
  if (stick) scrollToBottom();
}

function panelAvailable(s) {
  const st = s?.st;
  return Boolean(s?.learn && !s.readOnly && st?.plan && (st.phase === 'teach' || st.phase === 'close' || st.ended));
}

function panelOpen() {
  return NARROW.matches ? state.panelOpen.narrow : state.panelOpen.wide;
}

function setPanelVisible(visible) {
  els.planPanel.hidden = !visible;
  els.planScrim.hidden = !(visible && NARROW.matches);
  els.planToggle.setAttribute('aria-expanded', String(visible));
}

function renderPanel() {
  const s = state.current;
  if (!panelAvailable(s)) {
    els.planToggle.hidden = true;
    setPanelVisible(false);
    if (s) s.panelKey = '';
    return;
  }
  els.planToggle.hidden = false;
  const st = s.st;
  const key = JSON.stringify([st.planVersion, st.currentSection, st.completedSections, st.lastNodeId, st.checks, st.activePlugins, st.ended, s.panelFormOpen]);
  if (key !== s.panelKey) {
    s.panelKey = key;
    els.planPanel.replaceChildren(
      planPanel(st, {
        formOpen: s.panelFormOpen,
        draft: s.panelDraft,
        onDraft: (text) => (s.panelDraft = text),
        onRequest: (text) => cmd('request_plan_change', { sessionId: s.id, text }),
        onToggleForm: (open) => {
          s.panelFormOpen = open;
          renderPanel();
          const target = open ? els.planPanel.querySelector('.change-form textarea') : els.planPanel.querySelector('.panel-body > .btn');
          target?.focus();
        },
        onClose: () => togglePanel(false),
      }),
    );
  }
  setPanelVisible(panelOpen());
}

function togglePanel(open = !panelOpen()) {
  if (NARROW.matches) state.panelOpen.narrow = open;
  else state.panelOpen.wide = open;
  setPanelVisible(open && panelAvailable(state.current));
  if (open) els.planPanel.querySelector('button, textarea')?.focus();
  else els.planToggle.focus();
}

function updateComposerHint() {
  const s = state.current;
  if (!s?.learn || s.readOnly) return;
  const lastCard = s.tl.items.findLast((i) => i.kind === 'question' || i.kind === 'choice');
  let hint = 'Reply…';
  if (lastCard && isPending(lastCard)) hint = 'Answer the question above first…';
  else if (s.st?.phase === 'awaitApproval') hint = 'Approve the plan above, or request a change';
  els.prompt.placeholder = hint;
}

// ---------- opening sessions ----------

async function openSession(id) {
  if (state.practiceBusy && state.current?.id !== id) {
    toast('Wait for the current reply to finish, or stop it.');
    return;
  }
  setNav(false);
  if (state.current?.id === id) return;
  const loading = { id, events: [], wasRunning: running(id, 'tutor') };
  state.loading = loading;
  let data;
  try {
    [data] = await Promise.all([call('load_session', { id }), ensureKatex().catch(() => {})]);
  } catch (e) {
    if (state.loading === loading) state.loading = null;
    toast(message(e), true);
    return;
  }
  if (state.loading !== loading) return; // another session was opened meanwhile
  closeCurrent();
  const s = newView(data);
  if (s.learn) {
    for (const op of data.ops ?? []) applyOp(s.tl, op);
    s.userCount = s.tl.items.filter((i) => i.kind === 'user').length;
  } else {
    s.messages = data.messages;
    s.tl.items = data.messages.map((msg, i) => ({ kind: msg.role === 'user' ? 'user' : 'assistant', key: `m${i}`, msg }));
  }
  state.current = s;
  showConversation();
  mountTimeline();
  if (s.learn && !s.readOnly) {
    if (loading.wasRunning) ensureLive().midway = true;
    state.loading = null;
    for (const ev of loading.events) sessionEvent(s, ev);
    updateComposerHint();
    refreshState(s);
  } else {
    state.loading = null;
    const last = s.messages.at(-1);
    if (!s.learn && last?.role === 'user') showError('The tutor has not answered this yet.', 'Get a reply', () => runPractice(null));
  }
  scrollToBottom(true);
  fillViewport();
  syncBusy();
}

// ---------- sending ----------

async function startLearn(text) {
  closeCurrent();
  const s = newView({ id: null, mode: 'learn', title: text });
  state.current = s;
  state.starting = true;
  showConversation();
  mountTimeline();
  ensureLive();
  syncBusy();
  ensureKatex().catch(() => {});
  try {
    const summary = await call('start_teaching', { text });
    if (state.current === s && !s.id && summary?.id) onSessionSummary(summary);
  } catch (e) {
    if (state.current === s && !s.id) {
      // Nothing was saved (e.g. no model chosen): put the text back.
      state.starting = false;
      showHome();
      els.prompt.value = text;
      autoGrow();
      updateSendState();
      toast(message(e), true);
    } else if (state.current === s && !s.errorEl && !running(s.id, 'tutor')) {
      showTurnError(message(e));
    }
  } finally {
    state.starting = false;
    syncBusy();
  }
}

async function sendLearner(text) {
  const s = state.current;
  const before = s.userCount;
  try {
    await call('send_learner_message', { sessionId: s.id, text });
  } catch (e) {
    if (state.current !== s) return;
    if (s.userCount === before) {
      // Rejected (question pending, plan phase…): give the text back.
      if (!els.prompt.value) {
        els.prompt.value = text;
        autoGrow();
        updateSendState();
      }
      toast(message(e), true);
    } else if (!s.errorEl && !running(s.id, 'tutor')) {
      showTurnError(message(e));
    }
  }
}

/**
 * Practice: runs one tutor turn over the per-call channel. `text` is the
 * learner's new message, or null to retry the last unanswered one.
 */
async function runPractice(text) {
  const isNew = !state.current;
  if (isNew) {
    state.current = newView({ id: null, mode: 'practice', title: text });
    showConversation();
    mountTimeline();
  }
  const s = state.current;
  if (text !== null) {
    s.messages.push({ role: 'user', content: text });
    const item = { kind: 'user', key: `m${s.messages.length - 1}`, msg: { role: 'user', content: text } };
    s.tl.items.push(item);
    appendItem(item);
  }
  const live = ensureLive();
  scrollToBottom(true);
  state.practiceBusy = true;
  syncBusy();

  const onEvent = channel((ev) => {
    if (ev.kind === 'session') {
      if (!s.id) s.id = ev.session.id;
      upsertSession(ev.session);
      if (state.current === s) setActiveSession(s.id);
      setTitle(s, ev.session.title);
      updateSendState();
      return;
    }
    if (ev.kind === 'text') live.addText(ev.delta);
    else if (ev.kind === 'thinking') live.addThinking(ev.delta);
  });

  try {
    const stored = text === null
      ? await call('retry_reply', { sessionId: s.id, onEvent })
      : await call('send_message', { sessionId: isNew ? null : s.id, mode: 'practice', text, onEvent });
    s.messages.push(stored);
    const item = { kind: 'assistant', key: `m${s.messages.length - 1}`, msg: stored };
    item.el = live.finalize(stored);
    s.live = null;
    s.tl.items.push(item);
    appendItem(item);
  } catch (e) {
    dropLive(s);
    if (isNew && !s.id) {
      state.practiceBusy = false;
      showHome();
      els.prompt.value = text;
      autoGrow();
      toast(message(e), true);
    } else if (state.current === s) {
      showError(message(e), 'Retry', () => runPractice(null));
    }
  } finally {
    state.practiceBusy = false;
    syncBusy();
    if (state.current === s) scrollToBottom();
    if (s.id) bumpSession(s.id);
  }
}

function stop() {
  const s = state.current;
  if (s?.id) call('cancel_reply', { sessionId: s.id }).catch((e) => toast(message(e), true));
}

function submit(event) {
  event.preventDefault();
  if (isBusy()) {
    stop();
    return;
  }
  const text = els.prompt.value.trim();
  if (!text) return;
  if (!modelReady()) {
    toast('Choose a model first.', true);
    openSettings();
    return;
  }
  els.prompt.value = '';
  autoGrow();
  updateSendState();
  const s = state.current;
  if (!s) {
    if (state.mode === 'learn') startLearn(text);
    else runPractice(text);
  } else if (s.learn) sendLearner(text);
  else runPractice(text);
}

// ---------- links inside messages ----------

els.messages.addEventListener('click', (e) => {
  const a = e.target.closest('a[href]');
  if (!a) return;
  e.preventDefault();
  const url = a.getAttribute('href');
  if (url.startsWith('https://')) call('open_external', { url }).catch((err) => toast(message(err), true));
});

// ---------- wiring ----------

function wire() {
  els.composer.addEventListener('submit', submit);
  els.prompt.addEventListener('input', () => {
    autoGrow();
    updateSendState();
  });
  els.prompt.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      if (!isBusy()) els.composer.requestSubmit();
    }
  });
  document.querySelectorAll('#mode-toggle button').forEach((b) => b.addEventListener('click', () => setMode(b.dataset.mode)));
  const home = () => {
    if (state.practiceBusy) return toast('Wait for the current reply to finish, or stop it.');
    setNav(false);
    showHome();
  };
  $('#new-session').addEventListener('click', home);
  $('#readonly-new').addEventListener('click', () => {
    state.mode = 'learn';
    home();
  });
  els.sessionList.addEventListener('click', onSidebarClick);
  els.sessionList.addEventListener('keydown', (e) => {
    const item = e.target;
    if (!item.classList?.contains('session-item')) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      openSession(item.dataset.id);
    }
  });
  els.messages.addEventListener('scroll', onMessagesScroll, { passive: true });
  els.planToggle.addEventListener('click', () => togglePanel());
  els.planScrim.addEventListener('click', () => togglePanel(false));
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && NARROW.matches && !els.planPanel.hidden) togglePanel(false);
  });
  NARROW.addEventListener('change', () => {
    if (panelAvailable(state.current)) setPanelVisible(panelOpen());
  });
  $('#open-settings').addEventListener('click', openSettings);
  els.modelPill.addEventListener('click', openSettings);
  $('#setup-button').addEventListener('click', openSettings);
  $('#open-sidebar').addEventListener('click', () => setNav(true));
  $('#close-sidebar').addEventListener('click', () => setNav(false));
  $('#scrim').addEventListener('click', () => setNav(false));
  initSettings({ changed: applyConfig, toast });
}

async function start() {
  wire();
  showHome();
  if (!available) {
    toast('Open Learning Harness through its app to use it.', true);
    return;
  }
  subscribe();
  const [config, sessions] = await Promise.allSettled([loadConfig(), call('list_sessions')]);
  if (config.status === 'fulfilled') applyConfig(config.value);
  else toast(message(config.reason), true);
  if (sessions.status === 'fulfilled') {
    // Keep entries a `session` event may already have added.
    const seen = new Set(sessions.value.map((x) => x.id));
    state.sessions = [...state.sessions.filter((x) => !seen.has(x.id)), ...sessions.value].sort((a, b) => time(b.updatedAt) - time(a.updatedAt));
  } else toast(message(sessions.reason), true);
  renderSidebar();
  if (state.current) setActiveSession(state.current.id);
  // Warm KaTeX once the window is idle, not on the startup path.
  (window.requestIdleCallback ?? ((f) => setTimeout(f, 1500)))(() => ensureKatex().catch(() => {}));
}

start();
