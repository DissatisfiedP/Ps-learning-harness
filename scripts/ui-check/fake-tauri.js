// Stand-in for the Tauri bridge, loaded before the UI modules by host.html
// and by the app page (index.html?check). It answers the commands the
// artifact figures and view host use, plus sync and what the app shell reads
// at startup, and logs every call, so run.js can prove that nothing inside a view frame
// reached it. `window.ipc` and `window.webkit.messageHandlers.ipc` mimic the
// platform IPC handles Tauri installs in the main frame; any use is an escape.
(() => {
  const log = [];
  const forbidden = [];

  const plugins = [
    {
      id: 'circuit', title: 'Circuits', description: 'Draw, solve and explore DC and AC circuits.', version: '0.1.0', api: 1,
      platforms: ['desktop', 'mobile'], permissions: [], enabled: true, source: 'folder circuit', packageHash: 'check',
      hasView: true, tools: ['circuit_draw', 'circuit_solve'],
    },
    {
      id: 'ipc-probe', title: 'IPC escape probe', description: 'Security fixture.', version: '0.1.0', api: 1,
      platforms: ['desktop', 'mobile'], permissions: [], enabled: true, source: 'folder ipc-probe', packageHash: 'check',
      hasView: true, tools: ['ipc_probe_show'],
    },
  ];
  const views = {
    circuit: { name: 'circuit', html: '', calls: ['solve', 'render'], answer: true },
    'ipc-probe': { name: 'ipc-probe', html: '', calls: [], answer: false },
  };

  // The circuit solver for the one fixture this check draws: a source V1
  // over R1 and R2 with a Vout probe across R2. The real solver is covered
  // by plugins/circuit/tests and crates/harness/tests/plugins_e2e.rs.
  const part = (args, id) => args.components.find((c) => c.id === id);
  const solve = (args) => {
    const v = part(args, 'V1').value;
    const r1 = part(args, 'R1').value;
    const r2 = part(args, 'R2').value;
    return { probes: { Vout: (v * r2) / (r1 + r2) }, warnings: [] };
  };
  const render = (args) => {
    const vout = args.result?.probes?.Vout;
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 40"><text x="8" y="24" fill="currentColor">Vout = ${vout}</text></svg>`;
  };
  const viewCall = ({ pluginId, function: fn, args }) => {
    if (pluginId === 'circuit' && fn === 'solve') return solve(args);
    if (pluginId === 'circuit' && fn === 'render') return render(args);
    throw new Error(`\`${fn}\` is not one of the view's callable functions`);
  };

  // Sync (docs/sync-build.md) for the `sync` case, which loads the whole app
  // (index.html?check). `?sync=` picks the starting state: none (default),
  // synced, error or attention. A remote or URL containing "offline" fails.
  const ago = (min) => new Date(Date.now() - min * 60_000).toISOString();
  const sync = {
    remote: null, token: null, branch: 'main', lastSyncAt: null, lastError: null, ahead: 0, behind: 0, unsynced: false,
    needsAttention: [], statusFails: false, // run.js flips statusFails to prove the indicator hides quietly
    webdav: { url: null, username: null, password: null, lastSyncAt: null, lastError: null, unsynced: false },
  };
  const scenario = new URLSearchParams(location.search).get('sync') ?? 'none';
  if (scenario !== 'none') {
    Object.assign(sync, { remote: 'https://github.com/learner/notes.git', token: 'ghp-check', lastSyncAt: ago(5) });
    Object.assign(sync.webdav, { url: 'https://dav.example.com/harness', username: 'learner', password: 'app-pass', lastSyncAt: ago(12) });
  }
  if (scenario === 'error') Object.assign(sync, { lastError: 'the token was rejected; save a new one in Settings → Sync', unsynced: true, ahead: 2 });
  if (scenario === 'attention') sync.needsAttention = ['graph/topics.json', 'prompts/tutor.md'];
  const syncStatus = () => ({
    configured: Boolean(sync.remote), remote: sync.remote, branch: sync.branch, lastSyncAt: sync.lastSyncAt,
    lastError: sync.lastError, unsynced: sync.unsynced, ahead: sync.ahead, behind: sync.behind,
    needsAttention: [...sync.needsAttention], hasToken: Boolean(sync.token),
    webdav: {
      configured: Boolean(sync.webdav.url), url: sync.webdav.url, username: sync.webdav.username,
      hasPassword: Boolean(sync.webdav.password), lastSyncAt: sync.webdav.lastSyncAt,
      lastError: sync.webdav.lastError, unsynced: Boolean(sync.webdav.url) && (sync.webdav.unsynced || !sync.webdav.lastSyncAt),
    },
  });
  const davUrl = /^(https:\/\/|http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?(\/|$))/i;
  const syncHandlers = {
    sync_status: () => {
      if (sync.statusFails) throw new Error('could not read sync.json');
      return syncStatus();
    },
    set_sync: ({ remote, token, branch }) => {
      const r = remote?.trim();
      if (!r) Object.assign(sync, { remote: null, token: null, needsAttention: [], lastError: null, ahead: 0, behind: 0, unsynced: false });
      else {
        if (r !== sync.remote) sync.unsynced = true;
        sync.remote = r;
        if (token != null) sync.token = token.trim() || null;
        sync.lastError = null;
      }
      if (branch?.trim()) sync.branch = branch.trim();
      return syncStatus();
    },
    set_webdav: ({ url, username, password }) => {
      const u = url?.trim();
      if (!u) {
        sync.webdav = { url: null, username: null, password: null, lastSyncAt: null, lastError: null, unsynced: false };
        return syncStatus();
      }
      if (!davUrl.test(u)) throw new Error('WebDAV needs an https:// address (http only for localhost).');
      if (u !== sync.webdav.url) Object.assign(sync.webdav, { lastSyncAt: null, unsynced: true });
      Object.assign(sync.webdav, { url: u, username: username?.trim() || null, lastError: null });
      if (password != null) sync.webdav.password = password || null;
      return syncStatus();
    },
    sync_now: () => new Promise((resolve, reject) => {
      setTimeout(() => {
        if (!sync.remote && !sync.webdav.url) return reject('sync is not configured');
        const errors = [];
        const report = { committed: false, pulled: 0, pushed: 0, conflicts: [], needsAttention: [], webdav: null };
        if (sync.remote) {
          if (sync.remote.includes('offline')) {
            sync.lastError = 'the network is unreachable; try again when you are online';
            errors.push(sync.lastError);
          } else {
            Object.assign(report, { committed: true, pulled: 1, pushed: sync.ahead || 1 });
            Object.assign(sync, { lastSyncAt: new Date().toISOString(), lastError: null, unsynced: false, ahead: 0, behind: 0 });
          }
        }
        if (sync.webdav.url) {
          if (sync.webdav.url.includes('offline')) {
            sync.webdav.lastError = 'the WebDAV server could not be reached';
            errors.push(`WebDAV: ${sync.webdav.lastError}`);
          } else {
            report.webdav = { uploaded: 3, downloaded: 1, merged: 0, deletedRemote: 0, deletedLocal: 0, skipped: 0 };
            Object.assign(sync.webdav, { lastSyncAt: new Date().toISOString(), lastError: null, unsynced: false });
          }
        }
        return errors.length ? reject(errors.join('; ')) : resolve(report);
      }, 400);
    }),
    acknowledge_conflicts: () => {
      sync.needsAttention = [];
      return syncStatus();
    },
    // Best-effort background sync when the app is hidden; the fake only counts calls.
    sync_if_changed: () => {
      sync.backgroundCalls = (sync.backgroundCalls || 0) + 1;
      return false;
    },
    sync_in_background: () => {
      sync.catchUpCalls = (sync.catchUpCalls || 0) + 1;
      return false;
    },
  };

  // The rest of what the app shell asks for at startup and when Settings opens,
  // plus a WorkBuddy sign-in that waits until run.js calls __bridge.finishLogin.
  const providers = [
    { id: 'workbuddy', name: 'WorkBuddy', source: null, keyUrl: null },
    { id: 'anthropic', name: 'Anthropic', source: 'saved', credential: 'api-key', keyUrl: null, defaultBaseUrl: 'https://api.anthropic.com' },
    { id: 'openai', name: 'OpenAI', source: null, keyUrl: 'https://platform.openai.com/api-keys', defaultBaseUrl: 'https://api.openai.com/v1' },
  ];
  const settings = { roles: {} };
  let finishLogin = null;
  let libraries = [];
  const appHandlers = {
    subscribe_events: () => null,
    list_sessions: () => [],
    list_providers: () => structuredClone(providers),
    get_settings: () => structuredClone(settings),
    list_models: ({ provider }) => [{ id: `${provider}-model`, name: `${provider} model` }],
    set_role_model: ({ role, choice }) => {
      settings.roles[role] = choice;
      return null;
    },
    workbuddy_login_start: () => ({ state: 'check-state', authUrl: 'https://workbuddy.example/sign-in' }),
    workbuddy_login_wait: () => new Promise((resolve) => {
      finishLogin = (account) => {
        Object.assign(providers[0], { source: 'saved', credential: 'workbuddy-account', account });
        resolve(account);
      };
    }),
    workbuddy_login_cancel: () => null,
    list_roles: () => [],
    role_usage: () => [],
    focus_dnd_support: () => null,
    missing_locked: () => [],
    registries: () => [],
    registry_catalog: () => [],
    frontier: () => [],
    libraries: () => structuredClone(libraries),
    pick_folder: () => '/home/learner/huawei-ict',
    // An import streams progress over its channel, then adds the library.
    library_import: ({ path, onProgress }) => {
      const slug = path.split('/').at(-1);
      onProgress?.onmessage?.({ slug, stage: 'scan', done: 0, total: 0 });
      onProgress?.onmessage?.({ slug, stage: 'extract', done: 0, total: 2, path: 'outline.pdf' });
      const lib = {
        slug, name: slug, description: '', topic: '', updated: '', files: 2,
        kinds: { guide: 1, 'question-bank': 1 }, noText: 0, failed: 0, truncated: 0, textBytes: 2048,
        skipped: { 'no-text-type': 3 }, skippedSamples: {}, maxFileBytes: 52428800, source: path, originals: true,
      };
      libraries = [...libraries.filter((l) => l.slug !== slug), lib];
      return { library: lib, added: 2, changed: 0, unchanged: 0, removed: 0 };
    },
  };

  const handlers = {
    ...syncHandlers,
    ...appHandlers,
    plugins: () => plugins,
    plugin_view: ({ pluginId }) => {
      if (!views[pluginId]) throw new Error(`plugin \`${pluginId}\` has no view`);
      return views[pluginId];
    },
    plugin_view_url: ({ pluginId }) => `http://pluginview.localhost/${pluginId}`,
    plugin_view_call: viewCall,
    view_state: () => null,
  };

  function invoke(cmd, args = {}) {
    let logged = args;
    try {
      logged = structuredClone(args);
    } catch {
      // A Channel carries its callback; log the argument names only.
      logged = Object.fromEntries(Object.keys(args).map((k) => [k, typeof args[k]]));
    }
    log.push({ cmd, args: logged });
    const handler = handlers[cmd];
    if (!handler) return Promise.reject(`unexpected command ${cmd}`);
    try {
      return Promise.resolve(handler(args)).catch((err) => Promise.reject(String(err?.message ?? err)));
    } catch (err) {
      return Promise.reject(String(err.message ?? err));
    }
  }

  window.__bridge = { log, forbidden, sync, finishLogin: (account) => finishLogin?.(account) };
  window.__TAURI__ = { core: { invoke, Channel: class {} } };
  window.__TAURI_INTERNALS__ = { invoke };
  window.ipc = { postMessage: (message) => forbidden.push({ via: 'window.ipc', message: String(message) }) };
  window.webkit = {
    messageHandlers: { ipc: { postMessage: (message) => forbidden.push({ via: 'webkit', message: String(message) }) } },
  };
})();
