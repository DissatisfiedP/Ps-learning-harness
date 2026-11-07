// Settings → Plugins and Registries, and the consent screen (build prompt
// W8/W9). Every install path goes through `askConsent`; nothing installs as a
// side effect of approving a plan or opening Settings.

import { call, message } from './api.js';
import { loadPlugins, onPluginsChanged, plugins } from './pluginstore.js';

const STATUS_KEY = 'lh.registryStatus';
const PERMISSION_WORDS = {
  exec: 'Run programs on your device',
  net: 'Use the network',
  'fs-read': 'Read your files',
  'fs-write': 'Write your files',
};

let onChanged = () => {};
let wired = false;

export function permissionsText(permissions = []) {
  if (!permissions.length) return 'No files, network or programs.';
  const words = permissions.map((p) => PERMISSION_WORDS[p] ?? p);
  return `${words.join('; ')}.`;
}

function versionParts(v) {
  return String(v ?? '')
    .split('-')[0]
    .split('.')
    .map((x) => (x === '' ? 0 : Number(x)));
}

export function compareVersions(a, b) {
  const pa = versionParts(a);
  const pb = versionParts(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

export function latestVersion(versions = []) {
  return [...versions].sort((a, b) => compareVersions(b.version, a.version))[0] ?? null;
}

function element(id) {
  return document.getElementById(id);
}

function status(text, error = false) {
  const el = element('plugin-status');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('error', Boolean(error));
}

function registryStatus(text, error = false) {
  const el = element('registry-status');
  if (!el) return;
  el.textContent = text;
  el.classList.toggle('error', Boolean(error));
}

function readStatuses() {
  try {
    const raw = JSON.parse(localStorage.getItem(STATUS_KEY) ?? '{}');
    return raw && typeof raw === 'object' ? raw : {};
  } catch {
    return {};
  }
}

function saveStatuses(list) {
  const map = {};
  for (const s of list) map[s.url] = s;
  try {
    localStorage.setItem(STATUS_KEY, JSON.stringify(map));
  } catch {
    // Not persisted; the last check still shows for this window.
  }
}

/** Registry catalogs by plugin id: `{ registry, plugin }`. */
let catalogCache = null;
let catalogAt = 0;

export async function loadCatalog(force = false) {
  if (catalogCache && !force && Date.now() - catalogAt < 5000) return catalogCache;
  const map = new Map();
  try {
    const catalogs = await call('registry_catalog');
    for (const catalog of catalogs ?? []) {
      for (const plugin of catalog.plugins ?? []) {
        if (!map.has(plugin.id)) map.set(plugin.id, { registry: catalog.url, plugin, fetched: catalog.fetched });
      }
    }
  } catch {
    // No registries, no backend: an empty catalog is a valid state.
  }
  catalogCache = map;
  catalogAt = Date.now();
  return map;
}

export function pluginLabel(id) {
  return catalogCache?.get(id)?.plugin.title || id;
}

// ---------- consent ----------

/**
 * Shows the consent screen and runs `action` only if the learner agrees.
 * Resolves the action's value, or null on cancel.
 */
export function askConsent(details, action, { label = 'Install' } = {}) {
  const dialog = element('consent');
  if (!dialog) return Promise.resolve(null);
  element('consent-title').textContent = details.title || 'Install this plugin?';
  element('consent-desc').textContent = details.description || 'No description.';
  element('consent-version').textContent = details.version || 'unknown';
  element('consent-source').textContent = details.source || 'unknown';
  element('consent-perms').textContent = permissionsText(details.permissions ?? []);
  const install = element('consent-install');
  const cancel = element('consent-cancel');
  const statusEl = element('consent-status');
  statusEl.textContent = '';
  statusEl.classList.remove('error');
  install.textContent = label;
  install.disabled = false;
  cancel.disabled = false;

  if (!dialog.dataset.wired) {
    dialog.dataset.wired = 'yes';
    dialog.addEventListener('click', (e) => {
      if (e.target === dialog) dialog.close('cancel');
    });
  }

  return new Promise((resolve) => {
    const finish = (value) => {
      install.removeEventListener('click', accept);
      cancel.removeEventListener('click', decline);
      dialog.removeEventListener('close', onClose);
      if (dialog.open) dialog.close();
      resolve(value);
    };
    const decline = () => finish(null);
    const onClose = () => finish(null);
    const accept = async () => {
      install.disabled = true;
      cancel.disabled = true;
      statusEl.textContent = 'Installing…';
      try {
        const value = await action();
        statusEl.textContent = 'Installed.';
        finish(value);
      } catch (e) {
        install.disabled = false;
        cancel.disabled = false;
        statusEl.textContent = message(e);
        statusEl.classList.add('error');
        install.focus({ preventScroll: true });
      }
    };
    install.addEventListener('click', accept);
    cancel.addEventListener('click', decline);
    dialog.addEventListener('close', onClose);
    if (dialog.open) dialog.close();
    dialog.showModal();
    cancel.focus({ preventScroll: true });
  });
}

function candidateDetails(candidate, version) {
  return {
    title: candidate.plugin.title || candidate.plugin.id,
    description: candidate.plugin.description,
    version,
    source: candidate.registry,
    permissions: candidate.plugin.permissions ?? [],
  };
}

/** Consent + install of a registry plugin. `version` defaults to latest. */
export async function installCandidate(id, version = null) {
  let candidate = null;
  try {
    candidate = await call('install_candidate', { id });
  } catch (e) {
    throw new Error(message(e));
  }
  if (!candidate) {
    throw new Error('Not in any registry here. Add one in Settings → Registries.');
  }
  const chosen = version || latestVersion(candidate.versions)?.version || latestVersion(candidate.plugin?.versions)?.version;
  if (!chosen) throw new Error('No versions listed for this plugin.');
  const info = await askConsent(candidateDetails(candidate, chosen), async () => {
    const installed = await call('install_plugin', {
      registry: candidate.registry,
      id,
      version: chosen,
    });
    await loadPlugins(true);
    onChanged();
    return installed;
  });
  return info ?? null;
}

/** Consent + install of one `plugins.lock` entry that is missing here. */
export async function installLocked(entry) {
  if (entry?.source?.folder) {
    throw new Error('Folder plugins can’t download. Use “Add folder plugin…”.');
  }
  const registry = entry?.source?.registry;
  if (!registry) throw new Error('No registry to install from.');
  let candidate = null;
  try {
    candidate = await call('install_candidate', { id: entry.id });
  } catch {
    candidate = null;
  }
  const details = candidate
    ? candidateDetails(candidate, entry.version)
    : {
        title: entry.id,
        description: 'Refresh registries to see its description.',
        version: entry.version,
        source: registry,
        permissions: [],
      };
  const info = await askConsent(details, async () => {
    const installed = await call('install_plugin', {
      registry,
      id: entry.id,
      version: entry.version,
    });
    await loadPlugins(true);
    onChanged();
    return installed;
  });
  return info ?? null;
}

/** Pick a development folder, then consent and load it. */
export async function addFolderPlugin() {
  let path;
  try {
    path = await call('pick_folder');
  } catch (e) {
    if (/no folder chosen/i.test(message(e))) return null;
    status(`Couldn’t open the folder picker: ${message(e)}`, true);
    return null;
  }
  const info = await askConsent(
    {
      title: 'Add a folder plugin?',
      description: 'Reloads when its files change. Add only if you trust it.',
      version: 'declared by the folder',
      source: path,
      permissions: [],
    },
    async () => {
      const installed = await call('add_folder_plugin', { path });
      await loadPlugins(true);
      onChanged();
      return installed;
    },
    { label: 'Add plugin' },
  );
  return info ?? null;
}

// ---------- Settings → Plugins ----------

async function renderLock() {
  const box = element('plugin-lock');
  if (!box) return;
  box.textContent = '';
  let entries = [];
  try {
    entries = await call('missing_locked');
  } catch {
    entries = [];
  }
  if (!entries?.length) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  const p = document.createElement('p');
  p.className = 'lock-note';
  p.textContent = 'Missing on this device:';
  box.append(p);
  const ul = document.createElement('ul');
  ul.className = 'lock-items';
  for (const entry of entries) {
    const li = document.createElement('li');
    const name = document.createElement('span');
    name.className = 'lock-name';
    name.textContent = `${entry.id} ${entry.version}`;
    const source = document.createElement('span');
    source.className = 'meta';
    source.textContent = entry.source?.registry ? ` from ${entry.source.registry}` : entry.source?.folder ? ` from folder ${entry.source.folder}` : '';
    const install = document.createElement('button');
    install.type = 'button';
    install.className = 'btn text';
    install.textContent = entry.source?.folder ? 'Add folder…' : 'Install';
    install.setAttribute('aria-label', `Install ${entry.id} ${entry.version}`);
    install.addEventListener('click', async () => {
      install.disabled = true;
      try {
        if (entry.source?.folder) await addFolderPlugin();
        else await installLocked(entry);
        await render();
      } catch (e) {
        status(message(e), true);
        install.disabled = false;
      }
    });
    li.append(name, source, document.createTextNode(' '), install);
    ul.append(li);
  }
  box.append(ul);
}

function pluginRow(info, catalog) {
  const row = document.createElement('div');
  row.className = 'plugin';
  row.dataset.id = info.id;

  const head = document.createElement('div');
  head.className = 'plugin-head';
  const title = document.createElement('h3');
  title.className = 'plugin-title';
  title.textContent = info.title || info.id;
  const version = document.createElement('span');
  version.className = 'plugin-version';
  version.textContent = info.version;
  head.append(title, version);
  row.append(head);

  if (info.description) {
    const d = document.createElement('p');
    d.className = 'plugin-desc';
    d.textContent = info.description;
    row.append(d);
  }

  const meta = document.createElement('p');
  meta.className = 'meta';
  meta.textContent = `${info.id} · ${info.enabled === false ? 'Disabled' : 'Enabled'} · ${info.source}`;
  row.append(meta);

  const access = document.createElement('p');
  access.className = 'meta';
  access.textContent = `Access: ${permissionsText(info.permissions)}`;
  row.append(access);

  if (info.loadError) {
    const err = document.createElement('p');
    err.className = 'status error';
    err.setAttribute('role', 'status');
    err.textContent = `Couldn’t reload this folder plugin: ${info.loadError}`;
    row.append(err);
  }

  if (info.tools?.length) {
    const tools = document.createElement('p');
    tools.className = 'meta';
    tools.textContent = `Tools: ${info.tools.join(', ')}`;
    row.append(tools);
  }

  const actions = document.createElement('div');
  actions.className = 'actions';

  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'btn text';
  toggle.textContent = info.enabled === false ? 'Enable' : 'Disable';
  toggle.setAttribute('aria-label', `${info.enabled === false ? 'Enable' : 'Disable'} ${info.title || info.id}`);
  toggle.addEventListener('click', async () => {
    toggle.disabled = true;
    try {
      await call('set_plugin_enabled', { id: info.id, enabled: info.enabled === false });
      await loadPlugins(true);
      onChanged();
      status(`${info.title || info.id} ${info.enabled === false ? 'enabled' : 'disabled'}.`);
      await render();
    } catch (e) {
      status(message(e), true);
      toggle.disabled = false;
    }
  });
  actions.append(toggle);

  const entry = catalog.get(info.id);
  const newest = entry ? latestVersion(entry.plugin.versions) : null;
  if (newest && compareVersions(newest.version, info.version) > 0) {
    const update = document.createElement('button');
    update.type = 'button';
    update.className = 'btn text';
    update.textContent = `Update to ${newest.version}`;
    update.setAttribute('aria-label', `Update to ${newest.version}: ${info.title || info.id}`);
    update.addEventListener('click', async () => {
      update.disabled = true;
      try {
        await installCandidate(info.id, newest.version);
        status(`${info.title || info.id} updated to ${newest.version}.`);
        await render();
      } catch (e) {
        status(message(e), true);
        update.disabled = false;
      }
    });
    actions.append(update);
  }

  const remove = document.createElement('button');
  remove.type = 'button';
  remove.className = 'btn text';
  remove.textContent = 'Remove';
  remove.setAttribute('aria-label', `Remove ${info.title || info.id}`);
  remove.addEventListener('click', async () => {
    remove.disabled = true;
    try {
      await call('remove_plugin', { id: info.id });
      await loadPlugins(true);
      onChanged();
      status(`${info.title || info.id} removed.`);
      await render();
    } catch (e) {
      status(message(e), true);
      remove.disabled = false;
    }
  });
  actions.append(remove);

  row.append(actions);
  return row;
}

async function renderInstalled() {
  const list = element('plugin-list');
  if (!list) return;
  await loadPlugins(true);
  const catalog = await loadCatalog(true);
  const focusedId = document.activeElement?.closest?.('.plugin')?.dataset.id;
  list.textContent = '';
  if (!plugins().length) {
    const p = document.createElement('p');
    p.className = 'help';
    p.textContent = 'No plugins yet.';
    list.append(p);
  } else {
    for (const info of plugins()) list.append(pluginRow(info, catalog));
  }
  if (focusedId) {
    list.querySelector(`.plugin[data-id="${CSS.escape(focusedId)}"] button`)?.focus({ preventScroll: true });
  }
}

// ---------- Settings → Registries ----------

async function renderRegistries() {
  const list = element('registry-list');
  if (!list) return;
  const statuses = readStatuses();
  let registries = [];
  try {
    registries = await call('registries');
  } catch (e) {
    registryStatus(`Couldn’t read registries: ${message(e)}`, true);
    return;
  }
  await loadCatalog(true); // plan-chip labels follow the registries shown here
  const counts = new Map();
  try {
    const catalogs = await call('registry_catalog');
    for (const c of catalogs ?? []) counts.set(c.url, c);
  } catch {
    // Counts are optional.
  }
  list.textContent = '';
  if (!registries.length) {
    const p = document.createElement('p');
    p.className = 'help';
    p.textContent = 'No registries yet.';
    list.append(p);
    return;
  }
  for (const registry of registries) {
    const row = document.createElement('div');
    row.className = 'registry';
    const name = document.createElement('p');
    name.className = 'registry-name';
    name.textContent = registry.name || registry.url;
    const url = document.createElement('p');
    url.className = 'meta';
    url.textContent = registry.url;
    row.append(name, url);

    const cached = counts.get(registry.url);
    const count = cached?.plugins?.length ?? null;
    const st = statuses[registry.url];
    const line = document.createElement('p');
    line.className = 'meta';
    const plural = (n) => `${n} plugin${n === 1 ? '' : 's'}`;
    if (st?.fetchedAt) {
      const when = new Date(st.fetchedAt);
      line.textContent = `${st.ok ? plural(st.plugins) : 'Last check failed'} · checked ${Number.isNaN(when.getTime()) ? st.fetchedAt : when.toLocaleString()}`;
    } else if (count !== null) {
      line.textContent = `${plural(count)} in the cached index · not checked this session`;
    } else {
      line.textContent = 'Not fetched yet';
    }
    row.append(line);
    if (st?.error) {
      const err = document.createElement('p');
      err.className = 'status error';
      err.textContent = st.error;
      row.append(err);
    }

    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'btn text';
    remove.textContent = 'Remove registry';
    remove.setAttribute('aria-label', `Remove registry ${registry.name || registry.url}`);
    remove.addEventListener('click', async () => {
      remove.disabled = true;
      try {
        await call('remove_registry', { url: registry.url });
        const next = readStatuses();
        delete next[registry.url];
        try {
          localStorage.setItem(STATUS_KEY, JSON.stringify(next));
        } catch {
          // Fine: the row is gone anyway.
        }
        catalogCache = null;
        registryStatus(`Removed ${registry.name || registry.url}.`);
        await renderRegistries();
      } catch (e) {
        registryStatus(message(e), true);
        remove.disabled = false;
      }
    });
    row.append(remove);
    list.append(row);
  }
}

function wire() {
  if (wired) return;
  wired = true;

  element('add-folder-plugin')?.addEventListener('click', async () => {
    status('');
    try {
      const info = await addFolderPlugin();
      if (info) {
        status(`${info.title || info.id} added.`);
        await renderInstalled();
        onChanged();
      }
    } catch (e) {
      status(message(e), true);
    }
  });

  element('plugin-reload')?.addEventListener('click', async () => {
    status('');
    catalogCache = null;
    await renderInstalled();
    await renderRegistries();
    status('Plugins reloaded.');
  });

  element('registry-form')?.addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = element('registry-name').value.trim();
    const url = element('registry-url').value.trim();
    if (!url) {
      registryStatus('Enter the registry’s address.', true);
      element('registry-url').focus({ preventScroll: true });
      return;
    }
    try {
      await call('add_registry', { name, url });
      element('registry-name').value = '';
      element('registry-url').value = '';
      catalogCache = null;
      registryStatus(`${name || url} added. Refresh to fetch it.`);
      await renderRegistries();
      element('refresh-registries')?.focus({ preventScroll: true });
    } catch (err) {
      registryStatus(message(err), true);
    }
  });

  element('refresh-registries')?.addEventListener('click', async () => {
    const button = element('refresh-registries');
    button.disabled = true;
    registryStatus('Refreshing…');
    try {
      const statuses = await call('refresh_registries');
      saveStatuses(statuses ?? []);
      catalogCache = null;
      const failed = (statuses ?? []).filter((s) => !s.ok).length;
      registryStatus(
        failed
          ? `${failed} of ${statuses.length} failed (see below).`
          : `Refreshed ${statuses?.length ?? 0} registries.`,
        failed > 0,
      );
      await renderRegistries();
    } catch (e) {
      registryStatus(`Couldn’t refresh registries: ${message(e)}`, true);
    } finally {
      button.disabled = false;
    }
  });
}

async function render() {
  await renderLock();
  await renderInstalled();
  await renderRegistries();
}

export function initPluginSettings(hooks = {}) {
  if (hooks.onChanged) onChanged = hooks.onChanged;
  wire();
  onPluginsChanged(() => {
    // A plan-card install changes the list Settings shows next time.
    catalogCache = null;
  });
  return { render };
}
