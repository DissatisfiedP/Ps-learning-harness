// P's Learning Harness UI ("Carrel"): Home (start + session list) and the
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
import { initComposer } from './composer.js';
import { artifactsEl } from './content.js';
import { ensurePrism } from './highlight.js';
import { ensureKatex, plainText } from './markdown.js';
import { enterSession, initFocus, leaveSession, NUDGE_TEXT, nudgeEl, refreshFullscreen, takeNudge, toggleFullscreen } from './focus.js';
import { initSettings, loadConfig, openSettings } from './settings.js';
import { initSync } from './sync.js';
import { LiveReply, assistantEl, userEl } from './reply.js';
import { affectsState, applyHuntStates, applyOp, createTimeline, isPending } from './timeline.js';
import { createSessionList } from './sessions.js';
import { createMap } from './map.js';
import { installCandidate, loadCatalog, pluginLabel } from './pluginui.js';
import { getPlugin, loadPlugins, pluginIds } from './pluginstore.js';
import {
  choiceCard,
  closeExtras,
  dividerEl,
  huntCard,
  jobChip,
  newPlanEdits,
  noticeEl,
  planCard,
  planOverlay,
  questionCard,
  renderPhase as renderPhaseNav,
  sectionOpener,
  sectionPosition,
  sectionTitle,
  verdictText,
  placeText,
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
  resumeSession: $('#resume-session'),
  resumeTitle: $('#resume-title'),
  resumePlace: $('#resume-place'),
  conversation: $('#conversation'),
  back: $('#back'),
  title: $('#session-title'),
  phase: $('#phase'),
  headWord: $('#head-word'),
  headStatus: $('#head-status'),
  headWorkspace: $('#head-workspace'),
  headWorkspaceName: $('#head-workspace-name'),
  sessionMenuWrap: $('#session-menu-wrap'),
  sessionMenu: $('#session-menu'),
  sessionMenuList: $('#session-menu-list'),
  workspaceAttach: $('#workspace-attach'),
  workspaceDetach: $('#workspace-detach'),
  fullscreenToggle: $('#fullscreen-toggle'),
  workspaceStatus: $('#workspace-status'),
  planToggle: $('#plan-toggle'),
  messages: $('#messages'),
  timeline: $('#timeline'),
  planSlot: $('#plan-slot'),
  live: $('#live'),
  continueBtn: $('#continue-reading'),
  deskNote: $('#desk-note'),
  deskNoteText: $('#desk-note-text'),
  deskNoteButton: $('#desk-note-button'),
  deskComment: $('#desk-comment'),
  commentToggle: $('#desk-comment-toggle'),
  commentForm: $('#desk-comment-form'),
  commentInput: $('#desk-comment-input'),
  commentStatus: $('#desk-comment-status'),
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
    placeholder: 'A topic or goal…',
  },
  practice: {
    hero: 'What do you want to practise?',
    placeholder: 'A topic or problem set…',
  },
};

const READ_ONLY_NOTE = 'Made with an older tutor. Read-only.';
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
  mapLoad: 0, // increments per Map visit; stale knowledge_map results are dropped
};

function newView(summary) {
  return {
    id: summary.id ?? null,
    mode: summary.mode,
    title: summary.title ?? '',
    readOnly: Boolean(summary.readOnly),
    readOnlyReason: summary.readOnlyReason ?? null, // 'legacy' | 'desktop-plugin'
    desktopOnlyPlugins: summary.desktopOnlyPlugins ?? [],
    lockCheck: false, // a desktop-only plugin was used this turn (mobile lock)
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
    panelLevel: 'ideas', // the plan overlay opens on the claims; Topics is the overview
    panelDrawer: false, // the overlay's section drawer
    panelFromCard: false, // opened from the plan card's corner while awaiting approval
    planEdits: newPlanEdits(), // unsent plan-control edits (cards.js)
    planEditsRev: 0, // bumps when pending edits are discarded
    planDraft: '',
    planFormOpen: false,
    panelKey: '',
    panelDraft: '',
    panelFormOpen: false,
    planFailed: false,
    planEditing: false, // a plan-change request is in flight
    composerShown: true,
    turnStartedAt: 0,
    openersVersion: null, // plan version section openers were last refreshed for
    atEnd: false, // learner was at the end when their last message was added
    userCount: 0,
    checkedUnanswered: false,
  };
}

let composer = null;

const sessions = createSessionList(els.sessionList, {
  onOpen: (id) => openSession(id),
  onDelete: (id) => deleteSession(id),
  onExtractStatus: (id) => cmd('graph_status', { sessionId: id }),
  // The row's status line (role="status") reports the outcome.
  onExtract: async (id) => {
    await cmd('graph_build', { sessionId: id });
  },
});

const map = createMap(els.mapPage, {
  onChange: renderMapDesk,
  onReviewDecided: () => showMap(),
  onStartFrontier: (itemId, button) => startFromFrontier(itemId, button),
});

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
  refreshVision();
}

// ---------- Home ----------

function setMode(mode) {
  state.mode = mode;
  for (const r of els.modes.querySelectorAll('input')) r.checked = r.value === mode;
  const copy = COPY[mode];
  els.hero.textContent = copy.hero;
  if (!state.current) els.prompt.placeholder = copy.placeholder;
  composer?.setMode(mode);
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
  renderResume();
}

/**
 * Home's resumption cue (§7.9): the newest session, if it can go on, one
 * click away. Learn sessions need `session_state` to know whether they ended.
 */
async function renderResume() {
  const latest = sessions.newest();
  const ask = (state.resumeAsk = {});
  let place = null;
  if (latest && !latest.readOnly) {
    if (latest.mode === 'practice') place = 'Practice';
    else {
      const st = await call('session_state', { sessionId: latest.id }).catch(() => null);
      if (st && !st.ended) place = placeText(st);
    }
  }
  if (state.resumeAsk !== ask) return;
  els.resumeSession.hidden = !place;
  if (!place) return;
  const title = latest.title || 'Untitled';
  els.resumeSession.dataset.id = latest.id;
  els.resumeTitle.textContent = title;
  els.resumePlace.textContent = place;
  els.resumeSession.setAttribute('aria-label', `Continue ${title}, ${place}`);
}

async function deleteSession(id) {
  await cmd('delete_session', { id });
  state.turns.delete(id);
  const next = sessions.remove(id);
  focusQuiet(next ?? els.prompt);
  renderResume();
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
    composer?.setSession(null);
    composer?.setMode(state.mode);
  } else {
    els.dockSlot.append(els.composer);
    els.prompt.setAttribute('aria-labelledby', 'prompt-label');
    els.prompt.placeholder = 'Reply to the tutor';
    composer?.setSession(state.current?.id ?? null);
    // Practice sessions take images only (ui-design §7.3).
    composer?.setMode(state.current && !state.current.learn ? 'practice' : 'learn');
  }
  els.composer.hidden = false;
  setPromptError('');
}

/** Shows Home. `focusId`: the session just left (its row gets focus); `focus`: an element to focus instead. */
function showHome({ focusId = null, focus = null } = {}) {
  closeCurrent();
  leaveSession();
  state.loading = null;
  els.conversation.hidden = true;
  els.mapView.hidden = true;
  els.home.hidden = false;
  setComposerContext(true);
  setMode(state.mode);
  syncBusy();
  els.home.scrollTop = state.homeScroll;
  focusQuiet(focus ?? ((focusId && sessions.openButton(focusId)) || els.prompt));
  renderResume();
}

function showConversation() {
  const s = state.current;
  if (!els.home.hidden) state.homeScroll = els.home.scrollTop;
  els.home.hidden = true;
  els.mapView.hidden = true;
  els.conversation.hidden = false;
  enterSession();
  els.title.textContent = s.title;
  const learnLive = s.learn && !s.readOnly;
  els.phase.hidden = !learnLive;
  els.phase.textContent = '';
  els.headWord.hidden = learnLive;
  els.headWord.textContent = s.readOnly ? 'Read-only' : 'Practice';
  els.planToggle.hidden = true;
  headStatus('');
  workspaceStatus('');
  renderWorkspace(s);
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
  const ctx = { artifacts: s.tl.artifacts, sessionId: s.id };
  switch (item.kind) {
    case 'user':
      return userEl(item.msg.content, item.msg.attachments);
    case 'assistant':
      return assistantEl(item.msg, { showThinking: !s.learn, artifacts: item.artifacts ?? [], ctx });
    case 'artifacts':
      return artifactsEl(item.ops, ctx);
    case 'notice':
      return noticeEl(item);
    case 'divider':
      return dividerEl(item.text);
    case 'nudge':
      return nudgeEl(item.text);
    case 'section':
      return sectionOpener(item, s.st?.plan);
    case 'job':
      return jobChip(item, { onCancel: s.readOnly ? null : (jobId) => cmd('cancel_job', { sessionId: s.id, jobId }) });
    case 'question':
      return questionCard(item, { readOnly: s.readOnly, onAnswer, ctx });
    case 'choice':
      return choiceCard(item, { readOnly: s.readOnly, onAnswer });
    case 'hunt':
      return huntCard(item, {
        readOnly: s.readOnly,
        current: s.st?.currentSection ?? null,
        onReturn: (huntId, payload) => cmd('hunt_return', { sessionId: s.id, huntId, payload }),
        onSkip: (huntId) => cmd('hunt_skip', { sessionId: s.id, huntId }),
      });
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
  s.live = new LiveReply({
    showThinking: !s.learn,
    ctx: () => ({ artifacts: s.tl.artifacts, sessionId: s.id }),
  });
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
    st.addEventListener('click', () => openSettings({ to: 'model' }));
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
  if (ev.kind === 'repoChanged') {
    onRepoChanged();
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
    composer?.setSession(summary.id);
    syncBusy();
    return;
  }
  sessions.upsert(summary);
  if (s?.id === summary.id && summary.title) setTitle(s, summary.title);
}

/**
 * Sync brought another device's work in: re-read the session list, and the
 * open session when its file changed and nothing is in flight in it (a
 * streaming reply or a plan edit keeps its view; the next sync or reopen
 * catches up).
 */
async function onRepoChanged() {
  const s = state.current;
  const before = s?.id ? sessions.get(s.id)?.updatedAt : null;
  await loadSessions();
  if (!s?.id || state.current !== s || state.loading) return;
  const after = sessions.get(s.id)?.updatedAt;
  if (!after || after === before) return;
  if (isStreaming() || s.planEditing || state.turns.has(s.id)) return;
  const draft = els.prompt.value;
  await openSession(s.id, { reload: true });
  if (state.current?.id === s.id && draft && !els.prompt.value) {
    els.prompt.value = draft;
    updateSendState();
  }
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
      if (ev.agent !== 'tutor') renderPlanCard({ free: false });
      syncBusy();
      break;
    case 'turnEnd':
      if (ev.agent === 'tutor') {
        endLive(s, ev.error);
        if (s.lockCheck) recheckLock(s);
      } else {
        s.planEditing = false;
        if (ev.error) plannerFailed(s, ev.error);
      }
      renderPhase();
      renderDesk();
      if (ev.agent !== 'tutor') renderPlanCard({ free: false });
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
  else {
    if (!(state.lastAnnounceKind === 'exercise' && state.lastAnnounce >= s.turnStartedAt)) announce('Reply finished.');
    maybeNudge(s);
  }
}

/** Break nudge (ui-design §7.8): a UI-only line at a turn boundary, ahead of `before` when given. */
function maybeNudge(s, before = null) {
  if (state.current !== s || !takeNudge()) return;
  const item = { kind: 'nudge', key: `nudge:${performance.now()}`, text: NUDGE_TEXT };
  const at = before ? s.tl.items.indexOf(before) : -1;
  if (at >= 0) s.tl.items.splice(at, 0, item);
  else s.tl.items.push(item);
  appendItem(item);
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
  // A plugin call or figure from a plugin that doesn't run on phones: on a
  // phone the session locks once the turn ends (recheckLock).
  if ((op.op === 'tool' || op.op === 'artifact') && Array.isArray(op.platforms) && !op.platforms.includes('mobile')) {
    s.lockCheck = true;
  }
  const free = notEngaged();
  const change = applyOp(s.tl, op);
  if (change?.added) {
    for (const extra of change.extras ?? []) appendItem(extra);
    const item = change.added;
    if (item.kind === 'assistant') {
      if (s.live) {
        item.el = s.live.finalize(item.msg, item.artifacts ?? []);
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
      if (item.kind === 'section') maybeNudge(s, item);
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
  renderWorkspace(s);
  renderPhase();
  renderPlanCard({ free, initial });
  renderPanel();
  renderDesk({ quiet: initial, free });
  if (!s.st) return;
  // Every hunt card's lifecycle (out / returned / skipped / debriefed) is
  // `session_state`'s, resolved per card by hunt id: the card itself came from
  // the `hunt_send` op, and an earlier section's card must not read as live.
  for (const item of applyHuntStates(s.tl, s.st.hunts)) {
    if (item.el) rerenderItem(item);
  }
  renderCloseExtras(s).catch(() => {});
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

/**
 * The close's reference material below the close text (ui-design §7.7.5):
 * where the field lives, how confidence matched results, and what this session
 * parked. Only once the session has ended; harmless when every part is absent.
 */
async function renderCloseExtras(s) {
  const st = s.st;
  if (!st?.ended) {
    s.closeExtras?.remove();
    s.closeExtras = null;
    return;
  }
  if (!s.frontierLoaded) {
    s.frontierLoaded = true;
    try {
      const all = await call('frontier');
      s.frontier = [...(all ?? [])].sort((a, b) => String(b.at).localeCompare(String(a.at)));
    } catch {
      s.frontier = []; // the frontier is optional; the close still shows its guide
    }
    if (state.current !== s) return;
  }
  const node = closeExtras({
    venues: st.brief?.venues ?? [],
    depth: st.plan?.depth ?? '',
    calibration: st.calibration ?? null,
    frontier: (s.frontier ?? []).filter((f) => f.sessionId === s.id),
    onStartFrontier: (itemId, button) => startFromFrontier(itemId, button),
  });
  if (!node) {
    s.closeExtras?.remove();
    s.closeExtras = null;
    return;
  }
  if (s.closeExtras?.isConnected) s.closeExtras.replaceWith(node);
  else els.timeline.append(node);
  s.closeExtras = node;
}

/** Starts a fresh Learn session from a parked frontier item (Map or close). */
async function startFromFrontier(itemId, button) {
  if (button) button.disabled = true;
  if (!modelReady()) {
    headStatus('');
    openSettings({ to: 'model' });
    if (button) button.disabled = false;
    return;
  }
  try {
    const created = await cmd('start_from_frontier', { itemId });
    const id = typeof created === 'string' ? created : created?.id ?? created?.sessionId ?? null;
    await loadSessions();
    const target = id ?? sessions.newest()?.id ?? null;
    if (target) openSession(target);
  } catch (e) {
    if (button) button.disabled = false;
    headStatus(`Couldn’t start the session: ${message(e)}`);
  }
}

/**
 * A forked or merge session the tutor has not run yet waits for the learner:
 * the status bar offers "Start" (ui-design §7.1.1). True when it showed.
 */
function offerBranchStart(s) {
  const lineage = sessions.get(s.id)?.lineage;
  if (!lineage || lineage.started || lineage.ended || s.st.phase !== 'teach' || s.errorEl) return false;
  showError({
    title:
      lineage.kind === 'merge'
        ? 'The merge session is ready: it starts with a short revision.'
        : `This branch is ready: ${lineage.trackTitle}.`,
    action: 'Start',
    onRetry: () => resumeTurn(s),
    role: 'status',
  });
  return true;
}

/** After a crash or a failed turn, the last learner message may have no reply. */
function offerReplyIfUnanswered(s) {
  const st = s.st;
  if (running(s.id, 'tutor') || running(s.id, 'planner') || st.ended || st.pendingCall) return;
  if (st.phase !== 'measure' && st.phase !== 'teach') return;
  if (offerBranchStart(s)) return;
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

/**
 * Title first (§6.2): when the title would truncate or the head would wrap,
 * the phase list shows only the current step (the others stay for screen
 * readers). Runs from a ResizeObserver on the head, its title and the list.
 */
function fitHead() {
  const nav = els.phase;
  if (nav.hidden || els.conversation.hidden) return;
  nav.classList.remove('compact');
  const t = els.title;
  const top = t.offsetTop;
  const wraps = [...t.parentElement.children].some(
    (c) => !c.hidden && !c.classList.contains('head-status') && c.offsetTop > top + t.offsetHeight / 2,
  );
  if (wraps || t.scrollWidth > t.clientWidth + 1) nav.classList.add('compact');
}

// ---------- plugins (plan chip, Settings changes) ----------

async function installPlanPlugin(s, plugin, chip) {
  if (chip) chip.disabled = true;
  try {
    const info = await installCandidate(plugin);
    if (!info) {
      if (chip) chip.disabled = false;
      return;
    }
    await loadPlugins(true);
    await loadCatalog(true);
    announce(`${info.title || plugin} installed.`);
    renderPlanCard({ free: false });
  } catch (e) {
    const status = els.planSlot.querySelector('.form-status');
    if (status) {
      status.textContent = message(e);
      status.classList.add('error');
    }
    if (chip) chip.disabled = false;
  }
}

async function onPluginsChanged() {
  await loadPlugins(true);
  await loadCatalog(true);
  const s = state.current;
  if (s?.learn) {
    renderPlanCard({ free: false });
  }
}

function refreshVision() {
  call('vision_capabilities')
    .then((v) => composer?.setVision(v))
    .catch(() => {});
}

// ---------- workspace (session folder) ----------

function workspaceStatus(text, error = false) {
  els.workspaceStatus.textContent = text;
  els.workspaceStatus.classList.toggle('danger', Boolean(error));
}

function renderWorkspace(s) {
  const availableHere = Boolean(s?.learn && !s.readOnly);
  // More is in every session view (full screen); workspace items only where a workspace can attach.
  els.sessionMenuWrap.hidden = false;
  els.workspaceAttach.hidden = !availableHere;
  els.workspaceDetach.hidden = true;
  if (!availableHere) {
    els.headWorkspace.hidden = true;
    return;
  }
  // The name comes from the op log (every device); the path is device-local.
  const name = s.st?.workspace ?? null;
  els.headWorkspace.hidden = !name;
  els.headWorkspaceName.textContent = name ?? '';
  els.headWorkspace.title = name ? `Workspace: ${name}` : '';
  els.workspaceAttach.textContent = name ? 'Attach a different folder…' : 'Attach folder…';
  els.workspaceDetach.hidden = !name;
  if (!name) return;
  call('workspace_path', { sessionId: s.id })
    .then((path) => {
      if (state.current !== s || s.st?.workspace !== name || path) return;
      // Attached on another device: say so, and offer to point at a folder here.
      els.headWorkspaceName.textContent = `${name} (not on this device)`;
      els.headWorkspace.title = `On another device. More → Attach folder… to pick it here.`;
      els.workspaceAttach.textContent = 'Attach folder…';
    })
    .catch(() => {});
}

function closeSessionMenu({ restoreFocus = false } = {}) {
  if (els.sessionMenuList.hidden) return;
  els.sessionMenuList.hidden = true;
  els.sessionMenu.setAttribute('aria-expanded', 'false');
  if (restoreFocus) els.sessionMenu.focus({ preventScroll: true });
}

function openSessionMenu({ focusFirst = false } = {}) {
  els.sessionMenuList.hidden = false;
  els.sessionMenu.setAttribute('aria-expanded', 'true');
  refreshFullscreen();
  if (focusFirst) {
    const first = [...els.sessionMenuList.querySelectorAll('[role="menuitem"]')].find((b) => !b.hidden);
    first?.focus({ preventScroll: true });
  }
}

async function attachWorkspaceFlow() {
  const s = state.current;
  if (!s?.id) return;
  // Focus is on the menu item being hidden; give it back to the menu button.
  closeSessionMenu({ restoreFocus: true });
  workspaceStatus('Choosing a folder…');
  let path;
  try {
    path = await call('pick_folder');
  } catch (e) {
    if (/no folder chosen/i.test(message(e))) {
      workspaceStatus('');
      return;
    }
    workspaceStatus(`Couldn’t open the folder picker: ${message(e)}`, true);
    return;
  }
  workspaceStatus('Attaching…');
  try {
    const name = await call('attach_workspace', { sessionId: s.id, path });
    await refreshState(s);
    workspaceStatus('');
    announce(`Workspace attached: ${name}.`);
    if (!s.composerShown) els.deskNoteButton.focus({ preventScroll: true });
  } catch (e) {
    workspaceStatus(`Couldn’t attach the folder: ${message(e)}`, true);
  }
}

async function detachWorkspaceFlow() {
  const s = state.current;
  if (!s?.id) return;
  closeSessionMenu({ restoreFocus: true });
  workspaceStatus('Detaching…');
  try {
    await call('detach_workspace', { sessionId: s.id });
    await refreshState(s);
    workspaceStatus('');
    announce('Workspace detached.');
  } catch (e) {
    workspaceStatus(`Couldn’t detach the workspace: ${message(e)}`, true);
  }
}

async function useMentionedWorkspace({ path }) {
  const s = state.current;
  if (!s?.id) return;
  workspaceStatus('Attaching…');
  try {
    const name = await call('attach_workspace', { sessionId: s.id, path });
    await refreshState(s);
    workspaceStatus('');
    announce(`Workspace attached: ${name}.`);
  } catch (e) {
    workspaceStatus(`Couldn’t attach the workspace: ${message(e)}`, true);
  }
}

function wireSessionMenu() {
  els.sessionMenu.addEventListener('click', () => {
    if (els.sessionMenuList.hidden) openSessionMenu();
    else closeSessionMenu();
  });
  els.sessionMenu.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      openSessionMenu({ focusFirst: true });
    } else if (e.key === 'Escape') {
      closeSessionMenu();
    }
  });
  els.sessionMenuList.addEventListener('keydown', (e) => {
    const items = [...els.sessionMenuList.querySelectorAll('[role="menuitem"]')].filter((b) => !b.hidden);
    const index = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const next = e.key === 'ArrowDown' ? (index + 1) % items.length : (index - 1 + items.length) % items.length;
      items[next]?.focus({ preventScroll: true });
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      items[e.key === 'Home' ? 0 : items.length - 1]?.focus({ preventScroll: true });
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeSessionMenu({ restoreFocus: true });
    } else if (e.key === 'Tab') {
      closeSessionMenu();
    }
  });
  els.workspaceAttach.addEventListener('click', attachWorkspaceFlow);
  els.workspaceDetach.addEventListener('click', detachWorkspaceFlow);
  els.fullscreenToggle.addEventListener('click', () => {
    closeSessionMenu({ restoreFocus: true });
    toggleFullscreen();
  });
  document.addEventListener('click', (e) => {
    if (!els.sessionMenuWrap.contains(e.target)) closeSessionMenu();
  });
}

function renderPlanCard({ free = false, initial = false } = {}) {
  const s = state.current;
  const st = s.st;
  const show = st?.phase === 'awaitApproval' && st.plan;
  const busy = Boolean(show && (s.planEditing || running(s.id, 'planner')));
  const installed = pluginIds();
  const question = st?.pendingPlanQuestion ?? null;
  // Installing a plugin from the card re-renders it without re-announcing;
  // so does discarding pending plan edits.
  const key = show
    ? `v${st.planVersion}|${busy ? 1 : 0}|${[...installed].sort().join(',')}|${question?.callId ?? ''}|e${s.planEditsRev}|r${(st.knownRefuted ?? []).join(',')}`
    : '';
  if (key === s.planKey) return;
  const fresh = !s.planKey;
  s.planKey = key;
  if (s.planVersion !== st?.planVersion) {
    s.planVersion = st?.planVersion;
    s.planEdits = newPlanEdits();
    s.planEditing = false;
  }
  const wasInside = els.planSlot.contains(document.activeElement);
  if (!show) {
    els.planSlot.textContent = '';
    return;
  }
  const planPlugins = st.plan.sections.flatMap((sec) => sec.plugins ?? []);
  const labels = new Map(planPlugins.map((id) => [id, getPlugin(id)?.title || pluginLabel(id)]));
  const doc = planCard(st.plan, {
    edits: s.planEdits,
    onSendEdits: async (edits, comment) => {
      s.planEditing = true;
      try {
        await cmd('edit_plan', { sessionId: s.id, edits, comment });
      } catch (e) {
        s.planEditing = false;
        throw e;
      }
    },
    onDiscard: () => {
      s.planEdits = newPlanEdits();
      s.planEditsRev += 1;
      renderPlanCard({ free: false });
    },
    onHuntToggle: (sectionId, enabled) => cmd('set_hunt', { sessionId: s.id, sectionId, enabled }).then(() => scheduleState()),
    question,
    onAnswerQuestion: async (answer) => {
      await cmd('answer_plan_question', { sessionId: s.id, answer });
    },
    knownRefuted: st.knownRefuted ?? [],
    draft: s.planDraft,
    formOpen: s.planFormOpen,
    onDraft: (text) => (s.planDraft = text),
    onFormOpen: (open) => (s.planFormOpen = open),
    // The card's corner button opens the whole-screen plan (the same drawing).
    onExpand: () => openPlanSheet({ fromCard: true }),
    // Tools are edited through the boxes' "⋯" now, so approval removes nothing
    // by itself.
    onApprove: () => cmd('approve', { sessionId: s.id, removedPlugins: [] }),
    onRequest: async (text) => {
      s.planEditing = true;
      try {
        await cmd('request_plan_change', { sessionId: s.id, text });
      } catch (e) {
        s.planEditing = false;
        throw e;
      }
      renderPlanCard({ free: false });
    },
    installed,
    labels,
    busy,
    onInstall: (plugin, chip) => installPlanPlugin(s, plugin, chip),
    state: st,
  });
  els.planSlot.replaceChildren(doc);
  if (initial) return;
  const heading = doc.querySelector('.plan-title');
  if (fresh) {
    if (free) focusQuiet(heading);
    announce('Plan ready for review.');
  } else if (wasInside) focusQuiet(heading);
}

/**
 * Whether the whole-screen plan is reachable. Teaching, closing and finished
 * sessions reach it from the head's "Plan" button; a plan awaiting approval
 * reaches it from the card's corner "Expand" (owner direction, 2026-11-07), so
 * the head button stays hidden there — the plan is already on the page.
 */
function panelAvailable(s) {
  const st = s?.st;
  if (!s?.learn || s.readOnly || !st?.plan) return false;
  if (st.phase === 'awaitApproval') return Boolean(s.panelFromCard);
  return st.phase === 'teach' || st.phase === 'close' || st.ended;
}

/** The head's "Plan" button only exists once the plan is approved. */
function planButtonShown(s) {
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
  els.planToggle.hidden = !planButtonShown(s);
  if (!panelAvailable(s)) {
    if (els.planPanel.open) els.planPanel.close();
    if (s) s.panelKey = '';
    return;
  }
  const st = s.st;
  if (!els.planToggle.hidden) els.planToggle.setAttribute('aria-label', planButtonName(st));
  let checks = 0;
  for (const list of Object.values(st.checks ?? {})) checks += list.length;
  const key = [st.planVersion, st.currentSection, st.completedSections.length, st.lastNodeId, checks, st.activePlugins.join(','), st.ended].join('|');
  if (key === s.panelKey) return;
  s.panelKey = key;
  const hadFocus = els.planPanel.contains(document.activeElement);
  els.planPanel.replaceChildren(
    planOverlay(st, {
      headingId: 'plan-sheet-heading',
      level: s.panelLevel,
      drawerOpen: s.panelDrawer,
      formOpen: s.panelFormOpen,
      draft: s.panelDraft,
      onDraft: (text) => (s.panelDraft = text),
      onFormOpen: (open) => (s.panelFormOpen = open),
      onRequest: (text) => cmd('request_plan_change', { sessionId: s.id, text }),
      onClose: () => els.planPanel.close(),
      onLevel: (level) => (s.panelLevel = level),
      onDrawer: (open) => (s.panelDrawer = open),
    }),
  );
  if (hadFocus) focusQuiet($('#plan-sheet-heading'));
}

/** Opens the whole-screen plan, from the head's button or the card's corner. */
function openPlanSheet({ fromCard = false } = {}) {
  const s = state.current;
  if (!s || els.planPanel.open) return;
  if (fromCard) {
    // "Expand" shows the drawing the card is showing, not the claims view.
    s.panelFromCard = true;
    s.panelLevel = 'topics';
  } else if (els.planToggle.hidden) {
    return;
  }
  renderPanel();
  if (!els.planPanel.querySelector('.plan-overlay-inner')) return;
  els.planPanel.showModal();
  els.planPanel.querySelector('.plan-overlay-inner')?.show?.();
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
  if (s.readOnly && s.readOnlyReason === 'desktop-plugin') {
    const names = s.desktopOnlyPlugins;
    const verb = names.length === 1 ? 'runs' : 'run';
    return {
      note: `${joinNames(names)} only ${verb} on desktop. Continue this session on desktop.`,
      button: 'Continue here anyway',
      act: 'unlock',
    };
  }
  if (s.readOnly) return { note: READ_ONLY_NOTE, button: 'Start a new session', act: 'new', primary: true };
  if (!s.learn) return { composer: true };
  if (pendingItem(s)) return { note: 'Question waiting.', button: 'Go to question', act: 'question' };
  // Ended: the next step is retrieval practice on what was just taught (§7.9).
  if (s.st?.ended) {
    return planIdeas(s).length
      ? { note: 'Session complete.', button: 'Practise these ideas', act: 'practise', primary: true }
      : { note: 'Session complete.', button: 'Start a new session', act: 'new', primary: true };
  }
  const phase = s.st?.phase;
  if (phase === 'awaitApproval') return { note: 'Plan ready.', button: 'Go to plan', act: 'plan' };
  if (phase === 'plan') {
    if (s.planFailed && !running(s.id, 'planner')) return { note: 'Planning failed.' };
    return { note: 'Drafting your plan…' };
  }
  if (s.planFailed) return { composer: true, note: 'Planning failed. Describe your goal again.' };
  return { composer: true };
}

/** The ended session's plan nodes, in the Map selection's shape. */
function planIdeas(s) {
  const plan = s.st?.plan;
  const sections = plan?.sections ?? [];
  const topicOf = (id) => sections.find((sec) => (sec.nodeIds ?? []).includes(id))?.title ?? null;
  return (plan?.nodes ?? []).map((n) => ({
    title: n.title ?? n.id,
    topic: topicOf(n.id) ?? plan?.goal ?? 'this plan',
  }));
}

/** "A", "A and B", "A, B and C". */
function joinNames(names) {
  if (names.length < 2) return names[0] ?? 'A plugin';
  return `${names.slice(0, -1).join(', ')} and ${names.at(-1)}`;
}

/** "Continue here anyway": record the choice (synced) and re-open the session writable. */
async function unlockHere(s) {
  els.deskNoteButton.disabled = true;
  try {
    await cmd('mobile_unlock', { sessionId: s.id });
  } catch (e) {
    if (state.current === s) els.deskNoteText.textContent = `Couldn’t continue here: ${e}`;
    return;
  } finally {
    els.deskNoteButton.disabled = false;
  }
  if (state.current !== s) return;
  closeCurrent();
  openSession(s.id);
}

/** After a turn that used a desktop-only plugin: on a phone the session is now locked, so re-open it. */
async function recheckLock(s) {
  s.lockCheck = false;
  const data = await call('load_session', { id: s.id }).catch(() => null);
  if (!data?.readOnly || state.current !== s) return;
  closeCurrent();
  openSession(s.id);
}

function practiseEnded(s) {
  if (!modelReady()) {
    els.deskNoteText.textContent = 'Choose a model first.';
    openSettings({ to: 'model' });
    return;
  }
  const text = practiseText(planIdeas(s));
  closeCurrent();
  runPractice(text);
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
  renderComment(s);
}

// ---------- comment: interrupt the tutor at any moment ----------

/**
 * The comment box is offered when the learner can't simply reply: a question
 * or choice is open, a reply is streaming, or the plan waits for approval
 * (then the comment goes to the planner). Not while the planner drafts.
 */
function commentOffered(s) {
  if (!s?.learn || !s.id || s.readOnly || s.st?.ended) return false;
  const phase = s.st?.phase;
  if (phase === 'plan') return false;
  return Boolean(pendingItem(s)) || phase === 'awaitApproval' || running(s.id, 'tutor');
}

function renderComment(s) {
  const offered = commentOffered(s);
  if (els.deskComment.hidden === !offered) return;
  els.deskComment.hidden = !offered;
  if (!offered) closeComment();
}

function openComment() {
  els.commentForm.hidden = false;
  els.commentToggle.setAttribute('aria-expanded', 'true');
  els.commentToggle.textContent = 'Cancel comment';
  els.commentStatus.textContent = '';
  els.commentInput.focus({ preventScroll: true });
}

function closeComment({ focus = false } = {}) {
  els.commentForm.hidden = true;
  els.commentToggle.setAttribute('aria-expanded', 'false');
  els.commentToggle.textContent = 'Comment';
  if (focus) els.commentToggle.focus({ preventScroll: true });
}

/** Sends the comment; the tutor stops what it was doing and answers it. */
async function sendComment(event) {
  event.preventDefault();
  const s = state.current;
  const text = els.commentInput.value.trim();
  if (!text || !s?.id) return;
  if (!modelReady()) {
    els.commentStatus.textContent = 'Choose a model first.';
    return;
  }
  els.commentInput.value = '';
  closeComment();
  try {
    await call('comment', { sessionId: s.id, text });
  } catch (e) {
    if (state.current !== s) return;
    // Nothing was sent: give the text back with the reason.
    if (!els.deskComment.hidden) {
      openComment();
      els.commentInput.value = text;
      els.commentStatus.textContent = message(e);
    } else {
      setPromptError(message(e));
    }
  }
}

function onDeskNoteButton() {
  const s = state.current;
  const act = els.deskNoteButton.dataset.act;
  if (act === 'new') {
    state.mode = 'learn';
    showHome();
    return;
  }
  if (act === 'unlock') {
    unlockHere(s);
    return;
  }
  if (act === 'practise') {
    practiseEnded(s);
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

async function openSession(id, { reload = false } = {}) {
  if (state.current?.id === id && !reload) return;
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

async function startLearn(text, attachments = []) {
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
    const summary = await call('start_teaching', { text, attachments });
    if (state.current === s && !s.id && summary?.id) onSessionSummary(summary);
  } catch (e) {
    if (state.current === s && !s.id) {
      // Nothing was saved (e.g. no model chosen): put the text back.
      state.starting = false;
      showHome();
      els.prompt.value = text;
      composer?.restore(attachments);
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

async function sendLearner(text, attachments = []) {
  const s = state.current;
  const before = s.userCount;
  try {
    await call('send_learner_message', { sessionId: s.id, text, attachments });
  } catch (e) {
    if (state.current !== s) return;
    if (s.userCount === before) {
      // Rejected (question pending, plan phase…): give the text back.
      if (!els.prompt.value) {
        els.prompt.value = text;
        autoGrow();
        updateSendState();
      }
      composer?.restore(attachments);
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
async function runPractice(text, attachments = []) {
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
    s.messages.push({ role: 'user', content: text, attachments });
    const item = { kind: 'user', key: `m${s.messages.length - 1}`, msg: { role: 'user', content: text, attachments } };
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
      if (!s.id) {
        s.id = ev.session.id;
        composer?.setSession(s.id);
      }
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
      : await call('send_message', { sessionId: isNew ? null : s.id, mode: 'practice', text, attachments, onEvent });
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
      composer?.restore(attachments);
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
  if (ok && state.current === s) {
    announce('Reply finished.');
    maybeNudge(s);
  }
}

function stop() {
  const s = state.current;
  if (s?.id) call('cancel_reply', { sessionId: s.id }).catch((e) => setPromptError(message(e)));
}

async function submit(event) {
  event.preventDefault();
  if (isStreaming()) {
    stop();
    return;
  }
  const text = els.prompt.value.trim();
  if (!text) return;
  if (!modelReady()) {
    setPromptError('Choose a model first.');
    openSettings({ to: 'model' });
    return;
  }
  // With a model that can't see images this asks once; null = keep editing.
  const attachments = composer ? await composer.takeForSend() : [];
  if (attachments === null) return;
  els.prompt.value = '';
  autoGrow();
  setPromptError('');
  updateSendState();
  const s = state.current;
  if (!s) {
    if (state.mode === 'learn') startLearn(text, attachments);
    else runPractice(text, attachments);
  } else if (s.learn) sendLearner(text, attachments);
  else runPractice(text, attachments);
}

function goHome() {
  if (state.practiceBusy) {
    headStatus(GUARD);
    return;
  }
  showHome({ focusId: state.current?.id });
}

// ---------- Map ----------

/**
 * The Map's learning context (ui-design §7.7.6): level text per graph idea, the
 * confidence line and the frontier, all from the newest Learn session's state
 * plus `frontier`. Venues are not here: where the field lives belongs to
 * topics in the graph, so `knowledge_map.topicVenues` carries it. Null when
 * there is nothing to add, so a Map over a graph with no sessions looks
 * exactly as before.
 */
async function mapLearning() {
  const newest = sessions.newestLearn();
  const [st, frontier] = await Promise.all([
    newest ? call('session_state', { sessionId: newest.id }).catch(() => null) : null,
    call('frontier').catch(() => []),
  ]);
  const plan = st?.plan ?? null;
  const aimOf = new Map();
  for (const section of plan?.sections ?? []) {
    if (!section.targetLevel) continue;
    for (const id of section.nodeIds ?? []) aimOf.set(id, section.targetLevel);
  }
  const levels = {};
  for (const node of plan?.nodes ?? []) {
    const key = node.graphNode;
    if (!key) continue;
    const reached = st?.nodeLevels?.[node.id] ?? null;
    const aim = aimOf.get(node.id) ?? null;
    if (reached || aim) levels[key] = { reached, aim };
  }
  const items = [...(frontier ?? [])].sort((a, b) => String(b.at).localeCompare(String(a.at)));
  const learning = {
    levels,
    calibration: st?.calibration ?? null,
    frontier: items,
  };
  const empty = !Object.keys(levels).length && !learning.calibration && !items.length;
  return empty ? null : learning;
}

async function showMap() {
  if (!els.home.hidden) state.homeScroll = els.home.scrollTop;
  els.home.hidden = true;
  els.mapView.hidden = false;
  els.mapError.textContent = '';
  focusQuiet(els.mapHeading);
  const visit = ++state.mapLoad;
  map.loading();
  try {
    const [graph, review, learning] = await Promise.all([call('knowledge_map'), call('edge_proposals'), mapLearning()]);
    if (visit === state.mapLoad && !els.mapView.hidden) map.render(graph, review, learning);
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
    openSettings({ to: 'model' });
    return;
  }
  const text = practiseText(picked);
  map.clear();
  runPractice(text);
}

/**
 * First message of a mixed Practice session on `picked` ([{ title, topic }],
 * both from the knowledge map). The first line doubles as the session title
 * until the generated one arrives.
 */
function practiseText(picked) {
  const lines = picked.map(({ title, topic }) => `- ${title} (in “${topic}”)`);
  return [
    `Practise: ${picked.map((p) => p.title).join(', ')}`,
    '',
    'Mix them together rather than one at a time.',
    ...lines,
  ].join('\n');
}

// ---------- wiring ----------

function wire() {
  els.page = $('#page');
  const headFit = new ResizeObserver(fitHead);
  for (const el of [els.title.parentElement, els.title, els.phase]) headFit.observe(el);
  composer = initComposer({ onUseWorkspace: useMentionedWorkspace });
  wireSessionMenu();
  els.composer.addEventListener('submit', submit);
  els.prompt.addEventListener('input', () => {
    autoGrow();
    updateSendState();
    if (els.promptError.textContent) setPromptError('');
  });
  els.prompt.addEventListener('keydown', (e) => {
    if (e.defaultPrevented) return;
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
  els.commentToggle.addEventListener('click', () => (els.commentForm.hidden ? openComment() : closeComment({ focus: true })));
  els.commentForm.addEventListener('submit', sendComment);
  els.commentInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      els.commentForm.requestSubmit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeComment({ focus: true });
    }
  });
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
  els.planToggle.addEventListener('click', () => openPlanSheet());
  els.planPanel.addEventListener('close', () => {
    const s = state.current;
    const fromCard = Boolean(s?.panelFromCard);
    if (s) s.panelFromCard = false;
    if (fromCard) {
      // Back to the corner button the learner expanded from — unless the plan
      // was approved while it was open, in which case the card is gone and the
      // head's "Plan" button is the way back.
      const expand = els.planSlot.querySelector('.plan-expand');
      if (expand) {
        focusQuiet(expand);
        renderPanel();
        return;
      }
    }
    if (!els.planToggle.hidden) focusQuiet(els.planToggle);
    renderPanel();
  });
  els.planPanel.addEventListener('click', (e) => {
    if (e.target === els.planPanel) els.planPanel.close();
  });
  $('#open-settings').addEventListener('click', () => openSettings());
  $('#setup-button').addEventListener('click', () => openSettings({ to: 'providers' }));
  $('#sessions-retry').addEventListener('click', loadSessions);
  els.openMap.addEventListener('click', showMap);
  els.resumeSession.addEventListener('click', () => openSession(els.resumeSession.dataset.id));
  els.mapBack.addEventListener('click', () => showHome({ focus: els.openMap }));
  els.mapClear.addEventListener('click', () => {
    map.clear();
    focusQuiet(els.mapPractise.disabled ? els.mapHeading : els.mapPractise);
  });
  els.mapPractise.addEventListener('click', practiseSelection);
  initSettings({ changed: applyConfig, pluginsChanged: onPluginsChanged });
  initSync({ openSettings });
  initFocus({ onFullscreen: (on) => (els.fullscreenToggle.textContent = on ? 'Exit full screen' : 'Full screen') });
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
  const [config] = await Promise.all([
    loadConfig().catch((e) => ({ error: message(e) })),
    loadSessions(),
    loadPlugins(true),
    loadCatalog(),
  ]);
  if (config.error) setPromptError(`Couldn’t load your settings: ${config.error}`);
  else applyConfig(config);
  refreshVision();
  // Warm KaTeX and Prism core once the window is idle, not on the startup
  // path; Prism grammars still load only when a reply uses them.
  (window.requestIdleCallback ?? ((f) => setTimeout(f, 1500)))(() => {
    ensureKatex().catch(() => {});
    ensurePrism().catch(() => {});
  });
}

start();
