// Renders one case with the real UI modules (served from ui/) against the
// fake bridge in fake-tauri.js. run.js drives the page over the Chrome
// DevTools protocol and reads `window.__check`.
//
// ?case=ipc        the IPC escape probe view (plugins/fixtures/ipc-probe)
// ?case=placement  a finished turn with three artifacts, in the op order
//                  crates/harness/tests/plugins_e2e.rs asserts for the core:
//                  the reply places A1 mid-paragraph; A2 and A3 are unplaced.

import { artifactFigure } from '../artifacts.js';
import { artifactsEl } from '../content.js';
import { loadPlugins } from '../pluginstore.js';
import { assistantEl, userEl } from '../reply.js';
import { applyOp, createTimeline } from '../timeline.js';

const flow = document.getElementById('flow');
const check = (window.__check = { ready: false, probe: null, errors: [] });
window.addEventListener('error', (e) => check.errors.push(String(e.message)));
window.addEventListener('unhandledrejection', (e) => check.errors.push(String(e.reason)));

const at = '2026-11-06T10:00:00.000Z';
const svg = (label) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 240 40"><text x="8" y="24" fill="currentColor">${label}</text></svg>`;

/** The divider of plugins_e2e.rs (`common::divider`), with R2 adjustable. */
function divider(r2) {
  return {
    title: `Divider with R2 = ${r2} Ω`,
    analysis: { dc: true },
    components: [
      { id: 'V1', type: 'V', from: [0, 0], to: [0, 2], value: 12, label: 'Vin' },
      { id: 'R1', type: 'R', from: [0, 2], to: [2, 2], value: 3000, label: 'R1' },
      { id: 'R2', type: 'R', from: [2, 2], to: [2, 0], value: r2, label: 'R2', adjustable: { min: 500, max: 6000, step: 100 } },
      { id: 'W0', type: 'wire', from: [2, 0], to: [0, 0] },
      { id: 'GND', type: 'ground', from: [0, 0], to: [0, 0] },
      { id: 'P1', type: 'probe', from: [2, 2], to: [2, 0], label: 'Vout' },
    ],
    result: { probes: { Vout: (12 * r2) / (3000 + r2) }, warnings: [] },
  };
}

function artifact(n, pluginId, tool, data) {
  return {
    op: 'artifact', opId: `op-a${n}`, at, artifactId: `A${n}`, pluginId, pluginVersion: '0.1.0',
    packageHash: 'check', tool, input: {}, data, fallbackSvg: svg(`fallback A${n}`),
  };
}

function tool(callId, name, args, n) {
  return [
    { op: 'tool', opId: `op-${callId}`, at, callId, name, args },
    artifact(n, name.startsWith('circuit') ? 'circuit' : 'sketch', name, args),
    {
      op: 'tool', opId: `op-${callId}-r`, at, callId, name,
      result: { content: `artifact: A${n} — place it once with ![a short description](artifact:A${n}) next to the sentence that refers to it`, isError: false },
    },
  ];
}

async function placement() {
  const ops = [
    { op: 'message', opId: 'm1', at, role: 'user', content: 'Can you draw the divider?' },
    { op: 'message', opId: 'm2', at, role: 'assistant', content: 'Here is the circuit.' },
    ...tool('d1', 'circuit_draw', divider(1000), 1),
    ...tool('d2', 'circuit_draw', divider(3000), 2),
    ...tool('k1', 'sketch_draw', {}, 3),
    {
      op: 'message', opId: 'm3', at, role: 'assistant',
      content: 'With R2 = 1 kΩ the divider ![Divider with R2 = 1 kΩ](artifact:A1) gives 3 V at Vout, a quarter of the supply.',
    },
  ];
  const tl = createTimeline();
  for (const op of ops) applyOp(tl, op);
  const ctx = { sessionId: 'S1', artifacts: tl.artifacts };
  for (const item of tl.items) {
    if (item.kind === 'user') flow.append(userEl(item.msg.content));
    else if (item.kind === 'assistant') flow.append(assistantEl(item.msg, { artifacts: item.artifacts ?? [], ctx }));
    else if (item.kind === 'artifacts') flow.append(artifactsEl(item.ops, ctx));
  }
}

function probe() {
  const op = { ...artifact(0, 'ipc-probe', 'ipc_probe_show', {}), artifactId: 'A-PROBE' };
  flow.append(artifactFigure(op, 'IPC probe', { sessionId: 'S1' }));
  window.addEventListener('message', (event) => {
    const frame = flow.querySelector('iframe');
    if (!frame || event.source !== frame.contentWindow) return;
    if (event.data?.type === 'probe') check.probe = event.data.results;
  });
}

await loadPlugins();
const name = new URLSearchParams(location.search).get('case');
if (name === 'placement') await placement();
else if (name === 'ipc') probe();
else check.errors.push(`unknown case ${name}`);
check.ready = true;
