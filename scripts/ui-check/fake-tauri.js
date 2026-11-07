// Stand-in for the Tauri bridge, loaded before the UI modules by host.html.
// It answers only the commands the artifact figures and view host use, and
// logs every call, so run.js can prove that nothing inside a view frame
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

  const handlers = {
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
    log.push({ cmd, args: structuredClone(args) });
    const handler = handlers[cmd];
    if (!handler) return Promise.reject(`unexpected command ${cmd}`);
    try {
      return Promise.resolve(handler(args));
    } catch (err) {
      return Promise.reject(String(err.message ?? err));
    }
  }

  window.__bridge = { log, forbidden };
  window.__TAURI__ = { core: { invoke, Channel: class {} } };
  window.__TAURI_INTERNALS__ = { invoke };
  window.ipc = { postMessage: (message) => forbidden.push({ via: 'window.ipc', message: String(message) }) };
  window.webkit = {
    messageHandlers: { ipc: { postMessage: (message) => forbidden.push({ via: 'webkit', message: String(message) }) } },
  };
})();
