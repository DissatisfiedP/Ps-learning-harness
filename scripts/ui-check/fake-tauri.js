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
  // `?builtins=1`: the compiled-in plugins, as the core lists them -- one
  // that needs no permissions (on from the start), two that declare some
  // (off until the learner consents).
  if (new URLSearchParams(location.search).get('builtins') === '1') {
    plugins.push(
      {
        id: 'code-parse', title: 'Code structure', description: 'Parses source into a syntax tree.', version: '0.1.0', api: 1,
        platforms: ['desktop', 'mobile'], permissions: [], enabled: true, source: 'built-in', packageHash: 'builtin-code-parse',
        hasView: false, tools: ['code_parse'], builtin: true, onPlatform: true,
      },
      {
        id: 'cas', title: 'Computer algebra', description: 'Exact algebra with SymPy.', version: '0.1.0', api: 1,
        platforms: ['desktop'], permissions: ['exec'], enabled: false, source: 'built-in', packageHash: 'builtin-cas',
        hasView: true, tools: ['cas_solve'], builtin: true, onPlatform: true,
      },
      {
        id: 'live-search', title: 'Live search', description: 'Searches the web while teaching.', version: '0.1.0', api: 1,
        platforms: ['desktop', 'mobile'], permissions: ['net'], enabled: false, source: 'built-in', packageHash: 'builtin-live-search',
        hasView: false, tools: ['live_search'], builtin: true, onPlatform: true,
      },
    );
  }
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
  let stagedImages = 0;
  const stagedData = new Map();
  // Where keys are kept; run.js switches it to the 0600-file fallback.
  const secretStorage = { backend: 'os', label: 'the Secret Service (desktop keyring)', secure: true, notice: null };
  let finishLogin = null;
  let libraries = [];
  let imageSets = [];
  const appHandlers = {
    subscribe_events: () => null,
    list_sessions: () => [],
    list_providers: () => structuredClone(providers),
    secret_storage: () => structuredClone(secretStorage),
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
    pick_files: () => ['/home/learner/worksheet.txt'],
    stage_file: ({ path }) => ({ kind: 'file', path, bytes: 2048, name: path.split('/').at(-1) }),
    // Scratch-pad drawings: staged like pasted images, then loaded by path.
    stage_image: ({ data, name }) => {
      const path = `attachments/2026/STAGED${++stagedImages}.png`;
      stagedData.set(path, data);
      return { kind: 'image', path, mediaType: 'image/png', bytes: data.length, name };
    },
    unstage_image: ({ path }) => {
      stagedData.delete(path);
      return null;
    },
    attachment_data: ({ path }) => stagedData.get(path) ?? '',
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
    image_sets: () => structuredClone(imageSets),
    // A labelled folder becomes a set (subfolder name = label).
    image_set_import: ({ path, name }) => {
      const title = name || path.split('/').at(-1);
      const set = { id: title.toLowerCase(), title, size: 7, labels: [{ label: 'crow', count: 4 }, { label: 'raven', count: 3 }] };
      imageSets = [...imageSets.filter((s) => s.id !== set.id), set];
      return { set, skipped: 1 };
    },
    image_set_remove: ({ id }) => {
      imageSets = imageSets.filter((s) => s.id !== id);
      return null;
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
  // `&mathTitle=1` (the `touch` case): the first idea's title carries a formula.
  if (new URLSearchParams(location.search).get('mathTitle')) {
    for (const list of [graph.nodes, plan.nodes]) list.find((n) => n.id === 'limit').title = 'Limit $\\alpha_n \\to 0$';
  }
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
  // `&reading=check|predict` (the `reading` case): `__bridge.deliverReading()`
  // plays a tutor turn that delivers its explanation with `message_send` and
  // opens a question of that role, as a live turn would.
  const readingRole = new URLSearchParams(location.search).get('reading');
  // Long enough that the question arrives below the fold.
  const readingMessage = ['A derivative is the limit of secant slopes: **shrink the step** and the slope settles.',
    ...Array.from({ length: 40 }, (_, i) => `Step ${i + 1}: halve the step again and the secant turns a little further towards the tangent.`)].join('\n\n');
  const readingOps = [
    { op: 'tool', opId: 'o11', at, agent: 'tutor', callId: 'm1', name: 'message_send', args: { message: readingMessage } },
    { op: 'message', opId: 'o12', at, role: 'assistant', tag: 'sent', content: readingMessage },
    { op: 'tool', opId: 'o13', at, agent: 'tutor', callId: 'm1', result: { content: 'Shown to the learner.', isError: false } },
    // `&padFigure=1` (the `pad` case): the tutor also made a figure, which
    // the scratch pad can use as its background.
    ...(new URLSearchParams(location.search).get('padFigure')
      ? [{ op: 'artifact', opId: 'o13a', at, artifactId: 'A1', pluginId: 'static', pluginVersion: '1', packageHash: 'x', tool: 'render',
        input: {}, data: { title: 'Voltage divider' },
        fallbackSvg: '<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120" viewBox="0 0 240 120" fill="none" stroke="currentColor"><rect x="20" y="20" width="60" height="30"/><rect x="20" y="70" width="60" height="30"/><path d="M80 35H160M80 85H160M160 35V85"/><text x="170" y="64" fill="currentColor" stroke="none">Vout</text></svg>' }]
      : []),
    { op: 'tool', opId: 'o14', at, agent: 'tutor', callId: 'c1', name: 'question_ask',
      args: { role: readingRole, level: 'explain', nodeId: 'limit', format: 'free', prompt: 'Why does the secant slope settle?', answer: 'The step shrinks to zero.' } },
  ];
  const sessionState = {
    phase: 'teach', ended: false, plan, planVersion: 1, approvedVersion: 1, currentSection: 's1', completedSections: [], checks: {},
    activePlugins: [], lastNodeId: null, knownRefuted: [], nodeLevels: {}, hunt, hunts: [hunt], pendingPlanQuestion: null,
    openJobs: [], queuedJobs: [], grants: [], fileGrants: [], workspace: null, calibration: null, branch: null,
  };
  const learnHandlers = {
    list_sessions: () => [structuredClone(session)],
    // An answer that carries drawings comes back as its stored result, the
    // way the backend's `answer_result` writes it.
    answer: ({ callId, payload }) => {
      if (payload?.sketches) {
        const content = { questionId: callId, answer: payload.answer, thinking: payload.thinking, dontKnow: false,
          sketches: payload.sketches.map((s) => ({ in: s.in, path: s.attachment.path })) };
        emit({ kind: 'op', sessionId: 'L1', op: { op: 'tool', opId: 'o99', at, agent: 'tutor', callId, result: { content: JSON.stringify(content), isError: false } } });
      }
      return null;
    },
    subscribe_events: ({ onEvent }) => {
      channels.push(onEvent);
      return null;
    },
    load_session: ({ id }) => {
      if (id !== 'L1') throw new Error(`no session ${id}`);
      return { ...structuredClone(session), ops: structuredClone(sessionOps) };
    },
    // `&turnError=…` makes the backend report a failed last turn.
    session_state: () => ({
      ...structuredClone(sessionState),
      ...(new URLSearchParams(location.search).get('turnError')
        ? { lastTurnError: new URLSearchParams(location.search).get('turnError') }
        : {}),
    }),
    vision_capabilities: () => ({}),
    edit_plan: () => null,
    set_hunt: () => null,
    // The learner model: the chain rule is shaky (an open misconception),
    // the limit is held.
    learner_nodes: () => ({
      chain: {
        retrievability: 0.62, stability: 2.1, difficulty: 6.2, lastReview: '2026-11-10T09:00:00Z', due: '2026-11-12T09:00:00Z',
        held: false, layers: { recall: true, discrimination: false, composition: false },
        misconceptions: [{ text: 'differentiates the outer function only', at: '2026-11-10T09:00:00Z', sessionId: 'L1' }],
        reviews: 3, lapses: 1,
      },
      limit: {
        retrievability: 0.97, stability: 30, difficulty: 4.1, lastReview: '2026-11-12T09:00:00Z', due: '2026-12-10T09:00:00Z',
        held: true, layers: { recall: true, discrimination: true, composition: false }, misconceptions: [], reviews: 5, lapses: 0,
      },
    }),
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

  // The coach's Practice commands and events (docs/ui-design.md §11b) for the
  // `practice` and `practice-set` cases (`?practice=1`, with `&fixture=learn`
  // for the graph). One in-memory session per `start_practice`; a coach turn
  // is a turnStart, ops and text deltas, then a turnEnd, with short pauses so
  // the page can be seen mid-turn. `&practiceSeed=failed,owed,cut,legacy`
  // adds sessions a restart or a failed turn leaves behind.
  const practiceParams = new URLSearchParams(location.search);
  const practiceFixture = practiceParams.get('practice') === '1';
  const practiceSeed = (practiceParams.get('practiceSeed') ?? '').split(',').filter(Boolean);
  const channels = [];
  const emit = (ev) => channels.forEach((ch) => ch.onmessage?.(ev));
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  const practices = new Map();
  let opSeq = 0;
  const stamp = () => new Date().toISOString();
  const bank = [
    { target: 'chain', title: 'Chain rule', home: 'calc', statement: 'Differentiate $f(x)=\\sin(3x^2)$.', retest: 'Differentiate $g(x)=\\cos(5x^3)$.',
      solution: [{ move: 'Name the outer function $\\sin u$ and the inner $u=3x^2$.', decisionPoint: true }, { move: 'Multiply $\\cos u$ by $u\'=6x$.', decisionPoint: false }],
      finalAnswer: '$f\'(x)=6x\\cos(3x^2)$' },
    { target: 'limit', title: 'Limit of a function', home: 'calc', statement: 'What does $\\lim_{x\\to 2}\\frac{x^2-4}{x-2}$ equal?', retest: 'What does $\\lim_{x\\to 3}\\frac{x^2-9}{x-3}$ equal?',
      solution: [{ move: 'Factor the numerator: $(x-2)(x+2)$.', decisionPoint: true }, { move: 'Cancel $x-2$ and substitute $x=2$.', decisionPoint: false }],
      finalAnswer: '$4$' },
    { target: 'derivative', title: 'Derivative as a limit', home: 'calc', statement: 'Use the definition to find the slope of $g(x)=x^2$ at $x=1$.', retest: 'Use the definition to find the slope of $h(x)=x^2$ at $x=2$.',
      solution: [{ move: 'Write $\\frac{g(1+h)-g(1)}{h}$.', decisionPoint: false }, { move: 'Simplify to $2+h$ and let $h\\to 0$.', decisionPoint: true }],
      finalAnswer: '$2$' },
  ];
  const summaryOf = (S) => ({
    id: S.id, mode: 'practice', title: S.title, createdAt: S.createdAt, updatedAt: S.updatedAt, readOnly: false,
    readOnlyReason: null, desktopOnlyPlugins: [], lineage: null,
  });
  const entryOf = (S, p) => bank.find((b) => b.target === p.target) ?? bank[0];
  const refOf = (b) => ({ id: b.target, title: b.title, home: b.home });

  function newPractice({ id, format = 'stream', picked = [], size = 3, title = 'Practice' }) {
    const S = {
      id, format, size, title, picked, problems: [], feed: [], ops: [], answers: [], submitted: false, wrapUp: false, ended: false,
      running: false, lastError: null, legacy: false, messages: [], createdAt: stamp(), updatedAt: stamp(),
    };
    practices.set(id, S);
    return S;
  }
  function emitOp(S, op) {
    S.updatedAt = stamp();
    emit({ kind: 'op', sessionId: S.id, op: { opId: `o${++opSeq}`, at: stamp(), ...op } });
  }
  function addProblem(S, retestOf = null) {
    const index = S.problems.length;
    const want = S.picked.length ? S.picked[index % S.picked.length] : null;
    const source = retestOf ? S.problems.find((p) => p.id === retestOf) : null;
    const entry = source ? entryOf(S, source) : (bank.find((b) => b.target === want) ?? bank[index % bank.length]);
    const p = { id: `p${index + 1}`, index, target: entry.target, status: 'writing', hints: 0, retestOf, statement: null, result: null };
    S.problems.push(p);
    emitOp(S, { op: 'practice', event: 'next', problemId: p.id, index });
    return p;
  }
  function pose(S, p) {
    const entry = entryOf(S, p);
    p.status = 'open';
    p.statement = p.retestOf ? entry.retest : entry.statement;
    p.posedAt = stamp();
    S.feed.push({ kind: 'problem', problemId: p.id, at: stamp() });
    emitOp(S, { op: 'practice', event: 'pose', problemId: p.id });
  }
  function conclude(S, p, verdict) {
    const entry = entryOf(S, p);
    p.status = 'concluded';
    p.result = {
      verdict, hintRung: p.hints ? 1 : null,
      ...(verdict === 'wrong' ? { missClass: 'execution', misconception: 'Differentiated only the outer function.' } : {}),
      rating: verdict === 'correct' ? (p.hints ? 'good' : 'easy') : 'again', durationSec: 95,
      targets: [refOf(entry)], hosts: [], layer: 'recall', level: 'use', cueDistance: 1, isDistractor: false,
      ...(p.retestOf ? { retestOf: p.retestOf } : {}),
      reveal: { solution: entry.solution, finalAnswer: entry.finalAnswer, acceptableVariants: [], rubric: [] },
    };
    S.feed.push({ kind: 'concluded', problemId: p.id, verdict, at: stamp() });
    emitOp(S, { op: 'practice', event: 'conclude', problemId: p.id, conclusion: { verdict } });
  }
  /** One coach message: text deltas, then the persisted message (the feed gets it). */
  async function say(S, text) {
    for (let i = 0; i < text.length; i += 14) {
      emit({ kind: 'text', sessionId: S.id, agent: 'coach', delta: text.slice(i, i + 14) });
      await pause(12);
    }
    const opId = `o${++opSeq}`;
    S.feed.push({ kind: 'coach', text, thinking: '', opId, at: stamp() });
    S.updatedAt = stamp();
    emit({ kind: 'op', sessionId: S.id, op: { op: 'message', opId, at: stamp(), role: 'assistant', content: text } });
  }
  async function turn(S, body) {
    if (S.running) throw new Error('a coach turn is already running');
    S.running = true;
    S.lastError = null;
    emit({ kind: 'turnStart', sessionId: S.id, agent: 'coach' });
    let err = null;
    try {
      if (practiceParams.get('practiceFail') === 'start' && !S.failedOnce) {
        S.failedOnce = true;
        throw new Error('DeepSeek is not connected');
      }
      await body();
    } catch (e) {
      err = String(e.message ?? e);
      S.lastError = err;
    }
    S.running = false;
    emit({ kind: 'turnEnd', sessionId: S.id, agent: 'coach', ...(err ? { error: err } : {}) });
    if (err) throw new Error(err);
  }
  const retestsDue = (S) =>
    S.problems.filter((p) => !p.retestOf && p.status === 'concluded' && ['wrong', 'dontknow', 'dismissed'].includes(p.result.verdict)
      && !S.problems.some((q) => q.retestOf === p.id));
  const retestsLeft = (S) => retestsDue(S).length + S.problems.filter((p) => p.retestOf && p.status !== 'concluded').length;
  function pview(S) {
    if (S.legacy) {
      return { sessionId: S.id, legacy: true, format: 'stream', status: 'writing', wrapUp: false, setSize: null, retestsLeft: 0, turnRunning: false,
        lastTurnError: null, picked: [], problems: [], feed: [], can: { submit: false, hint: false, dismiss: false, next: false, end: false, submitSet: false }, calibration: null };
    }
    const set = S.format === 'set' || S.format === 'exam';
    const open = S.problems.find((p) => p.status === 'open');
    const writing = S.problems.some((p) => p.status === 'writing');
    let status;
    if (S.ended) status = 'ended';
    else if (set) status = writing || !S.problems.length ? 'set-writing' : !S.submitted ? 'set-open' : 'set-review';
    else status = open ? 'open' : writing || !S.problems.length ? 'writing' : 'concluded';
    const live = !S.ended && !S.running;
    const can = {
      submit: live && ['open', 'concluded', 'set-review'].includes(status),
      hint: live && !set && status === 'open',
      dismiss: live && !set && status === 'open',
      next: live && !set && status === 'concluded' && (!S.wrapUp || retestsLeft(S) > 0),
      end: live && status !== 'open' && !status.endsWith('writing') && !(S.exam && status === 'set-open'),
      submitSet: live && status === 'set-open' && !(S.exam && examView(S).closed),
    };
    return {
      sessionId: S.id, legacy: false, format: S.format, status, wrapUp: S.wrapUp, setSize: set ? S.size : null, retestsLeft: retestsLeft(S),
      turnRunning: S.running, lastTurnError: S.lastError, picked: S.picked.map((id) => refOf(bank.find((b) => b.target === id) ?? bank[0])),
      problems: S.problems.map((p) => ({
        problemId: p.id, index: p.index, status: p.status, hints: p.hints,
        ...(S.exam && p.marks ? { marks: p.marks } : {}),
        ...(p.statement ? { statement: p.statement, type: 'standard', answerCheck: 'passed', posedAt: p.posedAt } : {}),
        ...(p.result ? { result: p.result } : {}),
      })),
      feed: structuredClone(S.feed), can,
      ...(S.exam ? { exam: examView(S) } : {}),
      calibration: S.ended ? { graded: 2, buckets: [], notes: ['Confidence matches results.'] } : null,
    };
  }
  const need = (id) => {
    const S = practices.get(id);
    if (!S) throw new Error(`no practice session ${id}`);
    return S;
  };
  const ready = (S) => {
    if (S.running) throw new Error('a coach turn is already running');
    if (S.ended) throw new Error('this practice session has ended');
  };

  if (practiceFixture) settings.roles.tutor = { provider: 'anthropic', model: 'anthropic-model' };
  if (practiceFixture) {
    // Left behind by a failed turn, a restart that cut a turn short, and the old single-prompt Practice.
    if (practiceSeed.includes('failed')) {
      const S = newPractice({ id: 'PF', title: 'Practice: failed turn' });
      pose(S, addProblem(S));
      S.lastError = 'DeepSeek is not connected';
    }
    if (practiceSeed.includes('owed')) {
      const S = newPractice({ id: 'PO', title: 'Practice: reply owed' });
      pose(S, addProblem(S));
      S.feed.push({ kind: 'learner', intent: 'submission', text: 'I think it is 6x', confidence: 50, at: stamp() });
    }
    if (practiceSeed.includes('cut')) {
      const S = newPractice({ id: 'PC', title: 'Practice: problem not written' });
      addProblem(S);
    }
    if (practiceSeed.includes('legacy')) {
      const S = newPractice({ id: 'PL', title: 'Practice: older session' });
      S.legacy = true;
      S.messages = [
        { role: 'user', content: 'Practise the chain rule.', attachments: [] },
        { role: 'assistant', content: 'Differentiate $\\sin(3x^2)$ and tell me how you did it.' },
      ];
    }
  }

  const practiceHandlers = {
    subscribe_events: ({ onEvent }) => {
      channels.push(onEvent);
      return null;
    },
    list_sessions: () => [...(learnFixture ? [session] : []), ...[...practices.values()].map(summaryOf)],
    review_queue: () => [
      { id: 'chain', title: 'Chain rule', home: 'calc', retrievability: 0.62, dependents: 1 },
      { id: 'limit', title: 'Limit of a function', home: 'calc', retrievability: 0.7, dependents: 2 },
    ],
    load_session: ({ id }) => {
      if (id === 'L1' && learnFixture) return { ...structuredClone(session), ops: structuredClone(sessionOps) };
      const S = need(id);
      const ops = S.submitted ? [{ op: 'practice', opId: 'set', at: stamp(), event: 'setSubmit', answers: S.answers }] : [];
      return { ...summaryOf(S), messages: structuredClone(S.messages), ops };
    },
    start_practice: async ({ targets = [], format = 'stream', size }) => {
      if (format === 'exam') throw new Error('exam mode isn\'t built yet');
      for (const t of targets) if (!bank.some((b) => b.target === t) && !graph.nodes.some((n) => n.id === t)) throw new Error(`there is no idea \`${t}\` on the map`);
      const S = newPractice({ id: `P${practices.size + 1}`, format, picked: targets, size: size ?? 3 });
      emit({ kind: 'session', session: summaryOf(S) });
      // The scheduler plans first (a `next` op per problem); the coach then writes them.
      const planned = format === 'set' ? Array.from({ length: S.size }, () => addProblem(S)) : [addProblem(S)];
      await turn(S, async () => {
        await pause(400);
        for (const p of planned) pose(S, p);
      });
      return summaryOf(S);
    },
    practice_state: ({ sessionId }) => pview(need(sessionId)),
    practice_submit: async ({ sessionId, text, confidence, attachments }) => {
      const S = need(sessionId);
      ready(S);
      if (!text.trim()) throw new Error('the submission is empty');
      if (S.format === 'set' && !S.submitted) throw new Error('hand in the whole set first (practice_submit_set)');
      const open = S.problems.find((p) => p.status === 'open');
      S.feed.push({ kind: 'learner', intent: 'submission', text, ...(confidence != null ? { confidence } : {}), ...(attachments?.length ? { attachments } : {}), at: stamp() });
      emitOp(S, { op: 'practice', event: 'attempt', problemId: open?.id ?? '', text });
      await turn(S, async () => {
        await pause(60);
        if (open) {
          const wrong = /wrong/i.test(text);
          conclude(S, open, wrong ? 'wrong' : 'correct');
          await say(S, wrong ? 'Not quite. Check the inner function before you multiply.' : 'Yes. That is the answer, and your confidence fits.');
        } else {
          await say(S, 'Good question. The outer function is applied to the inner one first.');
        }
      });
    },
    practice_hint: async ({ sessionId }) => {
      const S = need(sessionId);
      ready(S);
      const open = S.problems.find((p) => p.status === 'open');
      if (!open || S.format === 'set') throw new Error('there is no open problem to give a hint on (a set has no hints)');
      open.hints += 1;
      S.feed.push({ kind: 'learner', intent: 'hint', text: '', at: stamp() });
      emitOp(S, { op: 'practice', event: 'hint', problemId: open.id });
      await turn(S, async () => {
        await pause(60);
        await say(S, 'Look at what sits inside the function. Which part is applied first?');
      });
    },
    practice_dismiss: async ({ sessionId }) => {
      const S = need(sessionId);
      ready(S);
      const open = S.problems.find((p) => p.status === 'open');
      if (!open || S.format === 'set') throw new Error('there is no open problem to skip');
      S.feed.push({ kind: 'learner', intent: 'dismiss', text: '', at: stamp() });
      emitOp(S, { op: 'practice', event: 'dismiss', problemId: open.id });
      conclude(S, open, 'dismissed');
      await turn(S, () => say(S, 'Skipped. We will come back to it at the end.'));
    },
    practice_next: async ({ sessionId }) => {
      const S = need(sessionId);
      ready(S);
      if (S.problems.some((p) => p.status !== 'concluded')) throw new Error('finish the current problem first');
      await turn(S, async () => {
        const p = addProblem(S, S.wrapUp ? retestsDue(S)[0]?.id ?? null : null);
        await pause(200);
        pose(S, p);
      });
    },
    practice_submit_set: async ({ sessionId, answers }) => {
      const S = need(sessionId);
      ready(S);
      if (S.format !== 'set' || S.submitted) throw new Error('this set was already handed in');
      S.submitted = true;
      S.answers = structuredClone(answers);
      S.feed.push({ kind: 'learner', intent: 'set', text: `Handed in ${answers.length} answer(s).`, at: stamp() });
      emitOp(S, { op: 'practice', event: 'setSubmit', answers });
      await turn(S, async () => {
        for (const p of S.problems) {
          await pause(120);
          const a = answers.find((x) => x.problemId === p.id);
          conclude(S, p, !a?.text?.trim() ? 'dontknow' : /wrong/i.test(a.text) ? 'wrong' : 'correct');
        }
        await say(S, 'Review: problem 1 is right. Problem 2 breaks at the inner derivative (an execution slip). Problem 3 was left empty.');
      });
    },
    practice_end: async ({ sessionId, skipRetests }) => {
      const S = need(sessionId);
      ready(S);
      if (S.problems.some((p) => p.status === 'open')) throw new Error('finish or skip the open problem before ending');
      if (S.format === 'stream' && !S.wrapUp && !skipRetests && retestsDue(S).length) {
        S.wrapUp = true;
        emitOp(S, { op: 'practice', event: 'wrap' });
        return;
      }
      S.ended = true;
      emitOp(S, { op: 'practice', event: 'end', skippedRetests: Boolean(skipRetests) || retestsLeft(S) > 0 });
    },
    practice_resume: async ({ sessionId }) => {
      const S = need(sessionId);
      ready(S);
      await turn(S, async () => {
        const writing = S.problems.filter((p) => p.status === 'writing');
        if (writing.length) {
          await pause(120);
          for (const p of writing) pose(S, p);
        } else {
          await say(S, 'Sorry about that. Your last answer was close; check the inner derivative.');
        }
      });
    },
    cancel_reply: () => null,
  };

  // The exam fixture (`&exam=1`, with `fixture=learn&practice=1`): L1 is an
  // exam goal whose plan `exam_plan` ranks (docs/ui-design.md §11c);
  // `start_exam` writes a timed paper (`&examShortMs=3000` ends its clock after
  // three seconds instead of the budget), `practice_exam_save` keeps the
  // draft, and at the deadline (plus two seconds) the paper is handed in from
  // the draft, as the backend's timer does.
  const examFixture = practiceParams.get('exam') === '1';
  const examShortMs = Number(practiceParams.get('examShortMs') ?? 0);
  const MARKS = [5, 4, 6];
  const GRACE_MS = 2000;
  const scheme = [
    { id: 'm1', criterion: 'Names the outer and the inner function (method mark)', essential: true },
    { id: 'a1', criterion: 'States the derivative with its factors', essential: false },
  ];
  function examView(S) {
    const e = S.exam;
    const now = Date.now();
    const d = e.deadlineAt ?? null;
    const done = S.problems.length > 0 && S.problems.every((p) => p.status === 'concluded');
    return {
      name: e.name, goalSessionId: e.goalSessionId, budgetSec: e.budgetSec,
      openedAt: e.openedAt ? new Date(e.openedAt).toISOString() : null, deadline: d ? new Date(d).toISOString() : null,
      now: new Date(now).toISOString(), remainingSec: d ? Math.max(0, Math.round((d - now) / 1000)) : null,
      expired: d != null && now >= d, closed: d != null && now > d + GRACE_MS,
      submitted: S.submitted, autoSubmitted: Boolean(e.auto), elapsedSec: e.elapsedSec ?? null, draft: structuredClone(e.draft),
      score: done ? { awarded: S.problems.reduce((n, p) => n + (p.result.marksAwarded ?? 0), 0), outOf: S.problems.reduce((n, p) => n + (p.marks ?? 0), 0) } : null,
    };
  }
  async function examSubmit(S, answers, auto) {
    S.submitted = true;
    S.answers = structuredClone(answers);
    S.exam.auto = auto;
    S.exam.elapsedSec = auto ? S.exam.budgetSec : Math.max(1, Math.round((Date.now() - S.exam.openedAt) / 1000));
    S.feed.push({ kind: 'learner', intent: 'set', text: auto ? `Time was up: handed in ${answers.length} saved answer(s).` : `Handed in ${answers.length} answer(s).`, at: stamp() });
    emitOp(S, { op: 'practice', event: 'setSubmit', answers, ...(auto ? { auto: true } : {}) });
    await turn(S, async () => {
      for (const p of S.problems) {
        await pause(120);
        const a = answers.find((x) => x.problemId === p.id);
        const verdict = !a?.text?.trim() ? 'dontknow' : /wrong/i.test(a.text) ? 'partial' : 'correct';
        conclude(S, p, verdict);
        p.result.marksAwarded = verdict === 'correct' ? p.marks : verdict === 'partial' ? Math.floor(p.marks / 2) : 0;
        if (verdict === 'partial') p.result.missClass = 'execution';
        p.result.reveal.rubric = scheme;
      }
      const got = S.problems.reduce((n, p) => n + p.result.marksAwarded, 0);
      await say(S, `${got} of ${S.problems.reduce((n, p) => n + p.marks, 0)} marks. Problem 1 earned full marks; problem 2 kept only its method mark; problem 3 was left empty.`);
    });
  }
  const autoSubmit = (S) => {
    if (S.submitted || S.running) return Promise.resolve();
    return examSubmit(S, S.exam.draft, true).catch(() => {});
  };
  const needExam = (id) => {
    const S = need(id);
    if (!S.exam) throw new Error('this is not an exam paper');
    return S;
  };
  const examNodes = [
    { id: 'derivative', title: 'Derivative as a limit', home: 'calc', homeTitle: 'Calculus', planNode: 'derivative', weight: 0.8, weightSource: 'plan', retrievability: 0.5, layerGap: 3, mode: 'teach',
      reason: 'an open misconception: differentiates the outer function only', evidence: true, practiceOnly: false, structuralMisses: 2, misconception: 'differentiates the outer function only' },
    { id: 'quadratic', title: 'Quadratic formula', home: 'alg', homeTitle: 'Algebra', planNode: 'quadratic', weight: 0.3, weightSource: 'default', retrievability: 0, layerGap: 3, mode: 'practice',
      reason: 'no answers yet: the first problems show what is there', evidence: false, practiceOnly: false, structuralMisses: 0 },
    { id: 'chain', title: 'Chain rule', home: 'calc', homeTitle: 'Calculus', planNode: 'chain', weight: 0.9, weightSource: 'plan', retrievability: 0.62, layerGap: 2, mode: 'practice',
      reason: 'recall has faded (R 0.62): a fresh problem brings it back', evidence: true, practiceOnly: true, structuralMisses: 0 },
    { id: 'limit', title: 'Limit of a function', home: 'calc', homeTitle: 'Calculus', planNode: 'limit', weight: 0.6, weightSource: 'goal', retrievability: 0.97, layerGap: 1, mode: 'practice',
      reason: 'held: keep it fresh', evidence: true, practiceOnly: false, structuralMisses: 0 },
  ].map((n) => ({ ...n, priority: n.weight * (1 - n.retrievability) * (n.layerGap + 1) })).sort((a, b) => b.priority - a.priority);
  const examGoal = () => ({
    goal: 'Pass MATH101', purpose: 'exam', successLooksLike: ['pass the final'],
    exam: { name: 'MATH101 final', date: new Date(Date.now() + 10 * 86400000).toISOString().slice(0, 10), syllabus: 'Limits, derivatives, the chain rule' },
  });
  const examHandlers = {
    session_state: (a) => ({ ...learnHandlers.session_state(a), goal: examGoal() }),
    exam_plan: ({ sessionId }) => {
      if (sessionId !== 'L1') throw new Error('the exam plan belongs to the Learn session of an exam goal');
      const g = examGoal();
      return {
        sessionId, exam: { name: g.exam.name, date: g.exam.date, syllabus: g.exam.syllabus }, now: stamp(), daysLeft: 10,
        nodes: structuredClone(examNodes), unmapped: [{ planNode: 'ghost', title: 'Series' }],
        today: { kind: 'teach', nodeIds: ['derivative'], reason: 'Derivative as a limit ranks near the top and needs teaching: an open misconception: differentiates the outer function only' },
        mix: { teachShare: 0.34, teachNodes: 1, practiceNodes: 3, sittings: 10, teachSittings: 3, practiceSittings: 7, papers: 2 },
        simulation: {
          label: 'estimate', note: 'An estimate, not a prediction. It reads each idea’s recall probability from FSRS, whose parameters were fit on flashcards, not exam answers [inference]; it counts an idea with no answers as 0 and an exam answer needs more than recall.',
          horizonDays: 10, expectedNow: 0.46, expectedExam: 0.33, unseen: 1,
          topics: [
            { home: 'calc', title: 'Calculus', nodes: 3, weight: 2.3, expectedNow: 0.6, expectedExam: 0.43 },
            { home: 'alg', title: 'Algebra', nodes: 1, weight: 0.3, expectedNow: 0, expectedExam: 0 },
          ],
          raise: [
            { id: 'derivative', title: 'Derivative as a limit', mode: 'teach', gain: 0.071 },
            { id: 'chain', title: 'Chain rule', mode: 'practice', gain: 0.043 },
          ],
        },
        papers: [{ library: 'maths', path: 'maths/2023-paper1.pdf', title: '2023 paper 1', pages: 12, markScheme: 'maths/2023-paper1-mark-scheme.pdf' }],
        paper: { defaultBudgetMin: 45, minBudgetMin: 5, maxBudgetMin: 360, minutesPerProblem: 15, defaultProblems: 3, targets: 3 },
      };
    },
    start_exam: async ({ goalSessionId, budgetMin }) => {
      if (!Number.isInteger(budgetMin) || budgetMin < 5 || budgetMin > 360) throw new Error('a paper’s time budget is 5 to 360 minutes');
      const size = Math.max(2, Math.min(10, Math.ceil(budgetMin / 15)));
      const S = newPractice({ id: `X${practices.size + 1}`, format: 'exam', size, title: 'Exam paper: MATH101 final' });
      S.exam = { goalSessionId, name: 'MATH101 final', budgetSec: budgetMin * 60, openedAt: null, deadlineAt: null, draft: [] };
      emit({ kind: 'session', session: summaryOf(S) });
      const planned = Array.from({ length: size }, () => addProblem(S));
      planned.forEach((p, i) => (p.marks = MARKS[i % MARKS.length]));
      await turn(S, async () => {
        await pause(300);
        for (const p of planned) pose(S, p);
        // The clock starts when the last problem is posed.
        S.exam.openedAt = Date.now();
        S.exam.deadlineAt = S.exam.openedAt + (examShortMs || S.exam.budgetSec * 1000);
        emitOp(S, { op: 'practice', event: 'examOpen', openedAt: new Date(S.exam.openedAt).toISOString(), deadline: new Date(S.exam.deadlineAt).toISOString() });
        setTimeout(() => autoSubmit(S), S.exam.deadlineAt + GRACE_MS - Date.now());
      });
      return summaryOf(S);
    },
    practice_exam_save: ({ sessionId, answers }) => {
      const S = needExam(sessionId);
      if (S.submitted) throw new Error('this paper was already handed in');
      if (!S.exam.deadlineAt) throw new Error('the paper isn’t open yet');
      if (examView(S).closed) throw new Error('time is up: answers saved after the deadline are not accepted');
      S.exam.draft = structuredClone(answers);
      return null;
    },
    practice_submit_set: async (a) => {
      const S = need(a.sessionId);
      if (!S.exam) return practiceHandlers.practice_submit_set(a);
      ready(S);
      if (S.submitted) throw new Error('this set was already handed in');
      if (examView(S).closed) {
        await autoSubmit(S);
        throw new Error('time is up: the paper was handed in at the deadline with your last saved answers; answers sent after it are not accepted');
      }
      await examSubmit(S, a.answers, false);
      return null;
    },
    practice_hint: (a) => {
      if (need(a.sessionId).exam) throw new Error('an exam paper has no hints');
      return practiceHandlers.practice_hint(a);
    },
    practice_resume: async (a) => {
      const S = need(a.sessionId);
      if (S.exam && !S.submitted && examView(S).expired) return autoSubmit(S);
      return practiceHandlers.practice_resume(a);
    },
  };

  const handlers = {
    ...syncHandlers,
    ...appHandlers,
    ...(learnFixture ? learnHandlers : {}),
    ...(practiceFixture ? practiceHandlers : {}),
    ...(examFixture ? examHandlers : {}),
    plugins: () => structuredClone(plugins),
    // The core's rules (plugins/load.rs): a built-in with permissions is
    // only turned on by consent, and none can be removed.
    set_plugin_enabled: ({ id, enabled }) => {
      const p = plugins.find((x) => x.id === id);
      if (!p) throw new Error(`plugin ${id} was not found`);
      p.enabled = enabled;
    },
    remove_plugin: ({ id }) => {
      const p = plugins.find((x) => x.id === id);
      if (p?.builtin) throw new Error(`\`${id}\` is built in; turn it off instead of removing it`);
      plugins.splice(0, plugins.length, ...plugins.filter((x) => x.id !== id));
    },
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

  const deliverReading = () => {
    emit({ kind: 'turnStart', sessionId: 'L1', agent: 'tutor' });
    for (const op of readingOps) emit({ kind: 'op', sessionId: 'L1', op });
    emit({ kind: 'turnEnd', sessionId: 'L1', agent: 'tutor' });
  };
  window.__bridge = { log, forbidden, sync, secretStorage, finishLogin: (account) => finishLogin?.(account), deliverReading };
  window.__TAURI__ = { core: { invoke, Channel: class {} } };
  window.__TAURI_INTERNALS__ = { invoke };
  window.ipc = { postMessage: (message) => forbidden.push({ via: 'window.ipc', message: String(message) }) };
  window.webkit = {
    messageHandlers: { ipc: { postMessage: (message) => forbidden.push({ via: 'webkit', message: String(message) }) } },
  };
})();
