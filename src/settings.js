// Settings dialog: model choice and provider connections.

import { call, message } from './api.js';
import { escapeHtml } from './markdown.js';

const dialog = document.getElementById('settings');
const modelForm = document.getElementById('model-form');
const providerList = document.getElementById('provider-list');

let providers = [];
let settings = {};
let models = new Map(); // provider id -> ModelInfo[]
let login = null; // { state, authUrl } while WorkBuddy sign-in is pending
let hooks = { changed: () => {}, toast: () => {} };

export function initSettings(h) {
  hooks = h;
  dialog.addEventListener('close', () => {
    if (login) cancelLogin();
  });
  dialog.addEventListener('click', (e) => {
    if (e.target === dialog) dialog.close();
  });
}

export async function openSettings() {
  await reload();
  if (!dialog.open) dialog.showModal();
}

/** Current providers + settings, for callers outside the dialog. */
export async function loadConfig() {
  [providers, settings] = await Promise.all([call('list_providers'), call('get_settings')]);
  return { providers, settings };
}

async function reload() {
  await loadConfig();
  renderProviders();
  await renderModelForm();
  hooks.changed({ providers, settings });
}

const connected = () => providers.filter((p) => p.source);

// ---------- model ----------

async function fetchModels(providerId, force = false) {
  if (!force && models.has(providerId)) return models.get(providerId);
  try {
    const list = await call('list_models', { provider: providerId });
    models.set(providerId, list);
    return list;
  } catch (e) {
    hooks.toast(`Could not load models: ${message(e)}`, true);
    return [];
  }
}

async function renderModelForm() {
  const options = connected();
  if (!options.length) {
    modelForm.innerHTML = `<p class="muted full">Connect a provider below, then pick its model here.</p>`;
    return;
  }
  const current = settings.model;
  const providerId = options.some((p) => p.id === current?.provider) ? current.provider : options[0].id;
  modelForm.innerHTML = `
    <div class="field">
      <label for="m-provider">Provider</label>
      <select class="input" id="m-provider">
        ${options.map((p) => `<option value="${p.id}" ${p.id === providerId ? 'selected' : ''}>${escapeHtml(p.name)}</option>`).join('')}
      </select>
    </div>
    <div class="field">
      <label for="m-model">Model</label>
      <input class="input" id="m-model" list="m-models" autocomplete="off" spellcheck="false" placeholder="Loading…" />
      <datalist id="m-models"></datalist>
    </div>
    <div class="field" id="m-effort-field" hidden>
      <label for="m-effort">Reasoning effort</label>
      <select class="input" id="m-effort"></select>
    </div>
    <div class="model-detail full" id="m-detail"></div>
    <div class="row full"><button class="btn primary" type="button" id="m-save">Use this model</button></div>`;

  const providerSel = modelForm.querySelector('#m-provider');
  const modelInput = modelForm.querySelector('#m-model');
  const effortField = modelForm.querySelector('#m-effort-field');
  const effortSel = modelForm.querySelector('#m-effort');
  const detail = modelForm.querySelector('#m-detail');

  const fill = async (pid, preferred) => {
    const list = await fetchModels(pid);
    modelForm.querySelector('#m-models').innerHTML = list
      .map((m) => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.name)}</option>`)
      .join('');
    modelInput.placeholder = list.length ? 'Pick or type a model id' : 'Type a model id';
    modelInput.value = preferred ?? list[0]?.id ?? '';
    describe();
  };
  const describe = () => {
    const info = (models.get(providerSel.value) ?? []).find((m) => m.id === modelInput.value.trim());
    const efforts = info?.efforts ?? [];
    effortField.hidden = efforts.length === 0;
    const chosen = settings.model?.model === info?.id ? settings.model?.effort : null;
    effortSel.innerHTML = efforts
      .map((e) => `<option value="${e}" ${e === (chosen ?? info.defaultEffort) ? 'selected' : ''}>${e}</option>`)
      .join('');
    const bits = [];
    if (info?.contextWindow) bits.push(`${Math.round(info.contextWindow / 1000)}K context`);
    if (info?.costLabel) bits.push(info.costLabel);
    if (info?.description) bits.push(info.description);
    detail.textContent = bits.join(' · ');
  };

  providerSel.addEventListener('change', () => fill(providerSel.value));
  modelInput.addEventListener('input', describe);
  modelForm.querySelector('#m-save').addEventListener('click', async () => {
    const choice = {
      provider: providerSel.value,
      model: modelInput.value.trim(),
      effort: effortField.hidden ? null : effortSel.value,
    };
    try {
      await call('set_model', { choice });
      hooks.toast(`Using ${choice.model}`);
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  });
  await fill(providerId, current?.provider === providerId ? current.model : undefined);
}

/** After the first provider connects, choose its first model so the app is usable at once. */
async function autoPick(providerId) {
  if (settings.model) return;
  const list = await fetchModels(providerId, true);
  if (!list.length) return;
  const m = list[0];
  await call('set_model', { choice: { provider: providerId, model: m.id, effort: m.defaultEffort ?? null } });
}

// ---------- providers ----------

function statusText(p) {
  if (!p.source) return 'Not connected';
  if (p.source === 'env') return `From ${p.envVar}`;
  if (p.account) return `Signed in as ${p.account}`;
  return p.credential === 'workbuddy-account' ? 'Connected' : 'Key saved';
}

// Provider ids whose card body is expanded. WorkBuddy starts open.
const expanded = new Set(['workbuddy']);

function renderProviders() {
  providerList.innerHTML = '';
  // WorkBuddy first, then connected providers, then the rest.
  const rank = (p) => (p.id === 'workbuddy' ? 0 : p.source ? 1 : 2);
  const ordered = [...providers].sort((a, b) => rank(a) - rank(b));
  for (const p of ordered) providerList.append(providerCard(p));
}

function providerCard(p) {
  const el = document.createElement('div');
  const open = expanded.has(p.id) || (login && p.id === 'workbuddy');
  el.className = `provider${p.source ? ' connected' : ''}${open ? ' open' : ''}`;
  el.innerHTML = `
    <div class="provider-head">
      <span class="provider-name">${escapeHtml(p.name)}</span>
      <span class="status ${p.source ? 'on' : ''}">${escapeHtml(statusText(p))}</span>
      <button class="btn small" type="button" aria-expanded="${open}">${open ? 'Close' : p.source ? 'Manage' : 'Connect'}</button>
    </div>
    <div class="provider-body"></div>`;
  el.querySelector('.provider-head .btn').addEventListener('click', () => {
    if (expanded.has(p.id)) expanded.delete(p.id);
    else expanded.add(p.id);
    renderProviders();
  });
  if (open) {
    const body = el.querySelector('.provider-body');
    if (p.id === 'workbuddy') workbuddyBody(p, body);
    else keyBody(p, body);
  }
  return el;
}

function keyRow(p, label) {
  const row = document.createElement('div');
  row.className = 'row';
  row.innerHTML = `
    <input class="input" type="password" autocomplete="off" spellcheck="false" placeholder="${escapeHtml(label)}" aria-label="${escapeHtml(label)}" />
    <button class="btn" type="button">Save</button>`;
  const input = row.querySelector('input');
  const save = async () => {
    const key = input.value.trim();
    if (!key) return input.focus();
    try {
      await call('save_api_key', { provider: p.id, key });
      await autoPick(p.id);
      hooks.toast(`${p.name} connected`);
      models.delete(p.id);
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  };
  row.querySelector('button').addEventListener('click', save);
  input.addEventListener('keydown', (e) => e.key === 'Enter' && save());
  return row;
}

function keyLink(p, text = 'Get a key') {
  if (!p.keyUrl) return null;
  const a = document.createElement('button');
  a.type = 'button';
  a.className = 'btn link';
  a.textContent = `${text} ↗`;
  a.addEventListener('click', () => call('open_external', { url: p.keyUrl }).catch((e) => hooks.toast(message(e), true)));
  return a;
}

function removeButton(p, label) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'btn danger';
  b.textContent = label;
  b.addEventListener('click', async () => {
    try {
      await call('remove_credential', { provider: p.id });
      models.delete(p.id);
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  });
  return b;
}

function keyBody(p, body) {
  const label = p.credential === 'chatgpt-token' ? 'ChatGPT access token' : 'API key';
  if (p.source === 'saved') {
    const row = document.createElement('div');
    row.className = 'row';
    row.append(removeButton(p, 'Remove key'));
    body.append(row);
  } else {
    body.append(keyRow(p, p.source === 'env' ? `${label} (overrides ${p.envVar})` : label));
    if (p.credential === 'chatgpt-token') {
      const hint = document.createElement('p');
      hint.className = 'hint';
      hint.textContent = 'The access token the Codex CLI keeps in ~/.codex/auth.json.';
      body.append(hint);
    }
    const link = keyLink(p);
    if (link) body.append(link);
  }
  body.append(baseUrlDetails(p));
}

function baseUrlDetails(p) {
  const d = document.createElement('details');
  d.innerHTML = `
    <summary>Advanced</summary>
    <div class="field">
      <label>Base URL</label>
      <div class="row">
        <input class="input" spellcheck="false" placeholder="${escapeHtml(p.defaultBaseUrl)}" value="${escapeHtml(p.baseUrl ?? '')}" />
        <button class="btn" type="button">Save</button>
      </div>
      <p class="hint">For proxies or a local OpenAI-compatible server. Leave empty for the default.</p>
    </div>`;
  if (p.baseUrl) d.open = true;
  const input = d.querySelector('input');
  d.querySelector('button').addEventListener('click', async () => {
    try {
      await call('set_base_url', { provider: p.id, url: input.value.trim() || null });
      hooks.toast('Base URL saved');
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  });
  return d;
}

function workbuddyBody(p, body) {
  if (p.source === 'saved') {
    const row = document.createElement('div');
    row.className = 'row';
    row.append(removeButton(p, p.account ? 'Sign out' : 'Disconnect'));
    body.append(row);
    return;
  }
  if (login) {
    body.innerHTML = `
      <div class="waiting"><span class="spinner"></span><span>Approve the sign-in in your browser…</span></div>
      <div class="row">
        <button class="btn" type="button" data-act="reopen">Open sign-in page again ↗</button>
        <button class="btn link" type="button" data-act="cancel">Cancel</button>
      </div>`;
    body.querySelector('[data-act="reopen"]').addEventListener('click', () => call('open_external', { url: login.authUrl }));
    body.querySelector('[data-act="cancel"]').addEventListener('click', cancelLogin);
    return;
  }
  const signIn = document.createElement('button');
  signIn.type = 'button';
  signIn.className = 'btn primary';
  signIn.textContent = 'Sign in with WorkBuddy';
  signIn.addEventListener('click', () => startLogin(signIn));
  const hint = document.createElement('p');
  hint.className = 'hint';
  hint.textContent = 'Opens workbuddy.ai in your browser; approve there and come back. Uses your WorkBuddy credits.';
  const alt = document.createElement('details');
  alt.innerHTML = '<summary>Use an API key instead</summary>';
  alt.append(keyRow(p, 'WorkBuddy API key'));
  const link = keyLink(p, 'Create a key on workbuddy.ai');
  if (link) alt.append(link);
  body.append(signIn, hint, alt);
}

async function startLogin(button) {
  button.disabled = true;
  try {
    login = await call('workbuddy_login_start');
    renderProviders();
    const account = await call('workbuddy_login_wait', { state: login.state });
    login = null;
    await autoPick('workbuddy');
    hooks.toast(account ? `Signed in as ${account}` : 'Signed in to WorkBuddy');
    models.delete('workbuddy');
    await reload();
  } catch (e) {
    const wasCancelled = !login;
    login = null;
    renderProviders();
    if (!wasCancelled) hooks.toast(message(e), true);
  }
}

function cancelLogin() {
  login = null;
  call('workbuddy_login_cancel').catch(() => {});
  renderProviders();
}
