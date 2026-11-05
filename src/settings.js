// Settings dialog: per-role model choices and provider connections.

import { call, message } from './api.js';
import { escapeHtml } from './markdown.js';

const dialog = document.getElementById('settings');
const modelForm = document.getElementById('model-form');
const providerList = document.getElementById('provider-list');

let providers = [];
let settings = {};
let roles = []; // RoleInfo[]
let usage = []; // RoleUsageEntry[]
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
  try {
    [roles, usage] = await Promise.all([call('list_roles'), call('role_usage')]);
  } catch {
    roles = [];
    usage = [];
  }
  renderProviders();
  renderModelForm();
  hooks.changed({ providers, settings });
}

const connected = () => providers.filter((p) => p.source);

// ---------- model roles ----------

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

const roleChoice = (role) => settings.roles?.[role] ?? null;
const usageFor = (role) => usage.find((u) => u.role === role)?.usage ?? { inputTokens: 0, outputTokens: 0 };

function renderModelForm() {
  if (!roles.length) {
    modelForm.innerHTML = '<p class="muted full">Connect a provider below, then pick its models here.</p>';
    return;
  }
  modelForm.innerHTML = '';
  const tutor = roles.find((r) => r.role === 'tutor');
  if (tutor) modelForm.append(roleCard(tutor, true));
  const others = roles.filter((r) => r.role !== 'tutor');
  if (others.length) {
    const group = document.createElement('details');
    group.className = 'roles-optional full';
    group.open = false;
    const summary = document.createElement('summary');
    summary.textContent = 'Other roles (optional)';
    group.append(summary);
    for (const r of others) group.append(roleCard(r, false));
    modelForm.append(group);
  }
}

function roleCard(info, required) {
  const role = info.role;
  const choice = roleChoice(role);
  const unset = !choice;
  const options = connected();
  const el = document.createElement('div');
  el.className = `role-card full${required ? ' required' : ''}`;
  el.dataset.role = role;

  const fallbackChip = unset && !required
    ? '<span class="chip fallback">using Tutor model</span>'
    : '';
  const requiredChip = required
    ? '<span class="chip req">required</span>'
    : '';

  el.innerHTML = `
    <div class="role-head">
      <span class="role-title"><strong>${escapeHtml(info.label)}</strong> · ${escapeHtml(info.weight)} · <em>${escapeHtml(info.phrase)}</em></span>
      ${requiredChip}${fallbackChip}
    </div>
    <p class="role-guidance">${escapeHtml(info.guidance)}</p>
    <div class="role-usage" id="usage-${role}"></div>
    <div class="field">
      <label for="provider-${role}">Provider</label>
      <select class="input" id="provider-${role}" aria-label="${escapeHtml(info.label)} provider">
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
      <select class="input" id="effort-${role}" aria-label="${escapeHtml(info.label)} effort"></select>
    </div>
    <div class="role-warnings" id="warn-${role}"></div>
    <div class="row">
      <button class="btn primary" type="button" data-act="save">Use this model</button>
      <button class="btn" type="button" data-act="clear" ${unset ? 'disabled' : ''}>Clear</button>
      <button class="btn" type="button" data-act="test">Test</button>
      <span class="role-test" id="test-${role}"></span>
    </div>`;

  const providerSel = el.querySelector(`#provider-${role}`);
  const modelInput = el.querySelector(`#model-${role}`);
  const effortField = el.querySelector(`#effort-field-${role}`);
  const effortSel = el.querySelector(`#effort-${role}`);
  const warn = el.querySelector(`#warn-${role}`);
  const testOut = el.querySelector(`#test-${role}`);
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

  const fill = async (pid, preferred) => {
    const list = await fetchModels(pid);
    el.querySelector(`#models-${role}`).innerHTML = list
      .map((m) => `<option value="${escapeHtml(m.id)}">${escapeHtml(m.name)}</option>`)
      .join('');
    modelInput.value = preferred ?? list[0]?.id ?? modelInput.value;
    describe();
  };

  providerSel.addEventListener('change', () => fill(providerSel.value));
  modelInput.addEventListener('input', describe);

  el.querySelector('[data-act="save"]').addEventListener('click', async () => {
    if (!options.length) return hooks.toast('Connect a provider first.', true);
    const c = {
      provider: providerSel.value,
      model: modelInput.value.trim(),
      effort: effortField.hidden ? null : effortSel.value,
    };
    try {
      await call('set_role_model', { role, choice: c });
      hooks.toast(`${info.label}: using ${c.model}`);
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  });

  el.querySelector('[data-act="clear"]').addEventListener('click', async () => {
    try {
      await call('set_role_model', { role, choice: null });
      await reload();
    } catch (e) {
      hooks.toast(message(e), true);
    }
  });

  el.querySelector('[data-act="test"]').addEventListener('click', async () => {
    testOut.textContent = 'Testing…';
    try {
      const r = await call('test_role', { role });
      testOut.textContent = r.called ? 'Tool call OK' : 'Did not call the tool';
      testOut.classList.toggle('error', !r.called);
      await reload();
    } catch (e) {
      testOut.textContent = message(e);
      testOut.classList.add('error');
    }
  });

  renderUsage(el.querySelector(`#usage-${role}`), usageFor(role));
  if (options.length) fill(providerSel.value, choice?.model);
  return el;
}

function renderUsage(node, u) {
  const fmt = (n) => (n ? `${n.toLocaleString()} tok` : '0 tok');
  node.textContent = `Usage · in ${fmt(u.inputTokens)} · out ${fmt(u.outputTokens)}`;
}

function warningLines(info, modelInfo) {
  const lines = [];
  const caps = modelInfo?.capabilities;
  const unverified = (v) => (v === undefined || v === null);
  if (!modelInfo) {
    lines.push('Capabilities unverified (model not in catalog).');
  } else {
    if (unverified(caps?.toolCalls)) lines.push('Tool calling: unverified.');
    else if (caps.toolCalls === false) lines.push('Warning: no tool calling.');
    if (info.visionRelevant) {
      if (unverified(caps?.vision)) lines.push('Vision: unverified.');
      else if (caps.vision === false) lines.push('Warning: no vision.');
    }
  }
  const ctx = modelInfo?.contextWindow;
  if (info.minContext) {
    if (ctx === undefined || ctx === null) {
      lines.push(`Context window recommended ≥${Math.round(info.minContext / 1000)}K: unverified.`);
    } else if (ctx < info.minContext) {
      lines.push(`Warning: context ${Math.round(ctx / 1000)}K is below the recommended ${Math.round(info.minContext / 1000)}K.`);
    }
  }
  return lines.map((l) => `<span class="warn-line">${escapeHtml(l)}</span>`).join('');
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
