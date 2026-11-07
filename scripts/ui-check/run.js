// Headless UI checks for plugin views, run with scripts/ui-check/run.sh.
//
// Serves ui/ (with the app CSP from crates/app/tauri.conf.json) as
// http://tauri.localhost, the plugin views (with VIEW_CSP from
// crates/app/src/main.rs) as http://pluginview.localhost/<id>, and records
// every request, including any to http://ipc.localhost. Chromium resolves
// all three names to this server (--host-resolver-rules), so the origins,
// the app CSP's frame-src and the view CSP are the ones Tauri uses on
// Windows/Android. Then, over the DevTools protocol:
//
//   ipc        the probe view (plugins/fixtures/ipc-probe) tries every way
//              out; every attempt must be blocked, no request may reach the
//              app or IPC origins, and the fake bridge must see only the
//              view host's own commands.
//   placement  a turn whose reply places A1 mid-paragraph: A1 sits in the
//              sentence, A2 and A3 follow the message in creation order, and
//              keyboard moves of A1's R2 slider re-solve through the bridge,
//              update the probe and post a `state` line.
//
// Exit code 0 when every check passes. Usage: run.js [ipc|placement]...

import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, normalize, extname } from 'node:path';

const ROOT = normalize(join(import.meta.dir, '../..'));
const read = (p) => readFileSync(join(ROOT, p), 'utf8');

const APP_CSP = JSON.parse(read('crates/app/tauri.conf.json')).app.security.csp;
const VIEW_CSP = read('crates/app/src/main.rs').match(/const VIEW_CSP: &str = "([^"]+)";/)?.[1];
if (!APP_CSP || !VIEW_CSP) throw new Error('could not read the app CSP or VIEW_CSP');

const PLUGIN_DIRS = { circuit: 'plugins/circuit', 'ipc-probe': 'plugins/fixtures/ipc-probe' };
function viewHtml(id) {
  const dir = PLUGIN_DIRS[id];
  if (!dir) return null;
  const manifest = Bun.TOML.parse(read(`${dir}/plugin.toml`));
  const html = read(`${dir}/${manifest.view[0].html}`);
  // The core inlines package assets; these fixtures have none to inline.
  if (/<script[^>]+\bsrc=|<link[^>]+\bhref=/i.test(html)) throw new Error(`${id}: view references assets; inline them for this check`);
  return html;
}

const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.json': 'application/json' };
const hits = [];

function file(base, path) {
  const full = normalize(join(ROOT, base, decodeURIComponent(path)));
  if (!full.startsWith(join(ROOT, base)) || !existsSync(full)) return null;
  return new Response(Bun.file(full), { headers: { 'content-type': TYPES[extname(full)] ?? 'application/octet-stream', 'content-security-policy': APP_CSP } });
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  fetch(req) {
    const url = new URL(req.url);
    const host = (req.headers.get('host') ?? '').replace(/:\d+$/, '');
    const leak = host === 'ipc.localhost' || url.pathname.startsWith('/leak/') || !['tauri.localhost', 'pluginview.localhost'].includes(host);
    hits.push({ host, path: url.pathname, method: req.method, leak, dest: req.headers.get('sec-fetch-dest') });
    if (leak) return new Response('reached', { headers: { 'access-control-allow-origin': '*' } });
    if (host === 'pluginview.localhost') {
      const html = viewHtml(url.pathname.slice(1));
      return html
        ? new Response(html, { headers: { 'content-type': 'text/html', 'content-security-policy': VIEW_CSP } })
        : new Response('no view', { status: 404 });
    }
    const res = url.pathname.startsWith('/check/') ? file('scripts/ui-check', url.pathname.slice(7)) : file('ui', url.pathname.slice(1));
    return res ?? new Response('not found', { status: 404 });
  },
});

// --- Chromium over the DevTools protocol ----------------------------------

const profile = mkdtempSync(join(tmpdir(), 'ui-check-'));
const rules = ['tauri.localhost', 'pluginview.localhost', 'ipc.localhost'].map((h) => `MAP ${h} 127.0.0.1:${server.port}`).join(',');
const chrome = Bun.spawn(
  [process.env.CHROMIUM ?? 'chromium', '--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--no-first-run',
    '--no-default-browser-check', '--disable-gpu', `--host-resolver-rules=${rules}`, 'about:blank'],
  { stderr: 'pipe', stdout: 'ignore' },
);

async function devtoolsUrl() {
  const reader = chrome.stderr.getReader();
  let text = '';
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    text += new TextDecoder().decode(value);
    const m = text.match(/DevTools listening on (ws:\/\/\S+)/);
    if (m) {
      reader.releaseLock();
      return m[1];
    }
  }
  throw new Error(`Chromium did not start:\n${text}`);
}

const ws = new WebSocket(await devtoolsUrl());
await new Promise((ok, no) => { ws.onopen = ok; ws.onerror = no; });
let seq = 0;
const pending = new Map();
const listeners = new Set();
ws.onmessage = (event) => {
  const m = JSON.parse(event.data);
  if (m.id && pending.has(m.id)) {
    const { ok, no } = pending.get(m.id);
    pending.delete(m.id);
    m.error ? no(new Error(`${m.error.message} ${m.error.data ?? ''}`)) : ok(m.result);
  } else {
    for (const fn of listeners) fn(m);
  }
};
const send = (method, params = {}, sessionId) =>
  new Promise((ok, no) => {
    const id = ++seq;
    pending.set(id, { ok, no });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(what, fn, ms = 15_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(50);
  }
}

const pages = [];
const frames = new Map(); // targetId → sessionId of attached iframe targets
listeners.add((m) => {
  if (m.method === 'Target.targetCreated' && m.params.targetInfo.type === 'page') pages.push(m.params.targetInfo);
  if (m.method === 'Target.attachedToTarget' && m.params.targetInfo.type === 'iframe') {
    frames.set(m.params.targetInfo.targetId, m.params.sessionId);
    send('Runtime.runIfWaitingForDebugger', {}, m.params.sessionId).catch(() => {});
  }
});
await send('Target.setDiscoverTargets', { discover: true });

async function open(caseName) {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sessionId);
  await send('Page.enable', {}, sessionId);
  await send('Runtime.enable', {}, sessionId);
  const url = `http://tauri.localhost/check/host.html?case=${caseName}`;
  await send('Page.navigate', { url }, sessionId);
  const evaluate = async (expression, session = sessionId, contextId) => {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, ...(contextId ? { contextId } : {}) }, session);
    if (r.exceptionDetails) throw new Error(`${expression}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
    return r.result.value;
  };
  await until('the page', () => evaluate('window.__check?.ready === true').catch(() => false));
  return { targetId, sessionId, url, evaluate };
}

/** An evaluator inside the document of the iframe matched by `selector`. */
async function inFrame(page, selector) {
  const { root } = await send('DOM.getDocument', { depth: 0 }, page.sessionId);
  const { nodeId } = await send('DOM.querySelector', { nodeId: root.nodeId, selector }, page.sessionId);
  if (!nodeId) throw new Error(`no ${selector}`);
  const { node } = await send('DOM.describeNode', { nodeId }, page.sessionId);
  const frameId = node.frameId;
  // An out-of-process frame is its own target (targetId = frameId);
  // otherwise the frame lives in the page's process.
  const session = await until(`the frame target of ${selector}`, () => frames.get(frameId), 2_000).catch(() => page.sessionId);
  await send('Page.enable', {}, session).catch(() => {});
  const { executionContextId } = await send('Page.createIsolatedWorld', { frameId, worldName: 'ui-check' }, session);
  return (expression) => page.evaluate(expression, session, executionContextId);
}

const failures = [];
const expect = (ok, message) => {
  if (!ok) failures.push(message);
  return ok;
};

// --- ipc ---------------------------------------------------------------------

async function ipc() {
  const before = hits.length;
  const page = await open('ipc');
  const results = await until('the probe report', () => page.evaluate('window.__check.probe'), 30_000);
  await sleep(1_500); // late beacons, the frame's self-navigation, popups
  const bridge = await page.evaluate('window.__bridge');
  const href = await page.evaluate('location.href');
  const seen = hits.slice(before);

  // Control: the app page itself may reach the IPC origin (the app CSP allows
  // it), so a leak from the frame would have been recorded too.
  const control = await page.evaluate(`fetch('http://ipc.localhost/control', { method: 'POST' }).then((r) => r.status, (e) => String(e))`);
  expect(control === 200 && hits.at(-1)?.path === '/control', `control request from the app page did not reach the server (${control})`);

  const width = Math.max(...results.map((r) => r.name.length));
  for (const r of results) console.log(`  ${r.outcome.padEnd(8)} ${r.name.padEnd(width)}  ${r.detail}`);
  const escaped = results.filter((r) => r.outcome === 'escaped');
  expect(escaped.length === 0, `escaped: ${escaped.map((r) => r.name).join(', ')}`);
  const leaks = seen.filter((h) => h.leak);
  expect(leaks.length === 0, `requests reached the server from the frame: ${JSON.stringify(leaks)}`);
  expect(seen.some((h) => h.host === 'pluginview.localhost' && h.path === '/ipc-probe'), 'the probe was not served from the view origin');
  const allowed = new Set(['plugins', 'plugin_view', 'plugin_view_url', 'view_state']);
  const stray = bridge.log.filter((c) => !allowed.has(c.cmd));
  expect(stray.length === 0, `the bridge saw commands the view host never sends: ${JSON.stringify(stray)}`);
  expect(bridge.forbidden.length === 0, `platform IPC stand-ins were called: ${JSON.stringify(bridge.forbidden)}`);
  const state = bridge.log.find((c) => c.cmd === 'view_state');
  expect(/ipc-probe: \d+ attempts, 0 escaped/.test(state?.args?.summary ?? ''), `state line: ${state?.args?.summary}`);
  expect(href === page.url, `the app page navigated to ${href}`);
  const popups = pages.filter((p) => p.openerId === page.targetId);
  expect(popups.length === 0, `the frame opened windows: ${popups.map((p) => p.url).join(', ')}`);
  const sent = results.filter((r) => r.outcome === 'sent').length;
  console.log(`ipc: ${results.length} attempts, ${escaped.length} escaped, ${sent} fire-and-forget with no request reaching the server, ${leaks.length} leaked requests, ${stray.length + bridge.forbidden.length} bridge calls from the frame`);
}

// --- placement ---------------------------------------------------------------

async function placement() {
  const page = await open('placement');
  await send('DOM.enable', {}, page.sessionId);
  const layout = await page.evaluate(`(() => {
    const entry = [...document.querySelectorAll('.entry.tutor')].pop();
    const fig = entry.querySelector('figure[data-artifact-id="A1"]');
    let before = '', after = '', seen = false;
    for (const n of fig?.parentElement?.childNodes ?? []) {
      if (n === fig) { seen = true; continue; }
      if (seen) after += n.textContent; else before += n.textContent;
    }
    return {
      block: fig?.parentElement?.tagName,
      inProse: Boolean(fig && entry.querySelector('.prose').contains(fig)),
      before: before.trim(), after: after.trim(),
      appended: [...entry.children].filter((c) => c.matches('figure')).map((f) => f.dataset.artifactId),
      all: [...document.querySelectorAll('figure[data-artifact-id]')].map((f) => f.dataset.artifactId),
      fallbackA3: Boolean(document.querySelector('figure[data-artifact-id="A3"] svg')),
    };
  })()`);
  console.log(`  layout ${JSON.stringify(layout)}`);
  expect(layout.block === 'P' && layout.inProse, 'A1 is not inside the reply paragraph');
  expect(layout.before.endsWith('the divider') && layout.after.startsWith('gives 3 V'), 'A1 is not mid-sentence');
  expect(JSON.stringify(layout.appended) === '["A2","A3"]', 'A2, A3 are not appended after the message in order');
  expect(JSON.stringify(layout.all) === '["A1","A2","A3"]', 'each artifact must render exactly once');
  expect(layout.fallbackA3, 'A3 (plugin not installed) should show its fallback SVG');

  await until('A1 view', () => page.evaluate(`Boolean(document.querySelector('figure[data-artifact-id="A1"].interactive iframe'))`));
  const frame = await inFrame(page, 'figure[data-artifact-id="A1"] iframe');
  await until('the slider', () => frame(`Boolean(document.getElementById('slider-R2'))`).catch(() => false));
  const start = Number(await frame(`document.getElementById('slider-R2').value`));
  await page.evaluate('window.__bridge.log.length = 0');

  // Keyboard only: Tab into the frame until the slider has focus, then
  // ArrowRight five times (step 100).
  const key = async (k, code, keyCode) => {
    await send('Input.dispatchKeyEvent', { type: 'keyDown', key: k, code, windowsVirtualKeyCode: keyCode }, page.sessionId);
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, code, windowsVirtualKeyCode: keyCode }, page.sessionId);
  };
  for (let i = 0; i < 12 && (await frame(`document.activeElement?.id`)) !== 'slider-R2'; i++) await key('Tab', 'Tab', 9);
  expect((await frame(`document.activeElement?.id`)) === 'slider-R2', 'Tab never reached the R2 slider');
  for (let i = 0; i < 5; i++) {
    await key('ArrowRight', 'ArrowRight', 39);
    await sleep(60);
  }
  const value = Number(await frame(`document.getElementById('slider-R2').value`));
  const vout = (12 * value) / (3000 + value);
  // The view host sends at most one state line per 250 ms; the last one must
  // describe the final position.
  await until('a state line', () => page.evaluate(`window.__bridge.log.some((c) => c.cmd === 'view_state')`), 5_000).catch(() => null);
  await sleep(400);
  const state = await page.evaluate(`window.__bridge.log.filter((c) => c.cmd === 'view_state').at(-1)?.args`);
  const solves = await page.evaluate(`window.__bridge.log.filter((c) => c.cmd === 'plugin_view_call' && c.args.function === 'solve').map((c) => c.args.args.components.find((p) => p.id === 'R2').value)`);
  const shown = await frame(`[...document.querySelectorAll('#probes tbody tr')].map((tr) => tr.textContent.trim())`);
  const row = shown.find((t) => t.startsWith('Vout')) ?? '';
  console.log(`  slider R2 ${start} → ${value}; solves for R2 = ${solves.join(', ')}; probe row "${row}"; state "${state?.summary}"`);
  expect(value === start + 500, `ArrowRight ×5 moved R2 from ${start} to ${value}`);
  expect(solves.at(-1) === value, 'the last bridge solve did not use the new R2');
  expect(Math.abs(parseFloat(row.replace(/^Vout\s*/, '')) - vout) < 0.01 * vout, `probe row "${row}" ≠ Vout ${vout}`);
  const stated = parseFloat((state?.summary ?? '').match(/Vout = ([\d.]+)/)?.[1]);
  expect(state?.artifactId === 'A1' && Math.abs(stated - vout) < 0.01 * vout, `last state line for A1 "${state?.summary}" ≠ Vout ${vout}`);
  console.log(`placement: A1 in the sentence, A2 and A3 appended in order; slider R2 → Vout ${vout.toFixed(3)} V through the bridge`);
}

const cases = { ipc, placement };
const wanted = process.argv.slice(2).length ? process.argv.slice(2) : Object.keys(cases);
try {
  for (const name of wanted) {
    if (!cases[name]) throw new Error(`unknown case ${name}`);
    console.log(`== ${name}`);
    await cases[name]();
  }
} catch (err) {
  failures.push(String(err.stack ?? err));
} finally {
  ws.close();
  chrome.kill();
  await chrome.exited;
  server.stop(true);
  rmSync(profile, { recursive: true, force: true });
}
if (failures.length) {
  console.log(`FAILED:\n- ${failures.join('\n- ')}`);
  process.exit(1);
}
console.log('all checks passed');
