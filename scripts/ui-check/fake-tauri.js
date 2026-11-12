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

  // The learning fixture (`?fixture=learn`) for the `map` and `session`
  // cases: a small knowledge graph with one topic-split proposal, and one
  // Learn session in Teach whose hunt came back and was debriefed. The graph
  // commands change it the way the backend would (`decide_splits` adds the
  // child topics and moves the ideas, `place_node` moves one), so the Map's
  // reload shows the result.
  const learnFixture = new URLSearchParams(location.search).get('fixture') === 'learn';
  const at = '2026-11-03T10:00:00.000Z';
  const graph = {
    topics: [
      { id: 'math', title: 'Mathematics', parent: null },
      { id: 'calc', title: 'Calculus', parent: 'math' },
      { id: 'alg', title: 'Algebra', parent: 'math' },
    ],
    nodes: [
      ['limit', 'Limit of a function', 'fact', 'calc'],
      ['continuity', 'Continuity at a point', 'mechanism', 'calc'],
      ['derivative', 'Derivative as a limit', 'mechanism', 'calc'],
      ['chain', 'Chain rule', 'procedure', 'calc'],
      ['quadratic', 'Quadratic formula', 'procedure', 'alg'],
      ['factor', 'Factoring a trinomial', 'procedure', 'alg'],
    ].map(([id, title, kind, home]) => ({ id, title, statement: `${title}: the claim.`, kind, home, correct: 0, missed: 0 })),
    edges: [
      { from: 'limit', to: 'continuity', relation: 'requires', weight: 'load-bearing' },
      { from: 'limit', to: 'derivative', relation: 'requires', weight: 'load-bearing' },
      { from: 'derivative', to: 'chain', relation: 'requires', weight: 'load-bearing' },
      { from: 'factor', to: 'quadratic', relation: 'requires', weight: 'load-bearing' },
    ],
    chains: [], splitsProposed: [], venues: [], topicVenues: {}, topicChecks: {},
  };
  let splitProposals = [{
    topic: 'calc',
    groups: [
      { title: 'Limit of a function (1)', nodeIds: ['limit', 'continuity'], why: '2 ideas linked by prerequisites' },
      { title: 'Derivative as a limit (2)', nodeIds: ['derivative', 'chain'], why: '2 ideas linked by prerequisites' },
    ],
  }];
  const slug = (t) => t.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  const placed = []; // learner moves, for the provenance record
  const plan = {
    version: 1, goal: 'Understand derivatives', approach: 'From limits to the chain rule.', depth: 'solid', explore: 'suggest',
    minutesPerSitting: null, style: 'socratic',
    nodes: [
      { id: 'limit', title: 'Limit of a function', kind: 'fact', requires: [] },
      { id: 'derivative', title: 'Derivative as a limit', kind: 'mechanism', requires: ['limit'] },
      { id: 'chain', title: 'Chain rule', kind: 'procedure', requires: ['derivative'] },
    ],
    sections: [
      { id: 's1', title: 'Limits', nodeIds: ['limit'], mode: 'guided', style: 'socratic', plugins: [], notes: '', exit: 'Explains a limit in words.',
        targetLevel: 'explain', hunt: { available: true, enabled: true, reason: 'People argue about what a limit is.' } },
      { id: 's2', title: 'Derivatives', nodeIds: ['derivative', 'chain'], mode: 'guided', style: 'socratic', plugins: [], notes: 'Start from secants.',
        exit: 'Differentiates a composite function.', targetLevel: 'use' },
    ],
    tracks: [],
  };
  const claims = [
    { claim: 'A limit is the value the function reaches.', verdict: 'partial', note: 'It is the value approached; the function need not reach it.', source: 'a forum thread' },
    { claim: 'Limits only exist for continuous functions.', verdict: 'wrong', note: 'A removable gap still has a limit.' },
    { claim: 'The epsilon-delta definition makes "approaches" precise.', verdict: 'correct', note: 'Exactly what it is for.', source: 'lecture notes' },
    { claim: 'Sketch the graph before computing a limit.', verdict: 'tip', note: 'A good habit, not a rule.', attribution: 'calculus teachers', scope: 'single-variable functions you can sketch' },
    { claim: 'Limits in topology use neighbourhoods.', verdict: 'offroute', note: 'Saved for later.' },
  ];
  const session = { id: 'L1', mode: 'learn', title: 'Derivatives', createdAt: at, updatedAt: at };
  const hunt = {
    huntId: 'h1', sectionId: 's1', question: 'Find out what people mean by a limit, and where they disagree.',
    lookAt: [{ kind: 'forum', why: 'see the arguments' }], bringBack: ['claims'], state: 'returned',
    returned: { notes: 'Some say a limit is reached, some say approached.', links: [], files: [] }, debriefed: true,
  };
  const sessionOps = [
    { op: 'message', opId: 'o1', at, role: 'user', content: 'Teach me derivatives.' },
    { op: 'plan', opId: 'o2', at, payload: plan },
    { op: 'phase', opId: 'o3', at, phase: 'teach', sectionId: 's1' },
    { op: 'tool', opId: 'o4', at, agent: 'tutor', callId: 'h1', name: 'hunt_send', args: { sectionId: 's1', question: hunt.question, lookAt: hunt.lookAt, bringBack: hunt.bringBack } },
    { op: 'tool', opId: 'o5', at, agent: 'tutor', callId: 'h1', result: { content: 'Hunt sent.', isError: false } },
    { op: 'message', opId: 'o6', at, role: 'user', content: '[hunt return] Some say reached, some say approached.' },
    { op: 'message', opId: 'o7', at, role: 'assistant', content: 'Good finds. Let me sort what you brought back before we start.' },
    { op: 'tool', opId: 'o8', at, agent: 'tutor', callId: 'd1', name: 'debrief_record', args: { huntId: 'h1', claims } },
    { op: 'debrief', opId: 'o9', at, huntId: 'h1', claims },
    { op: 'tool', opId: 'o10', at, agent: 'tutor', callId: 'd1', result: { content: 'Debrief recorded (5 claims).', isError: false } },
  ];
  const sessionState = {
    phase: 'teach', ended: false, plan, planVersion: 1, approvedVersion: 1, currentSection: 's1', completedSections: [], checks: {},
    activePlugins: [], lastNodeId: null, knownRefuted: [], nodeLevels: {}, hunt, hunts: [hunt], pendingPlanQuestion: null,
    openJobs: [], queuedJobs: [], grants: [], fileGrants: [], workspace: null, calibration: null, branch: null,
  };
  const learnHandlers = {
    list_sessions: () => [structuredClone(session)],
    load_session: ({ id }) => {
      if (id !== 'L1') throw new Error(`no session ${id}`);
      return { ...structuredClone(session), ops: structuredClone(sessionOps) };
    },
    session_state: () => structuredClone(sessionState),
    vision_capabilities: () => ({}),
    edit_plan: () => null,
    set_hunt: () => null,
    knowledge_map: () => structuredClone(graph),
    edge_proposals: () => ({ proposals: [], titles: {} }),
    graph_splits: () => structuredClone(splitProposals),
    decide_splits: ({ accept = [], dismiss = [], titles = {} }) => {
      for (const topic of accept) {
        const p = splitProposals.find((s) => s.topic === topic);
        if (!p) throw new Error(`no split is proposed for: ${topic}`);
        p.groups.forEach((g, i) => {
          const title = titles[topic]?.[i]?.trim() || g.title;
          const id = slug(title);
          graph.topics.push({ id, title, parent: topic });
          for (const n of graph.nodes) if (g.nodeIds.includes(n.id) && n.home === topic) n.home = id;
        });
      }
      splitProposals = splitProposals.filter((p) => !accept.includes(p.topic) && !dismiss.includes(p.topic));
      return { accepted: accept, dismissed: dismiss, remaining: splitProposals.length };
    },
    place_node: ({ nodeId, home }) => {
      const n = graph.nodes.find((x) => x.id === nodeId);
      if (!n) throw new Error(`there is no idea \`${nodeId}\` on the map`);
      if (n.home === home) throw new Error(`\`${n.title}\` is already in that topic`);
      const from = n.home;
      n.home = home;
      placed.push({ nodeId, home, at: new Date().toISOString() });
      return { nodeId, from, to: home };
    },
    node_provenance: ({ nodeId }) => {
      const n = graph.nodes.find((x) => x.id === nodeId);
      if (!n) throw new Error(`there is no idea \`${nodeId}\` on the map`);
      const events = [
        nodeId === 'quadratic'
          ? { at, kind: 'created', nodeId, provenance: { type: 'import', at } }
          : { at, kind: 'created', nodeId, provenance: { type: 'builder', sessionId: 'L1', at, quote: `${n.title} is what the lesson rests on.` }, sessionTitle: 'Derivatives', home: 'calc' },
      ];
      if (nodeId === 'chain') {
        events.push({ at, kind: 'merged', nodeId, other: 'chain-2', otherTitle: 'Differentiating a composition' });
      }
      for (const p of placed) if (p.nodeId === nodeId) events.push({ at: p.at, kind: 'placed', nodeId, home: p.home, by: 'learner', provenance: { type: 'learner', at: p.at } });
      return { nodeId, title: n.title, aliases: nodeId === 'chain' ? ['chain-2'] : [], events };
    },
  };

  const handlers = {
    ...syncHandlers,
    ...appHandlers,
    ...(learnFixture ? learnHandlers : {}),
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
