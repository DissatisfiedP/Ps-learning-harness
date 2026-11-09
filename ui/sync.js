// Sync (docs/sync-build.md, ui-design §7.5.2): the Settings → Sync section
// (git and WebDAV targets, Sync now, the needs-attention list), the Home
// header's sync icon with its small panel, and the Home banner. One
// `sync_status` read feeds all three; a failing read hides the Home parts quietly.

import { available, call, message } from './api.js';
import { expandSection, setSectionSummary } from './disclosure.js';

const $ = (id) => document.getElementById(id);

const home = $('home');
const wrap = $('sync-wrap');
const indicator = $('sync-indicator');
const dot = $('sync-dot');
const panel = $('sync-panel');
const panelTargets = $('sync-panel-targets');
const panelAttention = $('sync-panel-attention');
const panelNow = $('sync-panel-now');
const panelStatus = $('sync-panel-status');
const banner = $('sync-banner');
const bannerError = $('sync-banner-error');
const bannerErrorText = $('sync-banner-error-text');
const bannerAttention = $('sync-banner-attention');
const bannerAttentionText = $('sync-banner-attention-text');

const sectionToggle = $('sync-toggle');
const sectionStatus = $('sync-status');
const syncNowButton = $('sync-now');
const attention = $('sync-attention');
const attentionHeading = $('sync-attention-heading');
const attentionList = $('sync-paths');

const git = {
  form: $('sync-git'),
  state: $('sync-git-state'),
  error: $('sync-git-error'),
  status: $('sync-git-status'),
  disconnect: $('sync-git-disconnect'),
  remote: $('sync-git-remote'),
  token: $('sync-git-token'),
  branch: $('sync-git-branch'),
};
const webdav = {
  form: $('sync-webdav'),
  state: $('sync-webdav-state'),
  error: $('sync-webdav-error'),
  status: $('sync-webdav-status'),
  disconnect: $('sync-webdav-disconnect'),
  url: $('sync-webdav-url'),
  username: $('sync-webdav-username'),
  password: $('sync-webdav-password'),
};

const REFRESH_MS = 60_000;
const SAVED_SECRET = 'Saved · leave empty to keep';

let status = null; // SyncStatus, or null when unknown or unreadable
let running = false; // a sync_now this UI started is in flight
let readSeq = 0;
let timer = null;
let openSettings = async () => {};

// ---------- reading the status ----------

/** The configured targets, in run order, with the fields the Home parts read. */
function targets(s) {
  if (!s) return [];
  const list = [];
  if (s.configured) list.push({ name: 'Git', lastSyncAt: s.lastSyncAt, lastError: s.lastError, unsynced: s.unsynced });
  if (s.webdav?.configured) list.push({ name: 'WebDAV', ...s.webdav });
  return list;
}

/** Re-reads `sync_status` and repaints every sync surface. Never throws. */
async function refresh() {
  if (!available) return;
  const seq = ++readSeq;
  let next = null;
  let error = null;
  try {
    next = await call('sync_status');
  } catch (e) {
    error = message(e);
  }
  if (seq !== readSeq) return;
  status = next;
  render();
  if (error && !sectionStatus.textContent) say(sectionStatus, `Couldn’t read sync status: ${error}`, true);
}

// ---------- time ----------

const dayMonth = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' });
const fullTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

/** "just now", "5 min ago", "3 h ago", "yesterday", "on 3 Nov". */
function ago(iso) {
  const t = Date.parse(iso ?? '');
  if (Number.isNaN(t)) return null;
  const min = Math.floor((Date.now() - t) / 60_000);
  if (min < 1) return 'just now';
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} h ago`;
  if (h < 48) return 'yesterday';
  return `on ${dayMonth.format(t)}`;
}

const newest = (isos) => isos.filter((iso) => !Number.isNaN(Date.parse(iso ?? ''))).sort((a, b) => Date.parse(b) - Date.parse(a))[0] ?? null;

// ---------- rendering ----------

function say(node, text, error = false) {
  node.textContent = text;
  node.classList.toggle('error', error);
}

function render() {
  renderHome();
  renderSection();
}

function renderHome() {
  const list = targets(status);
  const on = list.length > 0;
  wrap.hidden = !on;
  if (!on) closePanel();
  let text = 'Off';
  if (on) {
    let title = '';
    const behind = list.some((t) => t.lastError || t.unsynced);
    if (running) text = 'Syncing…';
    else if (behind) text = 'Not synced';
    else {
      const last = newest(list.map((t) => t.lastSyncAt));
      text = last ? `Synced ${ago(last)}` : 'Not synced';
      if (last) title = `Last synced ${fullTime.format(Date.parse(last))}`;
    }
    indicator.title = title;
    indicator.setAttribute('aria-label', `Sync: ${text}`);
    if (running) indicator.setAttribute('aria-busy', 'true');
    else indicator.removeAttribute('aria-busy');
    dot.hidden = running || !behind;
    renderPanel(list);
  }
  // The collapsed Settings heading says the same; nothing when the status is unreadable.
  setSectionSummary('sync', status ? text : '');

  const firstError = list.find((t) => t.lastError)?.lastError ?? null;
  const parked = status?.needsAttention?.length ?? 0;
  bannerError.hidden = !firstError;
  bannerErrorText.textContent = firstError ? `Not synced: ${firstError}` : '';
  bannerAttention.hidden = parked === 0;
  bannerAttentionText.textContent = parked ? `${parked} ${parked === 1 ? 'file' : 'files'} changed on two devices need a look.` : '';
  banner.hidden = !firstError && parked === 0;
}

/** A target's line in the panel: "Last synced 5 min ago", "Not synced · last 3 h ago". */
function panelState(t) {
  const last = t.lastSyncAt ? ago(t.lastSyncAt) : null;
  if (t.lastError || t.unsynced) return last ? `Not synced · last ${last}` : 'Not synced';
  return last ? `Last synced ${last}` : 'Not synced';
}

/** The panel's contents; redrawn in place, so an open panel stays open across a refresh. */
function renderPanel(list) {
  panelTargets.textContent = '';
  for (const t of list) {
    const li = document.createElement('li');
    const line = document.createElement('p');
    line.textContent = `${t.name} · ${panelState(t)}`;
    li.append(line);
    if (t.lastError) {
      const error = document.createElement('p');
      error.className = 'sync-panel-error';
      error.textContent = t.lastError;
      li.append(error);
    }
    panelTargets.append(li);
  }
  const parked = status?.needsAttention?.length ?? 0;
  panelAttention.hidden = parked === 0;
  panelAttention.textContent = parked ? `${parked} ${parked === 1 ? 'file needs' : 'files need'} a look` : '';
  panelNow.disabled = running;
  panelNow.textContent = running ? 'Syncing…' : 'Sync now';
}

/** A target's state line: "Not connected", "Last synced 5 min ago · 2 ahead". */
function stateText(t) {
  if (!t.configured) return 'Not connected';
  const parts = [t.lastSyncAt && ago(t.lastSyncAt) ? `Last synced ${ago(t.lastSyncAt)}` : 'Not synced yet'];
  if (t.ahead) parts.push(`${t.ahead} ahead`);
  if (t.behind) parts.push(`${t.behind} behind`);
  if (t.unsynced && !t.ahead) parts.push('changes waiting');
  return parts.join(' · ');
}

function renderTarget(group, t) {
  group.state.textContent = stateText(t);
  group.error.textContent = t.configured && t.lastError ? t.lastError : '';
  group.error.hidden = !group.error.textContent;
  group.disconnect.hidden = !t.configured;
}

function renderSection() {
  const s = status;
  const gitTarget = { configured: Boolean(s?.configured), lastSyncAt: s?.lastSyncAt, lastError: s?.lastError, unsynced: s?.unsynced, ahead: s?.ahead, behind: s?.behind };
  renderTarget(git, gitTarget);
  renderTarget(webdav, { configured: Boolean(s?.webdav?.configured), ...(s?.webdav ?? {}) });
  syncNowButton.disabled = running || targets(s).length === 0;
  syncNowButton.textContent = running ? 'Syncing…' : 'Sync now';

  const paths = s?.needsAttention ?? [];
  attention.hidden = paths.length === 0;
  attentionList.textContent = '';
  for (const path of paths) {
    const li = document.createElement('li');
    const code = document.createElement('code');
    code.textContent = path;
    li.append(code);
    attentionList.append(li);
  }
}

/** Puts the saved settings back into the fields. Secrets are never shown. */
function fillForms() {
  const s = status;
  git.remote.value = s?.remote ?? '';
  git.branch.value = s?.branch || 'main';
  git.token.value = '';
  git.token.placeholder = s?.hasToken ? SAVED_SECRET : '';
  webdav.url.value = s?.webdav?.url ?? '';
  webdav.username.value = s?.webdav?.username ?? '';
  webdav.password.value = '';
  webdav.password.placeholder = s?.webdav?.hasPassword ? SAVED_SECRET : '';
}

// ---------- actions ----------

async function saveGit(e) {
  e.preventDefault();
  const remote = git.remote.value.trim();
  if (!remote) return git.remote.focus({ preventScroll: true });
  const token = git.token.value.trim();
  const branch = git.branch.value.trim();
  say(git.status, 'Saving…');
  try {
    status = await call('set_sync', { remote, token: token || null, branch: branch || null });
    say(git.status, 'Saved.');
    fillForms();
    render();
  } catch (err) {
    say(git.status, message(err), true);
  }
}

async function saveWebdav(e) {
  e.preventDefault();
  const url = webdav.url.value.trim();
  if (!url) return webdav.url.focus({ preventScroll: true });
  const password = webdav.password.value;
  say(webdav.status, 'Saving…');
  try {
    status = await call('set_webdav', { url, username: webdav.username.value.trim(), password: password || null });
    say(webdav.status, 'Saved.');
    fillForms();
    render();
  } catch (err) {
    say(webdav.status, message(err), true);
  }
}

/** Disconnects one target; focus moves to its first field, since the button hides. */
async function disconnect(group, command, args, first) {
  say(group.status, '');
  try {
    status = await call(command, args);
    say(group.status, 'Disconnected.');
    fillForms();
    render();
    first.focus({ preventScroll: true });
  } catch (err) {
    say(group.status, message(err), true);
  }
}

function reportText(report) {
  const parts = [];
  if (status?.configured) parts.push(`Git: ${report?.pulled ?? 0} pulled, ${report?.pushed ?? 0} pushed`);
  const w = report?.webdav;
  if (w) parts.push(`WebDAV: ${w.uploaded} uploaded, ${w.downloaded} downloaded`);
  return parts.length ? `Synced. ${parts.join('. ')}.` : 'Synced.';
}

/** Runs one `sync_now` (single-flight in this UI), then re-reads the status.
 * The Sync now buttons are disabled meanwhile; focus that took goes back to
 * the pressed button, or to the indicator when it is gone (the banner's "Try again"). */
async function syncNow(button) {
  if (running) return;
  const hadFocus = document.activeElement === button;
  running = true;
  say(sectionStatus, 'Syncing…');
  say(panelStatus, '');
  render();
  let report = null;
  let error = null;
  try {
    report = await call('sync_now');
  } catch (err) {
    error = message(err);
  }
  running = false;
  await refresh();
  render();
  say(sectionStatus, error ?? reportText(report), Boolean(error));
  say(panelStatus, error ?? reportText(report), Boolean(error));
  const active = document.activeElement;
  const lost = !active || active === document.body || active.getClientRects().length === 0;
  if (hadFocus && lost) (button.getClientRects().length ? button : indicator).focus({ preventScroll: true });
}

async function acknowledge(button) {
  try {
    status = await call('acknowledge_conflicts');
    say(sectionStatus, 'Acknowledged.');
    render();
    sectionToggle.focus({ preventScroll: true });
  } catch (err) {
    say(sectionStatus, message(err), true);
    button.focus({ preventScroll: true });
  }
}

/** Opens Settings at the Sync section (expanded), or at its needs-attention list. */
export async function openSyncSettings(at = 'section') {
  await openSettings();
  const toggle = expandSection('sync');
  const target = at === 'attention' && !attention.hidden ? attentionHeading : toggle;
  target.scrollIntoView({ block: 'start' });
  target.focus({ preventScroll: true });
}

/** Settings calls this on every open: fresh status, fields from the saved settings. */
export async function syncSettingsOpened() {
  for (const node of [sectionStatus, git.status, webdav.status]) say(node, '');
  await refresh();
  fillForms();
}

// ---------- Home: the sync icon's panel and the refresh timer ----------

function openPanel() {
  say(panelStatus, '');
  panel.hidden = false;
  indicator.setAttribute('aria-expanded', 'true');
  refresh();
}

function closePanel({ restoreFocus = false } = {}) {
  if (panel.hidden) return;
  panel.hidden = true;
  indicator.setAttribute('aria-expanded', 'false');
  if (restoreFocus) indicator.focus({ preventScroll: true });
}

const homeVisible = () => !home.hidden && document.visibilityState === 'visible';

/** On Home: refresh now and every minute; elsewhere: no timer. */
function onVisibility() {
  if (home.hidden) closePanel();
  if (homeVisible()) {
    if (!timer) {
      refresh();
      timer = setInterval(refresh, REFRESH_MS);
    }
  } else if (timer) {
    clearInterval(timer);
    timer = null;
  }
}

export function initSync(hooks) {
  openSettings = hooks.openSettings;
  git.form.addEventListener('submit', saveGit);
  webdav.form.addEventListener('submit', saveWebdav);
  git.disconnect.addEventListener('click', () => disconnect(git, 'set_sync', { remote: '' }, git.remote));
  webdav.disconnect.addEventListener('click', () => disconnect(webdav, 'set_webdav', { url: '', username: '', password: '' }, webdav.url));
  syncNowButton.addEventListener('click', () => syncNow(syncNowButton));
  panelNow.addEventListener('click', () => syncNow(panelNow));
  $('sync-banner-retry').addEventListener('click', (e) => syncNow(e.currentTarget));
  $('sync-banner-review').addEventListener('click', () => openSyncSettings('attention'));
  indicator.addEventListener('click', () => (panel.hidden ? openPanel() : closePanel()));
  $('sync-panel-settings').addEventListener('click', () => {
    closePanel();
    openSyncSettings();
  });
  wrap.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !panel.hidden) {
      e.preventDefault();
      closePanel({ restoreFocus: true });
    }
  });
  document.addEventListener('click', (e) => {
    if (!wrap.contains(e.target)) closePanel();
  });
  $('sync-acknowledge').addEventListener('click', (e) => acknowledge(e.currentTarget));
  if (!available) return;
  new MutationObserver(onVisibility).observe(home, { attributes: true, attributeFilter: ['hidden'] });
  document.addEventListener('visibilitychange', onVisibility);
  // Going to the background is mobile's "close": sync any local progress.
  // Best effort; the backend does nothing when nothing changed.
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') call('sync_if_changed').catch(() => {});
  });
  onVisibility();
}
