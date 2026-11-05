// Learning Harness UI: sessions sidebar, home screen, conversation view.

import { available, call, channel, message } from './api.js';
import { render, escapeHtml } from './markdown.js';
import { initSettings, loadConfig, openSettings } from './settings.js';

const $ = (sel) => document.querySelector(sel);

const els = {
  app: $('#app'),
  sessionList: $('#session-list'),
  home: $('#home'),
  conversation: $('#conversation'),
  messages: $('#messages'),
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

const BRAND_SVG = '<svg viewBox="0 0 24 24"><path d="M12 3 2 8l10 5 8-4v6h2V8L12 3Z"/><path d="M6 11.2V16c0 1.7 2.7 3 6 3s6-1.3 6-3v-4.8l-6 3-6-3Z" opacity=".55"/></svg>';
const TRASH_SVG = '<svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/></svg>';

const state = {
  mode: 'learn',
  sessions: [],
  current: null, // { id, mode, title, messages: [] }
  busy: false,
  config: { providers: [], settings: {} },
};

// ---------- utilities ----------

let toastTimer;
function toast(text, isError = false) {
  els.toast.textContent = text;
  els.toast.classList.toggle('error', isError);
  els.toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (els.toast.hidden = true), isError ? 6000 : 2600);
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
  if (force || nearBottom()) els.messages.scrollTop = els.messages.scrollHeight;
}

function autoGrow() {
  els.prompt.style.height = 'auto';
  els.prompt.style.height = `${Math.min(els.prompt.scrollHeight, 240)}px`;
}

function setBusy(busy) {
  state.busy = busy;
  els.composer.classList.toggle('busy', busy);
  els.send.setAttribute('aria-label', busy ? 'Stop' : 'Send');
  updateSendState();
}

function updateSendState() {
  els.send.disabled = state.busy ? !state.current : !els.prompt.value.trim();
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

function renderSidebar() {
  const list = els.sessionList;
  list.innerHTML = '';
  if (!state.sessions.length) {
    list.innerHTML = '<p class="session-empty">Your sessions will appear here.</p>';
    return;
  }
  let group = null;
  let container = null;
  for (const s of state.sessions) {
    const label = groupLabel(s.updatedAt);
    if (label !== group) {
      group = label;
      container = document.createElement('div');
      container.className = 'session-group';
      container.innerHTML = `<h4>${label}</h4>`;
      list.append(container);
    }
    const item = document.createElement('div');
    item.className = `session-item${state.current?.id === s.id ? ' active' : ''}`;
    item.setAttribute('role', 'button');
    item.tabIndex = 0;
    item.title = s.title;
    item.innerHTML = `
      <span class="mode-dot ${s.mode}" title="${s.mode === 'learn' ? 'Learn' : 'Practice'}"></span>
      <span class="title">${escapeHtml(s.title)}</span>
      <button class="delete" type="button" aria-label="Delete session">${TRASH_SVG}</button>`;
    item.addEventListener('click', () => openSession(s.id));
    item.addEventListener('keydown', (e) => e.key === 'Enter' && openSession(s.id));
    const del = item.querySelector('.delete');
    del.addEventListener('click', (e) => {
      e.stopPropagation();
      // Two clicks: the first arms the button for a few seconds.
      if (!del.classList.contains('armed')) {
        del.classList.add('armed');
        del.setAttribute('aria-label', 'Click again to delete');
        del.title = 'Click again to delete';
        setTimeout(() => del.classList.remove('armed'), 3000);
        return;
      }
      deleteSession(s);
    });
    container.append(item);
  }
}

async function refreshSessions() {
  try {
    state.sessions = await call('list_sessions');
  } catch (e) {
    toast(message(e), true);
  }
  renderSidebar();
}

async function deleteSession(s) {
  try {
    await call('delete_session', { id: s.id });
    if (state.current?.id === s.id) showHome();
    await refreshSessions();
  } catch (e) {
    toast(message(e), true);
  }
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

function showHome() {
  state.current = null;
  els.home.hidden = false;
  els.conversation.hidden = true;
  els.homeSlot.append(els.composer);
  els.title.textContent = 'New session';
  els.modeChip.hidden = true;
  setMode(state.mode);
  renderSidebar();
  updateSendState();
  els.prompt.focus();
}

function showConversation() {
  els.home.hidden = true;
  els.conversation.hidden = false;
  els.dockSlot.append(els.composer);
  const s = state.current;
  els.title.textContent = s.title;
  els.modeChip.hidden = false;
  els.modeChip.textContent = s.mode === 'learn' ? 'Learn' : 'Practice';
  els.modeChip.classList.toggle('practice', s.mode === 'practice');
  els.prompt.placeholder = 'Reply…';
  renderSidebar();
}

async function openSession(id) {
  if (state.busy && state.current?.id !== id) {
    toast('Wait for the current reply to finish, or stop it.');
    return;
  }
  setNav(false);
  try {
    const s = await call('load_session', { id });
    state.current = s;
    showConversation();
    renderMessages();
    scrollToBottom(true);
  } catch (e) {
    toast(message(e), true);
  }
}

// ---------- messages ----------

const ENDINGS = {
  'max-tokens': 'Hit the length limit',
  refusal: 'Declined',
  interrupted: 'Stopped',
};

function userEl(text) {
  const el = document.createElement('div');
  el.className = 'msg user';
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  bubble.textContent = text;
  el.append(bubble);
  return el;
}

/** Assistant message element with an `update(msg, streaming)` method. */
function assistantEl() {
  const el = document.createElement('div');
  el.className = 'msg assistant';
  el.innerHTML = `
    <div class="meta"><span class="avatar">${BRAND_SVG}</span><span class="who">Tutor</span><span class="model"></span></div>
    <details class="thinking" hidden><summary>Reasoning</summary><div class="thinking-body"></div></details>
    <div class="prose"></div>`;
  const prose = el.querySelector('.prose');
  const thinking = el.querySelector('.thinking');
  const thinkingBody = el.querySelector('.thinking-body');
  const meta = el.querySelector('.meta');
  el.update = (msg, streaming) => {
    el.querySelector('.model').textContent = msg.model ? `· ${msg.model}` : '';
    thinking.hidden = !msg.thinking;
    thinkingBody.textContent = msg.thinking ?? '';
    if (msg.content) {
      prose.innerHTML = render(msg.content);
    } else if (streaming) {
      prose.innerHTML = '<span class="typing"><i></i><i></i><i></i></span>';
    } else {
      prose.innerHTML = '';
    }
    prose.classList.toggle('streaming', streaming && Boolean(msg.content));
    meta.querySelector('.badge')?.remove();
    if (msg.ending) {
      const badge = document.createElement('span');
      badge.className = `badge${msg.ending === 'interrupted' ? '' : ' warn'}`;
      badge.textContent = ENDINGS[msg.ending] ?? msg.ending;
      meta.append(badge);
    }
  };
  return el;
}

function messageEl(msg) {
  if (msg.role === 'user') return userEl(msg.content);
  const el = assistantEl();
  el.update(msg, false);
  return el;
}

function renderMessages() {
  els.messages.innerHTML = '';
  for (const m of state.current.messages) els.messages.append(messageEl(m));
  const last = state.current.messages.at(-1);
  if (last?.role === 'user' && !state.busy) {
    els.messages.append(errorEl('The tutor has not answered this yet.', 'Get a reply'));
  }
}

function errorEl(text, action = 'Retry') {
  const el = document.createElement('div');
  el.className = 'error-card';
  el.innerHTML = `<div class="text"><strong>Reply failed</strong>${escapeHtml(text)}</div>`;
  const b = document.createElement('button');
  b.className = 'btn';
  b.type = 'button';
  b.textContent = action;
  b.addEventListener('click', () => {
    el.remove();
    runTurn(null);
  });
  el.append(b);
  if (/model|connect|settings/i.test(text)) {
    const s = document.createElement('button');
    s.className = 'btn link';
    s.type = 'button';
    s.textContent = 'Settings';
    s.addEventListener('click', openSettings);
    el.append(s);
  }
  return el;
}

// ---------- sending ----------

/**
 * Runs one tutor turn. `text` is the learner's new message, or null to retry
 * the last unanswered one.
 */
async function runTurn(text) {
  const isNew = !state.current;
  if (isNew) {
    state.current = { id: null, mode: state.mode, title: text, messages: [] };
    showConversation();
    els.messages.innerHTML = '';
  }
  if (text !== null) {
    state.current.messages.push({ role: 'user', content: text });
    els.messages.append(userEl(text));
  }
  const reply = { role: 'assistant', content: '', thinking: '' };
  const view = assistantEl();
  view.update(reply, true);
  els.messages.append(view);
  scrollToBottom(true);
  setBusy(true);

  let frame = 0;
  const paint = () => {
    frame = 0;
    const stick = nearBottom();
    view.update(reply, true);
    if (stick) scrollToBottom(true);
  };
  const onEvent = channel((ev) => {
    if (ev.kind === 'session') {
      Object.assign(state.current, ev.session);
      els.title.textContent = ev.session.title;
      refreshSessions();
      updateSendState();
      return;
    }
    if (ev.kind === 'text') reply.content += ev.delta;
    else if (ev.kind === 'thinking') reply.thinking += ev.delta;
    if (!frame) frame = requestAnimationFrame(paint);
  });

  try {
    const stored = text === null
      ? await call('retry_reply', { sessionId: state.current.id, onEvent })
      : await call('send_message', { sessionId: isNew ? null : state.current.id, mode: state.current.mode, text, onEvent });
    cancelAnimationFrame(frame);
    state.current.messages.push(stored);
    view.update(stored, false);
  } catch (e) {
    cancelAnimationFrame(frame);
    if (isNew && !state.current.id) {
      // Nothing was saved (e.g. no model chosen): put the text back.
      showHome();
      els.prompt.value = text;
      autoGrow();
      toast(message(e), true);
    } else {
      view.remove();
      els.messages.append(errorEl(message(e)));
    }
  } finally {
    setBusy(false);
    scrollToBottom();
    refreshSessions();
  }
}

function submit(event) {
  event.preventDefault();
  if (state.busy) {
    if (state.current?.id) call('cancel_reply', { sessionId: state.current.id });
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
  runTurn(text);
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
      if (!state.busy) els.composer.requestSubmit();
    }
  });
  document.querySelectorAll('#mode-toggle button').forEach((b) => b.addEventListener('click', () => setMode(b.dataset.mode)));
  $('#new-session').addEventListener('click', () => {
    if (state.busy) return toast('Wait for the current reply to finish, or stop it.');
    setNav(false);
    showHome();
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
  try {
    applyConfig(await loadConfig());
  } catch (e) {
    toast(message(e), true);
  }
  await refreshSessions();
}

start();
