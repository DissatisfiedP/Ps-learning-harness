// Settings dialog: per-role model choices, provider connections, focus
// mode, and the theme override. Statuses are inline (role="status") next to the control
// that caused them and persist until the next action in that group.

import { call, message } from './api.js';
import { escapeHtml } from './markdown.js';
import { initPluginSettings } from './pluginui.js';
import { dndSupport, focusPref, setFocusPref } from './focus.js';

const dialog = document.getElementById('settings');
const modelForm = document.getElementById('model-form');
const providerList = document.getElementById('provider-list');
const themeChoice = document.getElementById('theme-choice');
const focusChoice = document.getElementById('focus-choice');
const startingPointChoice = document.getElementById('starting-point');
const startingPointStatus = document.getElementById('starting-point-status');

const THEME_KEY = 'lh.theme';

let providers = [];
let settings = {};
let roles = []; // RoleInfo[]
let usage = []; // RoleUsageEntry[]
let models = new Map(); // provider id -> ModelInfo[]
let login = null; // { state, authUrl } while WorkBuddy sign-in is pending
let claudeLogin = null; // { state, authUrl } while Claude sign-in awaits a pasted code
let hooks = { changed: () => {}, pluginsChanged: () => {} };
let pluginSettings = null;

// Inline statuses that survive re-renders: key -> { text, error }.
const roleStatus = new Map();
const providerStatus = new Map();

export function initSettings(h) {
  hooks = h;
  pluginSettings = initPluginSettings({ onChanged: () => hooks.pluginsChanged?.() });
  dialog.addEventListener('close', () => {
    if (login) cancelLogin();
    if (claudeLogin) cancelClaudeLogin();
  });
  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) dialog.close();
  });
  const saved = readTheme();
  for (const r of themeChoice.querySelectorAll('input')) {
    r.checked = r.value === saved;
    r.addEventListener('change', () => r.checked && setTheme(r.value));
  }
  for (const box of focusChoice.querySelectorAll('input[data-focus]')) {
    box.checked = focusPref(box.dataset.focus);
    box.addEventListener('change', () => setFocusPref(box.dataset.focus, box.checked));
  }
  startingPointChoice?.addEventListener('change', (e) => {
    if (e.target.name === 'starting-point' && e.target.checked) saveStartingPoint(e.target.value);
  });
  renderDndRow();
}

// ---------- learning (ui-design §7.7.7) ----------

/** The stored default, or Measure me when the settings say nothing. */
const startingPoint = () => settings.startingPoint ?? 'measure';

function renderStartingPoint() {
  const value = startingPoint();
  for (const input of startingPointChoice?.querySelectorAll('input') ?? []) input.checked = input.value === value;
}

async function saveStartingPoint(value) {
  startingPointStatus.classList.remove('error');
  startingPointStatus.textContent = 'Saving…';
  try {
    await call('set_starting_point', { value });
    settings.startingPoint = value;
    startingPointStatus.textContent = 'Saved.';
  } catch (e) {
    startingPointStatus.textContent = message(e);
    startingPointStatus.classList.add('error');
    // Put the stored choice back: the radio must never claim a saved default.
    renderStartingPoint();
  }
}

// ---------- focus (ui-design §7.8) ----------

/** The DND row shows only when a notification backend was detected; its label names it. */
async function renderDndRow() {
  const backend = await dndSupport();
  document.getElementById('focus-dnd-row').hidden = !backend;
  document.getElementById('focus-dnd-label').textContent = backend
    ? `Silence notifications in sessions (${backend})`
    : 'Silence notifications in sessions';
}

// ---------- appearance ----------

function readTheme() {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return v === 'light' || v === 'dark' ? v : 'system';
  } catch {
    return 'system';
  }
}

function setTheme(value) {
  const root = document.documentElement;
  if (value === 'light' || value === 'dark') root.dataset.theme = value;
  else delete root.dataset.theme;
  try {
    if (value === 'system') localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, value);
  } catch {
    // Not persisted; the choice still applies to this window.
  }
}

// ---------- dialog ----------

export async function openSettings() {
  try {
    await reload();
  } catch (e) {
    modelForm.innerHTML = `<p class="status error" role="status">Couldn’t load settings: ${escapeHtml(message(e))}</p>`;
  }
  if (!dialog.open) dialog.showModal();
  renderDndRow();
  try {
    await pluginSettings?.render();
  } catch (e) {
    const el = document.getElementById('plugin-status');
    if (el) {
      el.textContent = `Couldn’t load plugins: ${message(e)}`;
      el.classList.add('error');
    }
  }
}

/** Current providers + settings, for callers outside the dialog. */
export async function loadConfig() {
  [providers, settings] = await Promise.all([call('list_providers'), call('get_settings')]);
  return { providers, settings };
}

async function reload() {
  await loadConfig();
  try {
    [roles, usage] = await Promise.all([call('list_roles'), call('role_usage')]);
  } catch {
    roles = [];
    usage = [];
  }
  renderProviders();
  renderModelForm();
  renderStartingPoint();
  hooks.changed({ providers, settings });
}

const connected = () => providers.filter((p) => p.source);

function statusEl(map, key) {
  const p = document.createElement('p');
  p.className = 'status';
  p.setAttribute('role', 'status');
  const s = map.get(key);
  if (s) {
    p.textContent = s.text;
    p.classList.toggle('error', Boolean(s.error));
  }
  return p;
}

/** Sets a persisted status and mirrors it into the rendered element, if any. */
function setStatus(map, key, node, text, error = false) {
  if (text) map.set(key, { text, error });
  else map.delete(key);
  if (node?.isConnected) {
    node.textContent = text;
    node.classList.toggle('error', error);
  }
}

// ---------- model roles ----------

async function fetchModels(providerId, force = false) {
  if (!force && models.has(providerId)) return models.get(providerId);
  const list = await call('list_models', { provider: providerId, refresh: force });
  models.set(providerId, list);
  return list;
}

const roleChoice = (role) => settings.roles?.[role] ?? null;
const usageFor = (role) => usage.find((u) => u.role === role)?.usage ?? { inputTokens: 0, outputTokens: 0 };

function renderModelForm() {
  if (!roles.length) {
    modelForm.innerHTML = '<p class="help">Connect a provider below, then pick its models here.</p>';
    return;
  }
  modelForm.textContent = '';
  const tutor = roles.find((r) => r.role === 'tutor');
  if (tutor) modelForm.append(roleFieldset(tutor, true));
  const others = roles.filter((r) => r.role !== 'tutor');
  if (others.length) {
    const group = document.createElement('details');
    group.className = 'roles-optional';
    const summary = document.createElement('summary');
    summary.textContent = 'Other roles (optional)';
    group.append(summary);
    for (const r of others) group.append(roleFieldset(r, false));
    modelForm.append(group);
  }
}

function roleFieldset(info, required) {
  const role = info.role;
  const choice = roleChoice(role);
  const unset = !choice;
  const options = connected();
  const el = document.createElement('fieldset');
  el.className = 'role';
  el.dataset.role = role;
  const legendTail = required ? ' <span class="role-tag">required</span>' : '';
  const fallback = unset && !required ? ' <span class="role-tag">uses the Tutor model</span>' : '';
  el.innerHTML = `
    <legend>${escapeHtml(info.label)}${legendTail}${fallback}</legend>
    <p class="role-sub">${escapeHtml(info.weight)} · ${escapeHtml(info.phrase)}</p>
    <p class="role-help">${escapeHtml(info.guidance)}</p>
    <p class="meta" id="usage-${role}"></p>
    <div class="field">
      <label for="provider-${role}">Provider</label>
      <select class="input" id="provider-${role}">
        ${options.length
          ? options.map((p) => `<option value="${p.id}" ${p.id === choice?.provider ? 'selected' : ''}>${escapeHtml(p.name)}</option>`).join('')
          : '<option value="">No provider connected</option>'}
      </select>
    </div>
    <div class="field">
      <label for="model-${role}">Model</label>
      <input class="input" id="model-${role}" list="models-${role}" autocomplete="off" spellcheck="false" placeholder="Type a model id" />
      <datalist id="models-${role}"></datalist>
    </div>
    <div class="field" id="effort-field-${role}" hidden>
      <label for="effort-${role}">Reasoning effort</label>
      <select class="input" id="effort-${role}"></select>
    </div>
    <div class="warnings" id="warn-${role}"></div>
    <div class="actions">
      <button class="btn ${required ? 'primary' : 'secondary'}" type="button" data-act="save">Use this model</button>
      <button class="btn text" type="button" data-act="clear" ${unset ? 'disabled' : ''}>Clear</button>
      <button class="btn text" type="button" data-act="test">Test</button>
      <button class="btn text" type="button" data-act="reload">Reload models</button>
    </div>`;
  const status = statusEl(roleStatus, role);
  el.append(status);
  const say = (text, error = false) => setStatus(roleStatus, role, status, text, error);

  const providerSel = el.querySelector(`#provider-${role}`);
  const modelInput = el.querySelector(`#model-${role}`);
  const effortField = el.querySelector(`#effort-field-${role}`);
  const effortSel = el.querySelector(`#effort-${role}`);
  const warn = el.querySelector(`#warn-${role}`);
  modelInput.value = choice?.model ?? '';

  const describe = () => {
    const list = models.get(providerSel.value) ?? [];
    const modelInfo = list.find((m) => m.id === modelInput.value.trim());
    const efforts = modelInfo?.efforts ?? [];
    effortField.hidden = efforts.length === 0;
    const chosen = choice?.model === modelInfo?.id ? choice?.effort : null;
    effortSel.innerHTML = efforts
      .map((e) => `<option value="${e}" ${e === (chosen ?? modelInfo?.defaultEffort) ? 'selected' : ''}>${e}</option>`)
      .join('');
    warn.innerHTML = warningLines(info, modelInfo);
  };

  const fill = async (pid, preferred, force = false) => {
    let list = [];
    let failed = false;
    try {
      list = await fetchModels(pid, force);
    } catch (e) {
      failed = true;
      say(`Couldn’t load models: ${message(e)}`, true);
    }
    if (!failed && !list.length) say('This provider listed no models; type an id instead.');
    el.querySelector(`#models-${role}`).innerHTML = list
      .map((m) => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.name)}</option>`)
      .join('');
    modelInput.value = preferred ?? list[0]?.id ?? modelInput.value;
    describe();
    return list;
  };

  providerSel.addEventListener('change', () => {
    say('');
    fill(providerSel.value);
  });
  modelInput.addEventListener('input', describe);

  el.querySelector('[data-act="save"]').addEventListener('click', async () => {
    if (!options.length) return say('Connect a provider first.', true);
    const c = {
      provider: providerSel.value,
      model: modelInput.value.trim(),
      effort: effortField.hidden ? null : effortSel.value,
    };
    say('Saving…');
    try {
      await call('set_role_model', { role, choice: c });
      say(`Saved — ${info.label} uses ${c.model}`);
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  });

  el.querySelector('[data-act="clear"]').addEventListener('click', async () => {
    say('');
    try {
      await call('set_role_model', { role, choice: null });
      say(required ? 'Cleared.' : `Cleared — ${info.label} uses the Tutor model`);
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  });

  el.querySelector('[data-act="test"]').addEventListener('click', async () => {
    say('Testing…');
    try {
      const r = await call('test_role', { role });
      say(r.called ? 'Tool call OK' : 'The model did not call the tool.', !r.called);
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  });

  el.querySelector('[data-act="reload"]').addEventListener('click', async () => {
    say('Reloading models…');
    const list = await fill(providerSel.value, modelInput.value.trim() || null, true);
    if (list.length) say(`Model list refreshed (${list.length}).`);
  });

  renderUsage(el.querySelector(`#usage-${role}`), usageFor(role));
  if (options.length) fill(providerSel.value, choice?.model);
  return el;
}

function renderUsage(node, u) {
  const fmt = (n) => `${(n ?? 0).toLocaleString()} tok`;
  node.textContent = `Usage: in ${fmt(u.inputTokens)} · out ${fmt(u.outputTokens)}`;
}

function warningLines(info, modelInfo) {
  const lines = [];
  const caps = modelInfo?.capabilities;
  const unverified = (v) => v === undefined || v === null;
  if (!modelInfo) {
    lines.push('Not in catalog; capabilities unverified.');
  } else {
    if (unverified(caps?.toolCalls)) lines.push('Tool calling unverified.');
    else if (caps.toolCalls === false) lines.push('This model can’t call tools.');
    if (info.visionRelevant) {
      if (unverified(caps?.vision)) lines.push('Vision unverified.');
      else if (caps.vision === false) lines.push('This model has no vision.');
    }
  }
  const ctx = modelInfo?.contextWindow;
  if (info.minContext) {
    if (ctx === undefined || ctx === null) {
      lines.push(`${Math.round(info.minContext / 1000)}K+ context advised; unverified.`);
    } else if (ctx < info.minContext) {
      lines.push(`Context ${Math.round(ctx / 1000)}K; ${Math.round(info.minContext / 1000)}K+ advised.`);
    }
  }
  return lines.map((l) => `<p class="meta">Note: ${escapeHtml(l)}</p>`).join('');
}

/** After the first provider connects, choose its first model as the Tutor so
 * the app is usable at once. */
async function autoPick(providerId) {
  if (settings.roles?.tutor) return;
  const list = await fetchModels(providerId, true);
  if (!list.length) return;
  const m = list[0];
  await call('set_role_model', {
    role: 'tutor',
    choice: { provider: providerId, model: m.id, effort: m.defaultEffort ?? null },
  });
}

// ---------- providers ----------

function statusText(p) {
  if (!p.source) return 'Not connected';
  if (p.source === 'env') return `From ${p.envVar}`;
  if (p.account) return `Signed in as ${p.account}`;
  return p.credential === 'workbuddy-account' || p.credential === 'claude-subscription'
    ? 'Connected'
    : 'Key saved';
}

// Provider ids whose body is expanded. Subscription providers start open.
const expanded = new Set(['workbuddy', 'claude-code']);

function renderProviders() {
  const focusedId = document.activeElement?.closest?.('.provider')?.dataset.id;
  providerList.textContent = '';
  // Subscription providers first, then connected providers, then the rest.
  const rank = (p) => (p.id === 'workbuddy' || p.id === 'claude-code' ? 0 : p.source ? 1 : 2);
  const ordered = [...providers].sort((a, b) => rank(a) - rank(b));
  for (const p of ordered) providerList.append(providerRow(p));
  // Keep keyboard users on the row they were working in.
  if (focusedId) providerList.querySelector(`.provider[data-id="${CSS.escape(focusedId)}"] .provider-toggle`)?.focus({ preventScroll: true });
}

function providerRow(p) {
  const el = document.createElement('div');
  const open = expanded.has(p.id) || (login && p.id === 'workbuddy') || (claudeLogin && p.id === 'claude-code');
  el.className = 'provider';
  el.dataset.id = p.id;
  const bodyId = `provider-body-${p.id}`;
  el.innerHTML = `
    <div class="provider-head">
      <span class="provider-name">${escapeHtml(p.name)}</span>
      <span class="provider-state">${escapeHtml(statusText(p))}</span>
      <button class="btn secondary provider-toggle" type="button" aria-expanded="${open}" aria-controls="${bodyId}">${open ? 'Close' : p.source ? 'Manage' : 'Connect'}</button>
    </div>
    <div class="provider-body" id="${bodyId}" ${open ? '' : 'hidden'}></div>`;
  el.querySelector('.provider-toggle').addEventListener('click', () => {
    if (expanded.has(p.id)) expanded.delete(p.id);
    else expanded.add(p.id);
    renderProviders();
  });
  const status = statusEl(providerStatus, p.id);
  const say = (text, error = false) => setStatus(providerStatus, p.id, status, text, error);
  if (open) {
    const body = el.querySelector('.provider-body');
    if (p.id === 'workbuddy') workbuddyBody(p, body, say);
    else if (p.id === 'claude-code') claudeBody(p, body, say);
    else keyBody(p, body, say);
  }
  el.append(status);
  return el;
}

let keyFieldId = 0;

function keyRow(p, label, say) {
  const id = `key-${p.id}-${++keyFieldId}`;
  const row = document.createElement('div');
  row.className = 'field';
  row.innerHTML = `
    <label for="${id}">${escapeHtml(label)}</label>
    <div class="inline-field">
      <input class="input" id="${id}" type="password" autocomplete="off" spellcheck="false" />
      <button class="btn secondary" type="button">Save</button>
    </div>`;
  const input = row.querySelector('input');
  const save = async () => {
    const key = input.value.trim();
    if (!key) return input.focus({ preventScroll: true });
    say('Saving…');
    try {
      await call('save_api_key', { provider: p.id, key });
      await autoPick(p.id);
      say(`${p.name} connected.`);
      models.delete(p.id);
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  };
  row.querySelector('button').addEventListener('click', save);
  input.addEventListener('keydown', (e) => e.key === 'Enter' && save());
  return row;
}

function keyLink(p, say, text = 'Get a key') {
  if (!p.keyUrl) return null;
  const a = document.createElement('button');
  a.type = 'button';
  a.className = 'btn text';
  a.textContent = `${text} ↗`;
  a.addEventListener('click', () => call('open_external', { url: p.keyUrl }).catch((e) => say(message(e), true)));
  return a;
}

function removeButton(p, label, say) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'btn secondary';
  b.textContent = label;
  b.addEventListener('click', async () => {
    say('');
    try {
      await call('remove_credential', { provider: p.id });
      models.delete(p.id);
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  });
  return b;
}

function keyBody(p, body, say) {
  const label = p.credential === 'chatgpt-token' ? 'ChatGPT access token' : 'API key';
  if (p.source === 'saved') {
    body.append(removeButton(p, 'Remove key', say));
  } else {
    body.append(keyRow(p, p.source === 'env' ? `${label} (overrides ${p.envVar})` : label, say));
    if (p.credential === 'chatgpt-token') {
      const hint = document.createElement('p');
      hint.className = 'help';
      hint.textContent = 'From ~/.codex/auth.json.';
      body.append(hint);
    }
    const link = keyLink(p, say);
    if (link) body.append(link);
  }
  body.append(baseUrlDetails(p, say));
}

function baseUrlDetails(p, say) {
  const d = document.createElement('details');
  const id = `base-url-${p.id}`;
  d.innerHTML = `
    <summary>Advanced</summary>
    <div class="field">
      <label for="${id}">Base URL</label>
      <div class="inline-field">
        <input class="input" id="${id}" spellcheck="false" placeholder="${escapeHtml(p.defaultBaseUrl ?? '')}" value="${escapeHtml(p.baseUrl ?? '')}" />
        <button class="btn secondary" type="button">Save</button>
      </div>
      <p class="help">For proxies or a local OpenAI-compatible server. Leave empty for the default.</p>
    </div>`;
  if (p.baseUrl) d.open = true;
  const input = d.querySelector('input');
  d.querySelector('button').addEventListener('click', async () => {
    say('Saving…');
    try {
      await call('set_base_url', { provider: p.id, url: input.value.trim() || null });
      // A different server serves a different list.
      models.delete(p.id);
      say('Base URL saved.');
      await reload();
    } catch (e) {
      say(message(e), true);
    }
  });
  return d;
}

function workbuddyBody(p, body, say) {
  if (p.source === 'saved') {
    body.append(removeButton(p, p.account ? 'Sign out' : 'Disconnect', say));
    return;
  }
  if (login) {
    body.innerHTML = `
      <p>Waiting for browser sign-in…</p>
      <div class="actions">
        <button class="btn secondary" type="button" data-act="reopen">Reopen sign-in ↗</button>
        <button class="btn text" type="button" data-act="cancel">Cancel</button>
      </div>`;
    body.querySelector('[data-act="reopen"]').addEventListener('click', () => call('open_external', { url: login.authUrl }).catch((e) => say(message(e), true)));
    body.querySelector('[data-act="cancel"]').addEventListener('click', cancelLogin);
    return;
  }
  const signIn = document.createElement('button');
  signIn.type = 'button';
  signIn.className = 'btn secondary';
  signIn.textContent = 'Sign in with WorkBuddy';
  signIn.addEventListener('click', () => startLogin(signIn, say));
  const hint = document.createElement('p');
  hint.className = 'help';
  hint.textContent = 'Uses WorkBuddy credits.';
  const alt = document.createElement('details');
  alt.innerHTML = '<summary>Use an API key instead</summary>';
  alt.append(keyRow(p, 'WorkBuddy API key', say));
  const link = keyLink(p, say, 'Create a key on workbuddy.ai');
  if (link) alt.append(link);
  body.append(signIn, hint, alt);
}

async function startLogin(button, say) {
  button.disabled = true;
  say('');
  try {
    login = await call('workbuddy_login_start');
    renderProviders();
    const account = await call('workbuddy_login_wait', { state: login.state });
    login = null;
    await autoPick('workbuddy');
    say(account ? `Signed in as ${account}.` : 'Signed in to WorkBuddy.');
    models.delete('workbuddy');
    await reload();
  } catch (e) {
    const wasCancelled = !login;
    login = null;
    if (!wasCancelled) say(message(e), true);
    renderProviders();
  }
}

function cancelLogin() {
  login = null;
  call('workbuddy_login_cancel').catch(() => {});
  renderProviders();
}

// ---------- Claude subscription sign-in (Claude Code OAuth) ----------

function claudeBody(p, body, say) {
  if (p.source === 'saved') {
    body.append(removeButton(p, p.account ? 'Sign out' : 'Disconnect', say));
    return;
  }
  if (claudeLogin) {
    body.innerHTML = `
      <p class="help">Paste the code from the browser.</p>
      <div class="field">
        <label for="claude-code-input">Authorization code</label>
        <div class="inline-field">
          <input class="input" id="claude-code-input" type="text" autocomplete="off" spellcheck="false" placeholder="code#state" />
          <button class="btn secondary" type="button" data-act="submit">Submit</button>
        </div>
      </div>
      <div class="actions">
        <button class="btn text" type="button" data-act="reopen">Reopen sign-in ↗</button>
        <button class="btn text" type="button" data-act="cancel">Cancel</button>
      </div>`;
    const input = body.querySelector('#claude-code-input');
    const submit = () => submitClaudeCode(input.value, say);
    body.querySelector('[data-act="submit"]').addEventListener('click', submit);
    input.addEventListener('keydown', (e) => e.key === 'Enter' && submit());
    body.querySelector('[data-act="reopen"]').addEventListener('click', () =>
      call('open_external', { url: claudeLogin.authUrl }).catch((e) => say(message(e), true)));
    body.querySelector('[data-act="cancel"]').addEventListener('click', cancelClaudeLogin);
    input.focus({ preventScroll: true });
    return;
  }
  const signIn = document.createElement('button');
  signIn.type = 'button';
  signIn.className = 'btn secondary';
  signIn.textContent = 'Sign in with Claude';
  signIn.addEventListener('click', () => startClaudeLogin(signIn, say));
  const hint = document.createElement('p');
  hint.className = 'help';
  hint.textContent = 'Uses your Claude Pro/Max plan.';
  body.append(signIn, hint);
}

async function startClaudeLogin(button, say) {
  button.disabled = true;
  say('');
  try {
    claudeLogin = await call('claude_login_start');
    renderProviders();
  } catch (e) {
    button.disabled = false;
    say(message(e), true);
  }
}

async function submitClaudeCode(code, say) {
  if (!code.trim()) return;
  say('Signing in…');
  try {
    const account = await call('claude_login_submit', { code });
    claudeLogin = null;
    await autoPick('claude-code');
    say(account ? `Signed in as ${account}.` : 'Signed in to Claude.');
    models.delete('claude-code');
    await reload();
  } catch (e) {
    say(message(e), true);
  }
}

function cancelClaudeLogin() {
  claudeLogin = null;
  call('claude_login_cancel').catch(() => {});
  renderProviders();
}
