// Learning Harness UI ("Carrel"): Home (start + session list) and the
// session view (running head, page, desk).
//
// Learn sessions run on the agent runtime: one global event subscription
// delivers turn/stream/op events, and the page is rendered from the
// session's op log (see timeline.js). Practice keeps its per-call channel.
//
// Nothing scrolls or takes focus on its own except where docs/ui-design.md
// §9.1 says so: turn start (once, when the learner is at the end), session
// open (resumption cue), and the learner's own actions.

import { available, call, channel, message } from './api.js';
import { ensureKatex, plainText } from './markdown.js';
import { initSettings, loadConfig, openSettings } from './settings.js';
import { LiveReply, assistantEl, userEl } from './reply.js';
import { affectsState, applyOp, createTimeline, isPending } from './timeline.js';
import { createSessionList } from './sessions.js';
import { createMap } from './map.js';
import {
  choiceCard,
  dividerEl,
  jobChip,
  noticeEl,
  planCard,
  planSheet,
  questionCard,
  removedPluginList,
  renderPhase as renderPhaseNav,
  sectionOpener,
  sectionPosition,
  sectionTitle,
  verdictText,
} from './cards.js';

const $ = (sel) => document.querySelector(sel);

const els = {
  home: $('#home'),
  start: $('#start'),
  unavailable: $('#unavailable'),
  liveError: $('#live-error'),
  hero: $('#hero'),
  modes: $('#mode-toggle'),
  notice: $('#setup-notice'),
  sessionList: $('#session-list'),
  sessionsError: $('#sessions-error'),
  sessionsErrorText: $('#sessions-error-text'),
  conversation: $('#conversation'),
  back: $('#back'),
  title: $('#session-title'),
  phase: $('#phase'),
  headWord: $('#head-word'),
  headStatus: $('#head-status'),
  planToggle: $('#plan-toggle'),
  messages: $('#messages'),
  timeline: $('#timeline'),
  planSlot: $('#plan-slot'),
  live: $('#live'),
  continueBtn: $('#continue-reading'),
  deskNote: $('#desk-note'),
  deskNoteText: $('#desk-note-text'),
  deskNoteButton: $('#desk-note-button'),
  planPanel: $('#plan-panel'),
  composer: $('#composer'),
  prompt: $('#prompt'),
  send: $('#send'),
  promptError: $('#prompt-error'),
  homeSlot: $('#home-composer-slot'),
  dockSlot: $('#dock-composer-slot'),
  announcer: $('#announcer'),
  openMap: $('#open-map'),
  mapView: $('#map'),
  mapBack: $('#map-back'),
  mapHeading: $('#map-heading'),
  mapPage: $('#map-page'),
  mapCount: $('#map-count'),
  mapClear: $('#map-clear'),
  mapPractise: $('#map-practise'),
  mapError: $('#map-error'),
};

const COPY = {
  learn: {
    hero: 'What do you want to learn?',
    placeholder: 'Describe a topic or a goal…',
  },
  practice: {
    hero: 'What do you want to practise?',
    placeholder: 'Name a topic, or paste a problem set…',
  },
};

const READ_ONLY_NOTE = 'This session was made with an earlier version of the tutor and can’t continue.';
const GUARD = 'Wait for the reply to finish, or stop it.';

/** Items mounted when a session opens; older ones are prepended on scroll. */
const PAGE = 40;
/** Gap between the running head and a block aligned under it (--s6). */
const ALIGN_GAP = 32;

const state = {
  mode: 'learn',
  current: null, // open session view, see newView()
  practiceBusy: false,
  starting: false, // start_teaching is waiting for its `session` event
  loading: null, // { id, events, wasRunning } while load_session runs
  turns: new Map(), // sessionId -> Set of running agents ("tutor" | "planner")
  config: { providers: [], settings: {} },
  homeScroll: 0,
  lastAnnounce: 0,
  lastAnnounceKind: '', // 'exercise' for a question/choice announcement
  mapLoad: 0, // increments per Map visit; stale learning_map results are dropped
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
    errorEl: null,
    st: null, // SessionState
    stError: null, // session_state failure message
    stTimer: 0,
    planKey: '',
    planVersion: null,
    removed: new Set(), // tools unchecked on the plan document
    planDraft: '',
    planFormOpen: false,
    panelKey: '',
    panelDraft: '',
    panelFormOpen: false,
    planFailed: false,
    composerShown: true,
    turnStartedAt: 0,
    openersVersion: null, // plan version section openers were last refreshed for
    atEnd: false, // learner was at the end when their last message was added
    userCount: 0,
    checkedUnanswered: false,
  };
}

const sessions = createSessionList(els.sessionList, {
  onOpen: (id) => openSession(id),
  onDelete: (id) => deleteSession(id),
});

const map = createMap(els.mapPage, { onChange: renderMapDesk });

// ---------- utilities ----------

/** Backend call whose rejection is normalised to a display string. */
async function cmd(name, args) {
  try {
    return await call(name, args);
  } catch (e) {
    throw message(e);
  }
}

/**
 * Polite screen-reader message: cleared first, set on the next frame.
 * Messages from the same frame are joined, so none overwrites another.
 */
let announceQueue = [];
function announce(text, kind = '') {
  state.lastAnnounce = performance.now();
  state.lastAnnounceKind = kind;
  if (!announceQueue.length) {
    els.announcer.textContent = '';
    requestAnimationFrame(() => {
      els.announcer.textContent = announceQueue.join(' ');
      announceQueue = [];
    });
  }
  announceQueue.push(text);
}

const focusQuiet = (el) => el?.focus({ preventScroll: true });

/**
 * Whether the learner is "not engaged elsewhere": focus is on the body, the
 * desk textarea, or a control that has just been disabled, hidden or removed.
 */
function notEngaged() {
  const a = document.activeElement;
  return !a || a === document.body || a === els.prompt || !a.isConnected || a.disabled || Boolean(a.closest('[hidden]'));
}

/** Bottom edge (viewport px) of the flow's last visible block. */
function contentBottom() {
  for (const box of [els.live, els.planSlot, els.timeline]) {
    let last = box.lastElementChild;
    while (last?.hidden) last = last.previousElementSibling;
    if (last) return last.getBoundingClientRect().bottom;
  }
  return els.messages.getBoundingClientRect().top;
}

/** The learner is "at the end": the flow's bottom is within 120 px of the viewport's. */
function nearBottom() {
  return contentBottom() - els.messages.getBoundingClientRect().bottom < 120;
}

/** Instantly scrolls so `el`'s top sits `gap` px below the running head. */
function alignUnderHead(el, gap = ALIGN_GAP) {
  const m = els.messages;
  const top = m.scrollTop + el.getBoundingClientRect().top - m.getBoundingClientRect().top - gap;
  m.scrollTo({ top: Math.max(0, top), behavior: 'instant' });
}

function autoGrow() {
  const p = els.prompt;
  p.style.height = 'auto';
  p.style.height = `${p.scrollHeight + 2}px`; // max-height (CSS) caps it; then it scrolls
}

function setPromptError(text) {
  els.promptError.textContent = text;
}

const running = (id, agent) => Boolean(id && state.turns.get(id)?.has(agent));

/** A tutor reply is being written into the open view. */
function isStreaming() {
  const s = state.current;
  if (!s) return false;
  if (!s.learn) return state.practiceBusy;
  return running(s.id, 'tutor') || (state.starting && !s.id);
}

function syncBusy() {
  const s = state.current;
  const busy = isStreaming();
  els.send.textContent = !s ? 'Start' : busy ? 'Stop' : 'Send';
  if (els.send.classList.contains('secondary') !== busy) {
    // The Send/Stop swap is a state change, not a hover: never fade it, even
    // when the pointer still rests on the button (§8).
    els.send.style.transition = 'none';
    els.send.classList.toggle('primary', !busy);
    els.send.classList.toggle('secondary', busy);
    getComputedStyle(els.send).backgroundColor; // commit the new colour untransitioned
    els.send.style.transition = '';
  }
  if (busy) els.send.setAttribute('aria-label', 'Stop the reply');
  else els.send.removeAttribute('aria-label');
  updateSendState();
  scheduleFrame();
}

function updateSendState() {
  els.send.disabled = isStreaming() ? !state.current?.id : !els.prompt.value.trim();
}

function headStatus(text) {
  if (els.headStatus.textContent !== text) els.headStatus.textContent = text;
}

// ---------- config / model ----------

function modelReady() {
  const { providers, settings } = state.config;
  const m = settings.roles?.tutor;
  return Boolean(m && providers.find((p) => p.id === m.provider)?.source);
}

function applyConfig(config) {
  state.config = config;
  els.notice.hidden = modelReady();
}

// ---------- Home ----------

function setMode(mode) {
  state.mode = mode;
  for (const r of els.modes.querySelectorAll('input')) r.checked = r.value === mode;
  const copy = COPY[mode];
  els.hero.textContent = copy.hero;
  if (!state.current) els.prompt.placeholder = copy.placeholder;
}

async function loadSessions() {
  els.sessionsError.hidden = true;
  try {
    sessions.reset(await call('list_sessions'));
  } catch (e) {
    els.sessionsErrorText.textContent = `Couldn’t load your sessions: ${message(e)}`;
    els.sessionsError.hidden = false;
    sessions.render();
  }
}

async function deleteSession(id) {
  await cmd('delete_session', { id });
  state.turns.delete(id);
  const next = sessions.remove(id);
  focusQuiet(next ?? els.prompt);
}

// ---------- views ----------

/** Drops the open session's live state before another one replaces it. */
function closeCurrent() {
  const s = state.current;
  if (!s) return;
  s.live?.dispose();
  clearTimeout(s.stTimer);
  if (els.planPanel.open) els.planPanel.close();
  state.current = null;
}

function setComposerContext(home) {
  if (home) {
    els.homeSlot.append(els.composer);
    els.prompt.setAttribute('aria-labelledby', 'hero');
    els.prompt.placeholder = COPY[state.mode].placeholder;
  } else {
    els.dockSlot.append(els.composer);
    els.prompt.setAttribute('aria-labelledby', 'prompt-label');
    els.prompt.placeholder = 'Reply to the tutor';
  }
  els.composer.hidden = false;
  setPromptError('');
}

/** Shows Home. `focusId`: the session just left (its row gets focus); `focus`: an element to focus instead. */
function showHome({ focusId = null, focus = null } = {}) {
  closeCurrent();
  state.loading = null;
  els.conversation.hidden = true;
  els.mapView.hidden = true;
  els.home.hidden = false;
  setComposerContext(true);
  setMode(state.mode);
  syncBusy();
  els.home.scrollTop = state.homeScroll;
  focusQuiet(focus ?? ((focusId && sessions.openButton(focusId)) || els.prompt));
}

function showConversation() {
  const s = state.current;
  if (!els.home.hidden) state.homeScroll = els.home.scrollTop;
  els.home.hidden = true;
  els.mapView.hidden = true;
  els.conversation.hidden = false;
  els.title.textContent = s.title;
  const learnLive = s.learn && !s.readOnly;
  els.phase.hidden = !learnLive;
  els.phase.textContent = '';
  els.headWord.hidden = learnLive;
  els.headWord.textContent = s.readOnly ? 'Read-only' : 'Practice';
  els.planToggle.hidden = true;
  headStatus('');
  setComposerContext(false);
  s.composerShown = true;
  renderDesk({ quiet: true });
  if (learnLive) renderPhase();
  syncBusy();
}

function setTitle(s, title) {
  s.title = title;
  if (state.current === s) els.title.textContent = title;
}

// ---------- page items ----------

function renderItem(item) {
  const s = state.current;
  const onAnswer = (callId, payload) => cmd('answer', { sessionId: s.id, callId, payload });
  switch (item.kind) {
    case 'user':
      return userEl(item.msg.content);
    case 'assistant':
      return assistantEl(item.msg, { showThinking: !s.learn });
    case 'notice':
      return noticeEl(item);
    case 'divider':
      return dividerEl(item.text);
    case 'section':
      return sectionOpener(item, s.st?.plan);
    case 'job':
      return jobChip(item, { onCancel: s.readOnly ? null : (jobId) => cmd('cancel_job', { sessionId: s.id, jobId }) });
    case 'question':
      return questionCard(item, { readOnly: s.readOnly, onAnswer });
    case 'choice':
      return choiceCard(item, { readOnly: s.readOnly, onAnswer });
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
  els.live.style.minBlockSize = '';
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

/**
 * One coalesced frame for everything that reads layout after a change:
 * older history near the top, reserved space below the view, and the
 * "Continue reading" button. Scroll events and ops only schedule it.
 */
let scrollFrame = 0;
function scheduleFrame() {
  if (!scrollFrame) scrollFrame = requestAnimationFrame(onFrame);
}

function onFrame() {
  scrollFrame = 0;
  if (!state.current || els.conversation.hidden) return;
  if (els.messages.scrollTop < 400) prependOlder();
  releaseReserve();
  updateContinue();
}

/**
 * Gives back the space reserved for an aligned reply (#live's min height),
 * but only the part below the viewport: the scroll position stays valid, so
 * nothing in view moves.
 */
function releaseReserve() {
  const reserved = parseFloat(els.live.style.minBlockSize);
  if (!reserved) return;
  const m = els.messages;
  const slack = m.scrollHeight - m.clientHeight - m.scrollTop;
  if (slack < 1) return;
  const next = Math.max(0, reserved - slack);
  els.live.style.minBlockSize = next ? `${next}px` : '';
}

/**
 * Appends an item's element to the flow. The live reply already sits at the
 * end of the timeline (finalised in place, never moved), and a learner entry
 * that arrives while it is still empty ("Writing…") goes before it.
 */
function appendItem(item) {
  const s = state.current;
  if (item.el?.isConnected) return;
  const el = itemEl(item);
  if (item.kind === 'user' && s?.live?.el.isConnected && !s.live.hasContent) els.timeline.insertBefore(el, s.live.el);
  else els.timeline.append(el);
}

function rerenderItem(item) {
  if (!item.el) return; // not mounted yet; renders fresh when scrolled to
  const old = item.el;
  const next = renderItem(item);
  const hadFocus = old.contains(document.activeElement);
  // Keep a <details> open if the learner had opened it.
  if (old.tagName === 'DETAILS' && next.tagName === 'DETAILS') next.open = old.open;
  old.replaceWith(next);
  item.el = next;
  if (hadFocus) {
    // Its focus would be lost with the old element: keep it in the block.
    let target = next.querySelector('[data-refocus], summary, button:not([disabled]), input, textarea');
    if (!target) {
      next.tabIndex = -1;
      target = next;
    }
    focusQuiet(target);
  }
}

// ---------- "Continue reading" ----------

/** Shows "↓ Continue reading" when the flow's end is below the viewport, never while streaming. */
function updateContinue() {
  const s = state.current;
  let show = false;
  if (s && !isStreaming()) show = contentBottom() > els.messages.getBoundingClientRect().bottom + 8;
  if (els.continueBtn.hidden === show) els.continueBtn.hidden = !show;
}

function continueReading() {
  const m = els.messages;
  const viewTop = m.getBoundingClientRect().top + ALIGN_GAP + 1;
  const tutors = els.page.querySelectorAll('.entry.tutor');
  const last = tutors[tutors.length - 1] ?? null;
  const blocks = [last];
  if (last) {
    const after = [...els.page.querySelectorAll('.exercise.pending, .plan-doc, .exercise')].find(
      (b) => last.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING,
    );
    blocks.push(after ?? null);
  }
  const target = blocks.find((b) => b && b.getBoundingClientRect().top > viewTop);
  if (target) alignUnderHead(target);
  else m.scrollTo({ top: m.scrollTop + contentBottom() - m.getBoundingClientRect().bottom + ALIGN_GAP, behavior: 'instant' });
  updateContinue();
  focusQuiet(!els.composer.hidden ? els.prompt : els.deskNoteButton.hidden ? null : els.deskNoteButton);
}

// ---------- live reply / errors ----------

/**
 * Starts the live reply. With `align` (turn start only), scrolls once,
 * instantly, if the learner is at the end — or was, just before their own
 * message was added (s.atEnd). A later model call in the same turn starts a
 * new reply segment without aligning, so the page never moves mid-turn.
 */
function beginLive(s, { align = false } = {}) {
  const atEnd = align && (s.atEnd || nearBottom());
  s.atEnd = false;
  s.live = new LiveReply({ showThinking: !s.learn });
  els.timeline.append(s.live.el);
  clearError();
  if (atEnd) {
    const m = els.messages;
    if (s.live.el.getBoundingClientRect().top > m.getBoundingClientRect().bottom - 64) {
      // Room for the reply to grow into, so its start can sit under the head
      // now and the page never has to move while it streams.
      els.live.style.minBlockSize = `${m.clientHeight}px`;
      alignUnderHead(s.live.el);
    }
  }
  return s.live;
}

function ensureLive({ align = false } = {}) {
  const s = state.current;
  return s.live ?? beginLive(s, { align });
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

/** The inline failure block at the end of the flow: `alert` for failures, `status` for recovery offers. */
function showError({ title = 'The reply failed.', text = '', action = 'Retry', onRetry, role = 'alert' }) {
  const s = state.current;
  const atEnd = nearBottom();
  clearError();
  const el = document.createElement('div');
  el.className = 'turn-error';
  el.setAttribute('role', role);
  const t = document.createElement('p');
  t.className = 'turn-error-title';
  t.textContent = title;
  el.append(t);
  if (text) {
    const p = document.createElement('p');
    p.className = 'turn-error-text';
    p.textContent = text;
    el.append(p);
  }
  const actions = document.createElement('div');
  actions.className = 'actions';
  const b = document.createElement('button');
  b.className = 'btn secondary';
  b.type = 'button';
  b.textContent = action;
  b.addEventListener('click', () => {
    clearError();
    onRetry();
  });
  actions.append(b);
  if (/model|connect|settings/i.test(text)) {
    const st = document.createElement('button');
    st.className = 'btn text';
    st.type = 'button';
    st.textContent = 'Settings';
    st.addEventListener('click', openSettings);
    actions.append(st);
  }
  el.append(actions);
  s.errorEl = el;
  els.live.append(el);
  if (atEnd) el.scrollIntoView({ block: 'nearest', behavior: 'instant' });
  scheduleFrame();
}

function resumeTurn(s) {
  cmd('resume_turn', { sessionId: s.id }).catch((err) => {
    if (state.current === s && !s.errorEl && !running(s.id, 'tutor')) showTurnError(err);
  });
}

function showTurnError(text) {
  const s = state.current;
  showError({ text, onRetry: () => resumeTurn(s) });
}

// ---------- Learn: runtime events ----------

function subscribe() {
  call('subscribe_events', { onEvent: channel(onRuntimeEvent) }).catch((e) => {
    els.liveError.textContent = `Live updates are unavailable: ${message(e)}`;
    els.liveError.hidden = false;
  });
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
    const agents = state.turns.get(id);
    agents?.delete(ev.agent);
    if (agents?.size === 0) state.turns.delete(id);
    if (ev.agent === 'tutor') sessions.bump(id);
  }
  if (state.loading?.id === id) {
    state.loading.events.push(ev);
    return;
  }
  const s = state.current;
  if (s?.learn && s.id === id) sessionEvent(s, ev);
}

function onSessionSummary(summary) {
  const known = sessions.has(summary.id);
  const s = state.current;
  if (state.starting && s?.learn && !s.id && !known && summary.mode === 'learn') {
    s.id = summary.id;
    sessions.upsert(summary);
    setTitle(s, summary.title);
    syncBusy();
    return;
  }
  sessions.upsert(summary);
  if (s?.id === summary.id && summary.title) setTitle(s, summary.title);
}

function sessionEvent(s, ev) {
  switch (ev.kind) {
    case 'turnStart':
      if (ev.agent === 'tutor') {
        s.turnStartedAt = performance.now();
        ensureLive({ align: true });
      } else {
        s.planFailed = false;
      }
      renderPhase();
      renderDesk();
      syncBusy();
      break;
    case 'turnEnd':
      if (ev.agent === 'tutor') endLive(s, ev.error);
      else if (ev.error) plannerFailed(s, ev.error);
      renderPhase();
      renderDesk();
      syncBusy();
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
  s.atEnd = false;
  // The message op (the persisted truth) always arrives before turnEnd and
  // has finalised the reply; what is left is an empty "Writing…" or a
  // view that joined mid-turn.
  dropLive(s);
  releaseReserve();
  if (error) showTurnError(error);
  else if (!(state.lastAnnounceKind === 'exercise' && state.lastAnnounce >= s.turnStartedAt)) announce('Reply finished.');
}

function plannerFailed(s, error) {
  s.planFailed = true;
  const item = { kind: 'notice', key: `planfail:${performance.now()}`, text: `Planning failed: ${error}`, error: true };
  s.tl.items.push(item);
  appendItem(item);
  announce('Planning failed.');
}

function onOp(s, op) {
  if (op.op === 'title' && op.title) setTitle(s, op.title);
  const free = notEngaged();
  const change = applyOp(s.tl, op);
  if (change?.added) {
    const item = change.added;
    if (item.kind === 'assistant') {
      if (s.live) {
        item.el = s.live.finalize(item.msg);
        s.live = null;
      }
      appendItem(item);
    } else {
      if (item.kind === 'user') {
        s.userCount++;
        s.planFailed = false;
        clearError();
      }
      // An exercise ends the streamed segment; drop an empty "Writing…".
      if ((item.kind === 'question' || item.kind === 'choice') && s.live && !s.live.hasContent) dropLive(s);
      if (item.kind === 'user') s.atEnd = nearBottom();
      appendItem(item);
      if (item.kind === 'user') item.el.scrollIntoView({ block: 'nearest', behavior: 'instant' });
      else if (item.kind === 'section') announce(`New section: ${sectionTitle(item, s.st?.plan)}`);
      else if (isPending(item)) exerciseArrived(s, item, free);
    }
  } else if (change?.updated) {
    const item = change.updated;
    rerenderItem(item);
    if (item.kind === 'question' && item.verdict && !item.verdictSaid) {
      item.verdictSaid = true;
      announce(`Feedback: ${verdictText(item.verdict)}.`);
    }
  }
  if (affectsState(op)) scheduleState();
  renderDesk();
  if (!isStreaming()) scheduleFrame();
}

/** A pending question/choice: the desk switches to its note; focus follows only if free. */
function exerciseArrived(s, item, free) {
  renderDesk({ quiet: true });
  if (free) focusQuiet(item.el.querySelector('[data-first]'));
  const text = item.kind === 'question' ? item.args.prompt : item.args.question;
  announce(`${item.kind === 'question' ? 'Question' : 'Choice'}: ${plainText(text)}`, 'exercise');
}

// ---------- Learn: session state, head, plan ----------

function scheduleState() {
  const s = state.current;
  clearTimeout(s.stTimer);
  s.stTimer = setTimeout(() => refreshState(s), 50);
}

async function fetchState(s) {
  try {
    s.st = await call('session_state', { sessionId: s.id });
    s.stError = null;
  } catch (e) {
    s.stError = message(e);
  }
}

async function refreshState(s) {
  if (!s.learn || s.readOnly || !s.id) return;
  await fetchState(s);
  if (state.current !== s) return;
  applyState(s);
}

function applyState(s, { initial = false } = {}) {
  const free = notEngaged();
  renderPhase();
  renderPlanCard({ free, initial });
  renderPanel();
  renderDesk({ quiet: initial, free });
  if (!s.st) return;
  // Section openers seen before any `plan` op take their title from the
  // state: refresh them once per plan version, not on every state refresh.
  if (s.st.plan && s.openersVersion !== s.st.planVersion) {
    s.openersVersion = s.st.planVersion;
    for (const item of s.tl.items) if (item.kind === 'section' && !item.plan && !item.close && item.el) rerenderItem(item);
  }
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
    showError({ title: 'The tutor hasn’t answered this yet.', action: 'Get a reply', onRetry: () => resumeTurn(s), role: 'status' });
  }
}

function renderPhase() {
  const s = state.current;
  if (!s?.learn || s.readOnly) return;
  renderPhaseNav(els.phase, s.st ?? { phase: 'measure' }, {
    planning: running(s.id, 'planner'),
    planFailed: s.planFailed,
    error: s.stError,
  });
}

function renderPlanCard({ free = false, initial = false } = {}) {
  const s = state.current;
  const st = s.st;
  const show = st?.phase === 'awaitApproval' && st.plan;
  const key = show ? `v${st.planVersion}` : '';
  if (key === s.planKey) return;
  const fresh = !s.planKey;
  s.planKey = key;
  if (s.planVersion !== st?.planVersion) {
    s.planVersion = st?.planVersion;
    s.removed.clear();
  }
  const wasInside = els.planSlot.contains(document.activeElement);
  if (!show) {
    els.planSlot.textContent = '';
    return;
  }
  const doc = planCard(st.plan, {
    removed: s.removed,
    draft: s.planDraft,
    formOpen: s.planFormOpen,
    onDraft: (text) => (s.planDraft = text),
    onFormOpen: (open) => (s.planFormOpen = open),
    onApprove: () => cmd('approve', { sessionId: s.id, removedPlugins: removedPluginList(s.removed) }),
    onRequest: (text) => cmd('request_plan_change', { sessionId: s.id, text }),
  });
  els.planSlot.replaceChildren(doc);
  if (initial) return;
  const heading = doc.querySelector('.plan-title');
  if (fresh) {
    if (free) focusQuiet(heading);
    announce('Plan ready for review.');
  } else if (wasInside) focusQuiet(heading);
}

function panelAvailable(s) {
  const st = s?.st;
  return Boolean(s?.learn && !s.readOnly && st?.plan && (st.phase === 'teach' || st.phase === 'close' || st.ended));
}

function planButtonName(st) {
  if (st.ended || st.phase === 'close') return 'Plan (Close)';
  const pos = sectionPosition(st);
  return pos ? `Plan (Teach, section ${pos.n} of ${pos.m})` : 'Plan (Teach)';
}

function renderPanel() {
  const s = state.current;
  if (!panelAvailable(s)) {
    els.planToggle.hidden = true;
    if (els.planPanel.open) els.planPanel.close();
    if (s) s.panelKey = '';
    return;
  }
  const st = s.st;
  els.planToggle.hidden = false;
  els.planToggle.setAttribute('aria-label', planButtonName(st));
  let checks = 0;
  for (const list of Object.values(st.checks ?? {})) checks += list.length;
  const key = [st.planVersion, st.currentSection, st.completedSections.length, st.lastNodeId, checks, st.activePlugins.join(','), st.ended].join('|');
  if (key === s.panelKey) return;
  s.panelKey = key;
  const hadFocus = els.planPanel.contains(document.activeElement);
  const inner = document.createElement('div');
  inner.className = 'sheet-inner';
  inner.append(
    planSheet(st, {
      headingId: 'plan-sheet-heading',
      formOpen: s.panelFormOpen,
      draft: s.panelDraft,
      onDraft: (text) => (s.panelDraft = text),
      onFormOpen: (open) => (s.panelFormOpen = open),
      onRequest: (text) => cmd('request_plan_change', { sessionId: s.id, text }),
      onClose: () => els.planPanel.close(),
    }),
  );
  els.planPanel.replaceChildren(inner);
  if (hadFocus) focusQuiet($('#plan-sheet-heading'));
}

function openPlanSheet() {
  renderPanel();
  if (els.planToggle.hidden || els.planPanel.open) return;
  els.planPanel.showModal();
  focusQuiet($('#plan-sheet-heading'));
}

// ---------- desk ----------

/** The pending question or choice, if any. Read-only sessions have none. */
function pendingItem(s) {
  const last = s.tl.lastExercise;
  return !s.readOnly && last && isPending(last) ? last : null;
}

/** The desk's one state: composer, or a note (with at most one button). */
function deskState(s) {
  if (s.readOnly) return { note: READ_ONLY_NOTE, button: 'Start a new session', act: 'new', primary: true };
  if (!s.learn) return { composer: true };
  if (pendingItem(s)) return { note: 'Answer the question to continue.', button: 'Go to question', act: 'question' };
  const phase = s.st?.phase;
  if (phase === 'awaitApproval') return { note: 'Review the plan to continue.', button: 'Go to plan', act: 'plan' };
  if (phase === 'plan') {
    if (s.planFailed && !running(s.id, 'planner')) return { note: 'Planning failed.' };
    return { note: 'The planner is drafting your plan. You can keep reading.' };
  }
  if (s.planFailed) return { composer: true, note: 'Planning failed. You can describe what you want and try again.' };
  return { composer: true };
}

function renderDesk({ quiet = false, free = notEngaged() } = {}) {
  const s = state.current;
  if (!s) return;
  const d = deskState(s);
  const wasShown = s.composerShown;
  s.composerShown = Boolean(d.composer);
  els.composer.hidden = !d.composer;
  els.deskNote.hidden = !d.note;
  if (d.note && els.deskNoteText.textContent !== d.note) els.deskNoteText.textContent = d.note;
  const b = els.deskNoteButton;
  b.hidden = !d.button;
  if (d.button) {
    if (b.textContent !== d.button) b.textContent = d.button;
    b.dataset.act = d.act;
    b.classList.toggle('primary', Boolean(d.primary));
    b.classList.toggle('secondary', !d.primary);
  }
  // The composer came back (plan approved, question answered): resume there.
  if (!quiet && d.composer && !wasShown && free) focusQuiet(els.prompt);
}

function onDeskNoteButton() {
  const s = state.current;
  const act = els.deskNoteButton.dataset.act;
  if (act === 'new') {
    state.mode = 'learn';
    showHome();
    return;
  }
  let target = null;
  if (act === 'question') target = pendingItem(s)?.el?.querySelector('[data-first]');
  else if (act === 'plan') target = els.planSlot.querySelector('.plan-title');
  if (!target) return;
  target.scrollIntoView({ block: 'center', behavior: 'instant' });
  focusQuiet(target);
}

// ---------- opening sessions ----------

async function openSession(id) {
  if (state.current?.id === id) return;
  const loading = { id, events: [], wasRunning: running(id, 'tutor') };
  state.loading = loading;
  const summary = sessions.get(id);
  const wantsState = summary?.mode === 'learn' && !summary.readOnly;
  let data;
  let st = null;
  try {
    [data, st] = await Promise.all([
      call('load_session', { id }),
      wantsState ? call('session_state', { sessionId: id }).catch((e) => ({ error: message(e) })) : null,
      ensureKatex().catch(() => {}),
    ]);
  } catch (e) {
    if (state.loading === loading) state.loading = null;
    sessions.setRowStatus(id, `Couldn’t open this session: ${message(e)}`);
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
  if (st?.error) s.stError = st.error;
  else if (st) s.st = st;
  state.current = s;
  showConversation();
  mountTimeline();
  if (s.learn && !s.readOnly) {
    if (loading.wasRunning) ensureLive().midway = true;
    if (!s.st && !s.stError) await fetchState(s);
    if (state.current !== s) return;
    applyState(s, { initial: true });
    state.loading = null;
    for (const ev of loading.events) sessionEvent(s, ev);
  } else {
    state.loading = null;
    const last = s.messages.at(-1);
    if (!s.learn && last?.role === 'user') {
      showError({ title: 'The tutor hasn’t answered this yet.', action: 'Get a reply', onRetry: () => runPractice(null), role: 'status' });
    }
  }
  fillViewport();
  resume(s);
  syncBusy();
}

/**
 * Resumption cue on open: align the last tutor entry under the head (or the
 * pending exercise / plan if it would not fit below it) and focus the
 * learner's next action.
 */
function resume(s) {
  const m = els.messages;
  const tutors = els.timeline.querySelectorAll('.entry.tutor');
  const last = tutors[tutors.length - 1] ?? null;
  const pending = pendingItem(s)?.el ?? null;
  const plan = els.planSlot.querySelector('.plan-doc');
  const block = pending ?? plan;
  let target = last;
  if (block && (!last || last.compareDocumentPosition(block) & Node.DOCUMENT_POSITION_FOLLOWING)) {
    const room = m.clientHeight - ALIGN_GAP;
    if (!last || block.getBoundingClientRect().bottom - last.getBoundingClientRect().top > room) target = block;
  }
  if (target) alignUnderHead(target);
  let focus;
  if (pending) focus = pending.querySelector('[data-first]');
  else if (plan) focus = plan.querySelector('.plan-title');
  else if (s.readOnly) focus = els.deskNoteButton;
  else focus = els.prompt;
  focusQuiet(focus);
}

// ---------- sending ----------

async function startLearn(text) {
  closeCurrent();
  const s = newView({ id: null, mode: 'learn', title: text });
  state.current = s;
  state.starting = true;
  showConversation();
  mountTimeline();
  beginLive(s);
  s.turnStartedAt = performance.now();
  syncBusy();
  focusQuiet(els.prompt);
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
      setPromptError(message(e));
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
      setPromptError(message(e));
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
    focusQuiet(els.prompt);
    ensureKatex().catch(() => {});
  }
  const s = state.current;
  if (text !== null) {
    s.messages.push({ role: 'user', content: text });
    const item = { kind: 'user', key: `m${s.messages.length - 1}`, msg: { role: 'user', content: text } };
    s.tl.items.push(item);
    s.atEnd = !isNew && nearBottom();
    appendItem(item);
    if (!isNew) item.el.scrollIntoView({ block: 'nearest', behavior: 'instant' });
  }
  const live = beginLive(s, { align: !isNew });
  state.practiceBusy = true;
  syncBusy();

  const onEvent = channel((ev) => {
    if (ev.kind === 'session') {
      if (!s.id) s.id = ev.session.id;
      sessions.upsert(ev.session);
      setTitle(s, ev.session.title);
      updateSendState();
      return;
    }
    if (ev.kind === 'text') live.addText(ev.delta);
    else if (ev.kind === 'thinking') live.addThinking(ev.delta);
  });

  let ok = false;
  try {
    const stored = text === null
      ? await call('retry_reply', { sessionId: s.id, onEvent })
      : await call('send_message', { sessionId: isNew ? null : s.id, mode: 'practice', text, onEvent });
    s.messages.push(stored);
    const item = { kind: 'assistant', key: `m${s.messages.length - 1}`, msg: stored };
    item.el = live.finalize(stored);
    if (s.live === live) s.live = null;
    s.tl.items.push(item);
    appendItem(item);
    ok = true;
  } catch (e) {
    if (s.live === live) dropLive(s);
    else live.dispose();
    if (isNew && !s.id) {
      state.practiceBusy = false;
      showHome();
      els.prompt.value = text;
      autoGrow();
      setPromptError(message(e));
    } else if (state.current === s) {
      showError({ text: message(e), onRetry: () => runPractice(null) });
    }
  } finally {
    state.practiceBusy = false;
    if (state.current === s) headStatus('');
    syncBusy();
    if (s.id) sessions.bump(s.id);
  }
  if (ok && state.current === s) announce('Reply finished.');
}

function stop() {
  const s = state.current;
  if (s?.id) call('cancel_reply', { sessionId: s.id }).catch((e) => setPromptError(message(e)));
}

function submit(event) {
  event.preventDefault();
  if (isStreaming()) {
    stop();
    return;
  }
  const text = els.prompt.value.trim();
  if (!text) return;
  if (!modelReady()) {
    setPromptError('Choose a model first.');
    openSettings();
    return;
  }
  els.prompt.value = '';
  autoGrow();
  setPromptError('');
  updateSendState();
  const s = state.current;
  if (!s) {
    if (state.mode === 'learn') startLearn(text);
    else runPractice(text);
  } else if (s.learn) sendLearner(text);
  else runPractice(text);
}

function goHome() {
  if (state.practiceBusy) {
    headStatus(GUARD);
    return;
  }
  showHome({ focusId: state.current?.id });
}

// ---------- Map ----------

async function showMap() {
  if (!els.home.hidden) state.homeScroll = els.home.scrollTop;
  els.home.hidden = true;
  els.mapView.hidden = false;
  els.mapError.textContent = '';
  focusQuiet(els.mapHeading);
  const visit = ++state.mapLoad;
  map.loading();
  try {
    const plans = await call('learning_map');
    if (visit === state.mapLoad && !els.mapView.hidden) map.render(plans);
  } catch (e) {
    if (visit === state.mapLoad && !els.mapView.hidden) map.failed(message(e), showMap);
  }
}

function renderMapDesk(count) {
  els.mapCount.textContent = count ? `${count} selected` : 'Nothing selected';
  els.mapClear.hidden = !count;
  els.mapPractise.disabled = !count;
  els.mapPractise.textContent = count ? `Practise ${count} ${count === 1 ? 'idea' : 'ideas'}` : 'Practise';
  if (count) els.mapError.textContent = '';
}

/** Starts a Practice session on the selected ideas, mixed rather than blocked. */
function practiseSelection() {
  const picked = map.selection();
  if (!picked.length) return;
  if (!modelReady()) {
    els.mapError.textContent = 'Choose a model first.';
    openSettings();
    return;
  }
  // The first line doubles as the session title until the generated one arrives.
  const lines = picked.map(({ title, goal }) => `- ${title} (from “${goal}”)`);
  const text = [
    `Practise: ${picked.map((p) => p.title).join(', ')}`,
    '',
    'Mix them together rather than one at a time. From my Learn sessions:',
    ...lines,
  ].join('\n');
  map.clear();
  runPractice(text);
}

// ---------- wiring ----------

function wire() {
  els.page = $('#page');
  els.composer.addEventListener('submit', submit);
  els.prompt.addEventListener('input', () => {
    autoGrow();
    updateSendState();
    if (els.promptError.textContent) setPromptError('');
  });
  els.prompt.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.isComposing) {
      e.preventDefault();
      if (!isStreaming()) els.composer.requestSubmit();
    }
  });
  els.modes.addEventListener('change', (e) => {
    if (e.target.name === 'mode') setMode(e.target.value);
  });
  els.back.addEventListener('click', goHome);
  els.deskNoteButton.addEventListener('click', onDeskNoteButton);
  els.continueBtn.addEventListener('click', continueReading);
  els.messages.addEventListener('scroll', scheduleFrame, { passive: true });
  els.messages.addEventListener('click', (e) => {
    const a = e.target.closest('a[href]');
    if (!a) return;
    e.preventDefault();
    const url = a.getAttribute('href');
    if (url.startsWith('https://')) {
      call('open_external', { url }).catch((err) => headStatus(`Couldn’t open the link: ${message(err)}`));
    }
  });
  els.planToggle.addEventListener('click', openPlanSheet);
  els.planPanel.addEventListener('close', () => {
    if (!els.planToggle.hidden) focusQuiet(els.planToggle);
  });
  els.planPanel.addEventListener('click', (e) => {
    if (e.target === els.planPanel) els.planPanel.close();
  });
  $('#open-settings').addEventListener('click', openSettings);
  $('#setup-button').addEventListener('click', openSettings);
  $('#sessions-retry').addEventListener('click', loadSessions);
  els.openMap.addEventListener('click', showMap);
  els.mapBack.addEventListener('click', () => showHome({ focus: els.openMap }));
  els.mapClear.addEventListener('click', () => {
    map.clear();
    focusQuiet(els.mapPractise.disabled ? els.mapHeading : els.mapPractise);
  });
  els.mapPractise.addEventListener('click', practiseSelection);
  initSettings({ changed: applyConfig });
}

async function start() {
  wire();
  showHome();
  if (!available) {
    els.start.hidden = true;
    els.openMap.hidden = true;
    els.unavailable.hidden = false;
    sessions.render();
    return;
  }
  subscribe();
  const [config] = await Promise.all([loadConfig().catch((e) => ({ error: message(e) })), loadSessions()]);
  if (config.error) setPromptError(`Couldn’t load your settings: ${config.error}`);
  else applyConfig(config);
  // Warm KaTeX once the window is idle, not on the startup path.
  (window.requestIdleCallback ?? ((f) => setTimeout(f, 1500)))(() => ensureKatex().catch(() => {}));
}

start();
