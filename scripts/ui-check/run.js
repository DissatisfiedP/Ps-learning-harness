// Headless UI checks for plugin views, run with scripts/ui-check/run.sh.
//
// Serves ui/ (with the app CSP from crates/app/tauri.conf.json) as
// http://tauri.localhost, the plugin views (with VIEW_CSP from
// crates/app/src/lib.rs) as http://pluginview.localhost/<id>, and records
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
//   sync       the whole app (index.html with the fake bridge) in each sync
//              state: the Home sync icon (states, names, a one-line header
//              at 1440, 390 and 320 px) and its panel (placement, closing,
//              Sync now, Sync settings), the banner, the collapsible
//              Providers and Sync sections (closed by default, summaries,
//              remembered state, opened by navigation, a WorkBuddy sign-in
//              surviving collapse), Settings → Sync (save git and WebDAV,
//              Sync now, acknowledge, disconnect), the other Settings sections
//              still in place, and phone widths with notch insets.
//              UI_CHECK_SHOTS=<dir> also saves screenshots there.
//
// Exit code 0 when every check passes. Usage: run.js [ipc|placement|sync]...

import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, normalize, extname } from 'node:path';

const ROOT = normalize(join(import.meta.dir, '../..'));
const read = (p) => readFileSync(join(ROOT, p), 'utf8');

const APP_CSP = JSON.parse(read('crates/app/tauri.conf.json')).app.security.csp;
const VIEW_CSP = read('crates/app/src/lib.rs').match(/const VIEW_CSP: &str = "([^"]+)";/)?.[1];
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
    // The app page with the fake bridge loaded before any module.
    if (url.pathname === '/index.html' && url.searchParams.has('check')) {
      const html = read('ui/index.html').replace('<script src="./theme.js">', '<script src="/check/fake-tauri.js"></script>\n  <script src="./theme.js">');
      return new Response(html, { headers: { 'content-type': 'text/html', 'content-security-policy': APP_CSP } });
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

// --- sync --------------------------------------------------------------------

const SHOTS = process.env.UI_CHECK_SHOTS;

/** The app page in one sync state (`?sync=`), at a viewport and colour scheme.
 *  `insets` emulates an edge-to-edge phone's status bar/notch and gesture bar
 *  (`env(safe-area-inset-*)`), e.g. `{ top: 47, bottom: 34 }`; `mobile` also
 *  turns on touch, so `(pointer: coarse)` matches as on a phone. */
async function openApp(scenario, { width = 1440, height = 900, mobile = false, dark = false, insets = null } = {}) {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('Page.enable', {}, sessionId);
  await send('Runtime.enable', {}, sessionId);
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile }, sessionId);
  if (mobile) await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 }, sessionId);
  if (insets) await send('Emulation.setSafeAreaInsetsOverride', { insets }, sessionId);
  await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: dark ? 'dark' : 'light' }] }, sessionId);
  await send('Page.addScriptToEvaluateOnNewDocument', {
    source: `window.__errors = [];
      addEventListener('error', (e) => __errors.push(String(e.message)));
      addEventListener('unhandledrejection', (e) => __errors.push(String(e.reason?.message ?? e.reason)));`,
  }, sessionId);
  await send('Page.navigate', { url: `http://tauri.localhost/index.html?check&sync=${scenario}` }, sessionId);
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, sessionId);
    if (r.exceptionDetails) throw new Error(`${expression}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
    return r.result.value;
  };
  await until(`the app (${scenario})`, () => evaluate(`document.readyState === 'complete' && Boolean(window.__bridge?.log.some((c) => c.cmd === 'sync_status'))`).catch(() => false));
  await sleep(150);
  const shot = async (name) => {
    if (!SHOTS) return;
    await sleep(400); // let the Settings sheet finish fading in
    const { data } = await send('Page.captureScreenshot', { format: 'png' }, sessionId);
    await Bun.write(join(SHOTS, `${name}.png`), Buffer.from(data, 'base64'));
  };
  const close = () => send('Target.closeTarget', { targetId });
  return { evaluate, shot, close };
}

const HOME = `(() => {
  const $ = (id) => document.getElementById(id);
  const shown = (id) => !$(id).hidden && $(id).getClientRects().length > 0;
  const icon = $('sync-indicator');
  return {
    icon: shown('sync-indicator'),
    name: icon.getAttribute('aria-label'),
    title: icon.title,
    dot: shown('sync-dot'),
    busy: icon.getAttribute('aria-busy') === 'true',
    expanded: icon.getAttribute('aria-expanded'),
    sameColour: getComputedStyle(icon).color === getComputedStyle($('open-map')).color,
    iconSize: [icon.getBoundingClientRect().width, icon.getBoundingClientRect().height].map(Math.round),
    coarse: matchMedia('(pointer: coarse)').matches,
    panel: shown('sync-panel'),
    banner: shown('sync-banner'),
    error: shown('sync-banner-error') ? $('sync-banner-error-text').textContent : null,
    attention: shown('sync-banner-attention') ? $('sync-banner-attention-text').textContent : null,
    barHeight: document.querySelector('.home-bar').getBoundingClientRect().height,
    headH: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--head-h')),
    overflow: document.documentElement.scrollWidth > innerWidth || $('home').scrollWidth > $('home').clientWidth
      || $('open-settings').getBoundingClientRect().right > innerWidth || icon.getBoundingClientRect().right > innerWidth,
  };
})()`;
const PANEL = `(() => {
  const $ = (id) => document.getElementById(id);
  const icon = $('sync-indicator').getBoundingClientRect();
  const p = $('sync-panel').getBoundingClientRect();
  return {
    open: !$('sync-panel').hidden,
    lines: [...$('sync-panel-targets').children].map((li) => [...li.children].map((c) => c.textContent)),
    attention: $('sync-panel-attention').hidden ? null : $('sync-panel-attention').textContent,
    now: $('sync-panel-now').textContent, nowDisabled: $('sync-panel-now').disabled,
    status: $('sync-panel-status').textContent,
    rightAligned: Math.abs(p.right - icon.right) < 1,
    below: p.top >= icon.bottom && p.top - icon.bottom <= 8,
    inside: p.left >= 0 && p.right <= innerWidth && p.bottom <= innerHeight,
    box: [p.left, p.top, p.right, p.bottom].map(Math.round),
    focus: document.activeElement?.id ?? null,
  };
})()`;
/** Escape as a key press on whatever has focus (the panel listens on its wrapper). */
const escape = `document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`;
const oneLine = (h) => h.barHeight <= h.headH + 0.5;
const SECTION = `(() => {
  const $ = (id) => document.getElementById(id);
  return {
    git: $('sync-git-state').textContent, gitError: $('sync-git-error').hidden ? null : $('sync-git-error').textContent,
    gitDisconnect: !$('sync-git-disconnect').hidden, tokenPlaceholder: $('sync-git-token').placeholder,
    webdav: $('sync-webdav-state').textContent, webdavStatus: $('sync-webdav-status').textContent,
    webdavDisconnect: !$('sync-webdav-disconnect').hidden, passwordPlaceholder: $('sync-webdav-password').placeholder,
    syncNow: $('sync-now').textContent, syncNowDisabled: $('sync-now').disabled, status: $('sync-status').textContent,
    paths: $('sync-attention').hidden ? [] : [...$('sync-paths').children].map((li) => li.textContent),
    focus: document.activeElement?.id ?? null,
  };
})()`;
const SECTIONS = `Object.fromEntries(['providers', 'sync'].map((n) => [n, {
  open: document.getElementById(n + '-toggle').getAttribute('aria-expanded') === 'true',
  shown: !document.getElementById(n + '-body').hidden,
  summary: document.getElementById(n + '-summary').textContent,
  stored: localStorage.getItem('lh.settings.' + n),
}]))`;
const lastCall = (cmd) => `(window.__bridge.log.filter((c) => c.cmd === '${cmd}').at(-1)?.args ?? null)`;
const fill = (id, value) => `(() => { const el = document.getElementById('${id}'); el.value = ${JSON.stringify(value)}; el.dispatchEvent(new Event('input')); })()`;
const click = (id) => `(document.getElementById('${id}').focus(), document.getElementById('${id}').click())`;
const submit = (id) => `document.getElementById('${id}').requestSubmit()`;
const workbuddy = `document.querySelector('#provider-list .provider[data-id="workbuddy"]')`;
/** Closes Settings and resolves once its `close` event has run (the event is a
 *  queued task; a late one would cancel a sign-in started in the next opening). */
const closeSettings = `(() => {
  const d = document.getElementById('settings');
  if (!d.open) return true;
  return new Promise((done) => { d.addEventListener('close', () => done(true), { once: true }); d.close(); });
})()`;
/** Collapses Providers and Sync (as the learner would) and scrolls them into view. */
const collapseBoth = `(() => {
  for (const n of ['providers', 'sync']) {
    const t = document.getElementById(n + '-toggle');
    if (t.getAttribute('aria-expanded') === 'true') t.click();
  }
  document.getElementById('providers-toggle').scrollIntoView({ block: 'center' });
})()`;

async function sync() {
  // Nothing configured: no Home parts; Settings → Sync sits after Providers.
  let page = await openApp('none');
  let home = await page.evaluate(HOME);
  expect(!home.icon && !home.banner, `unconfigured Home shows sync parts: ${JSON.stringify(home)}`);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  const order = await page.evaluate(`[...document.querySelectorAll('#settings .settings-section > h2')].map((h) => (h.querySelector('.section-name') ?? h).textContent)`);
  expect(JSON.stringify(order) === JSON.stringify(['Model roles', 'Learning', 'Focus', 'Providers', 'Sync', 'Plugins', 'Registries', 'Appearance']), `Settings sections: ${order}`);
  const others = await page.evaluate(`({ providers: document.querySelectorAll('#provider-list .provider').length, plugins: document.querySelectorAll('#plugin-list .plugin').length, roles: document.getElementById('model-form').textContent.trim() })`);
  expect(others.providers === 3 && others.plugins === 2 && others.roles.length > 0, `other Settings sections did not render: ${JSON.stringify(others)}`);

  // Providers and Sync start collapsed, each heading carrying its summary.
  let sections = await page.evaluate(SECTIONS);
  expect(!sections.providers.open && !sections.providers.shown && sections.providers.summary === '1 connected'
    && !sections.sync.open && !sections.sync.shown && sections.sync.summary === 'Off', `default sections: ${JSON.stringify(sections)}`);
  await page.evaluate(`document.getElementById('providers-toggle').scrollIntoView({ block: 'center' })`);
  await page.shot('settings-collapsed-desktop');
  await page.evaluate(click('sync-toggle'));
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.open && sections.sync.shown && sections.sync.stored === 'open' && !sections.providers.open, `Sync heading did not expand it: ${JSON.stringify(sections)}`);
  let section = await page.evaluate(SECTION);
  expect(section.git === 'Not connected' && section.webdav === 'Not connected' && section.syncNowDisabled, `unconfigured section: ${JSON.stringify(section)}`);

  // Save git, then WebDAV (a plain-http URL is refused with the backend's sentence).
  await page.evaluate(fill('sync-git-remote', 'https://github.com/learner/notes.git'));
  await page.evaluate(fill('sync-git-token', 'ghp-secret'));
  await page.evaluate(submit('sync-git'));
  await until('git saved', () => page.evaluate(`document.getElementById('sync-git-status').textContent === 'Saved.'`));
  const gitArgs = await page.evaluate(lastCall('set_sync'));
  expect(JSON.stringify(gitArgs) === JSON.stringify({ remote: 'https://github.com/learner/notes.git', token: 'ghp-secret', branch: 'main' }), `set_sync args: ${JSON.stringify(gitArgs)}`);
  section = await page.evaluate(SECTION);
  expect(section.git === 'Not synced yet · changes waiting' && section.gitDisconnect && /Saved/.test(section.tokenPlaceholder) && !section.syncNowDisabled, `after git save: ${JSON.stringify(section)}`);
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.summary === 'Not synced', `Sync summary after git save: ${sections.sync.summary}`);
  await page.evaluate(fill('sync-webdav-url', 'http://dav.example.com/harness'));
  await page.evaluate(fill('sync-webdav-username', 'learner'));
  await page.evaluate(fill('sync-webdav-password', 'app-pass'));
  await page.evaluate(submit('sync-webdav'));
  await until('webdav refused', () => page.evaluate(`document.getElementById('sync-webdav-status').classList.contains('error')`));
  section = await page.evaluate(SECTION);
  expect(/https/.test(section.webdavStatus) && section.webdav === 'Not connected', `http WebDAV URL: ${JSON.stringify(section)}`);
  await page.evaluate(fill('sync-webdav-url', 'https://dav.example.com/harness'));
  await page.evaluate(submit('sync-webdav'));
  await until('webdav saved', () => page.evaluate(`document.getElementById('sync-webdav-status').textContent === 'Saved.'`));
  const davArgs = await page.evaluate(lastCall('set_webdav'));
  expect(JSON.stringify(davArgs) === JSON.stringify({ url: 'https://dav.example.com/harness', username: 'learner', password: 'app-pass' }), `set_webdav args: ${JSON.stringify(davArgs)}`);
  section = await page.evaluate(SECTION);
  expect(section.webdav === 'Not synced yet · changes waiting' && /Saved/.test(section.passwordPlaceholder), `after WebDAV save: ${JSON.stringify(section)}`);

  // Sync now: "Syncing…" while it runs, then the report and fresh state lines.
  await page.evaluate(click('sync-now'));
  section = await page.evaluate(SECTION);
  sections = await page.evaluate(SECTIONS);
  expect(section.syncNow === 'Syncing…' && section.syncNowDisabled && sections.sync.summary === 'Syncing…', `while syncing: ${JSON.stringify({ section, summary: sections.sync.summary })}`);
  home = await page.evaluate(HOME);
  expect(home.name === 'Sync: Syncing…' && home.busy && !home.dot, `Home icon while syncing: ${JSON.stringify(home)}`);
  await until('the sync report', () => page.evaluate(`document.getElementById('sync-status').textContent.startsWith('Synced')`));
  section = await page.evaluate(SECTION);
  expect(section.status === 'Synced. Git: 1 pulled, 1 pushed. WebDAV: 3 uploaded, 1 downloaded.' && section.git === 'Last synced just now' && section.webdav === 'Last synced just now' && section.focus === 'sync-now', `after Sync now: ${JSON.stringify(section)}`);
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.summary === 'Synced just now', `Sync summary after Sync now: ${sections.sync.summary}`);
  await page.shot('settings-sync-desktop');

  // A saved secret is kept when its field stays empty.
  await page.evaluate(fill('sync-git-branch', 'notes'));
  await page.evaluate(submit('sync-git'));
  await until('branch saved', () => page.evaluate(`${lastCall('set_sync')}?.branch === 'notes'`));
  const kept = await page.evaluate(lastCall('set_sync'));
  expect(kept.token === null && (await page.evaluate(`window.__bridge.sync.token`)) === 'ghp-secret', `empty token field did not keep the saved token: ${JSON.stringify(kept)}`);

  // Reopening keeps what the learner chose: Sync open, Providers closed; then
  // the learner closes Sync, and that is kept too.
  await page.evaluate(closeSettings);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.open && sections.sync.shown && !sections.providers.open, `reopened Settings forgot the open Sync: ${JSON.stringify(sections)}`);
  await page.evaluate(click('sync-toggle'));
  await page.evaluate(closeSettings);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  sections = await page.evaluate(SECTIONS);
  expect(!sections.sync.open && !sections.sync.shown && sections.sync.stored === 'closed' && sections.sync.summary === 'Synced just now', `reopened Settings forgot the closed Sync: ${JSON.stringify(sections)}`);

  // Close Settings: the Home icon says "Synced just now", in the links' colour, on one line.
  await page.evaluate(closeSettings);
  home = await page.evaluate(HOME);
  expect(home.icon && home.name === 'Sync: Synced just now' && /^Last synced /.test(home.title) && !home.dot && !home.busy
    && home.expanded === 'false' && home.sameColour && oneLine(home) && !home.banner, `Home after sync: ${JSON.stringify(home)}`);

  // The icon opens the panel under itself, right-aligned; Escape, the icon and
  // an outside click close it.
  await page.evaluate(click('sync-indicator'));
  await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
  let panel = await page.evaluate(PANEL);
  expect(JSON.stringify(panel.lines) === JSON.stringify([['Git · Last synced just now'], ['WebDAV · Last synced just now']])
    && panel.rightAligned && panel.below && panel.inside && panel.now === 'Sync now' && !panel.attention
    && (await page.evaluate(HOME)).expanded === 'true', `panel: ${JSON.stringify(panel)}`);
  await page.shot('panel-desktop');
  await page.evaluate(escape);
  panel = await page.evaluate(PANEL);
  expect(!panel.open && panel.focus === 'sync-indicator', `Escape: ${JSON.stringify(panel)}`);
  await page.evaluate(click('sync-indicator'));
  await page.evaluate(click('sync-indicator'));
  expect(!(await page.evaluate(PANEL)).open, 'the icon did not close its panel');
  await page.evaluate(click('sync-indicator'));
  await page.evaluate(`document.getElementById('hero').click()`);
  expect(!(await page.evaluate(PANEL)).open, 'an outside click did not close the panel');

  // "Sync settings" in the panel expands Sync and focuses its heading; WebDAV disconnects alone.
  await page.evaluate(click('sync-indicator'));
  await page.evaluate(click('sync-panel-settings'));
  await until('Settings at Sync', () => page.evaluate(`document.activeElement?.id === 'sync-toggle'`));
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.open && sections.sync.shown && !(await page.evaluate(PANEL)).open, `Sync settings left Sync collapsed: ${JSON.stringify(sections)}`);
  await page.evaluate(click('sync-webdav-disconnect'));
  await until('WebDAV disconnected', () => page.evaluate(`document.getElementById('sync-webdav-state').textContent === 'Not connected'`));
  const off = await page.evaluate(lastCall('set_webdav'));
  section = await page.evaluate(SECTION);
  expect(off.url === '' && section.git === 'Last synced just now' && section.focus === 'sync-webdav-url', `WebDAV disconnect: ${JSON.stringify({ off, section })}`);
  // Opening it to get there is not remembered: a plain reopen shows it closed.
  await page.evaluate(closeSettings);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  sections = await page.evaluate(SECTIONS);
  expect(!sections.sync.open && sections.sync.stored === 'closed', `navigation changed the remembered Sync state: ${JSON.stringify(sections)}`);
  await page.evaluate(closeSettings);

  // "Connect a model to start → Settings" expands Providers. A WorkBuddy
  // sign-in keeps waiting while Providers is collapsed and expanded, and its
  // result lands with the section closed.
  expect(await page.evaluate(`!document.getElementById('setup-notice').hidden`), 'the setup notice is not showing');
  await page.evaluate(click('setup-button'));
  await until('Settings at Providers', () => page.evaluate(`document.activeElement?.id === 'providers-toggle'`));
  sections = await page.evaluate(SECTIONS);
  expect(sections.providers.open && sections.providers.shown && sections.providers.stored === null, `setup notice did not expand Providers: ${JSON.stringify(sections)}`);
  await page.evaluate(`[...${workbuddy}.querySelectorAll('button')].find((b) => b.textContent === 'Sign in with WorkBuddy').click()`);
  await until('the WorkBuddy wait', () => page.evaluate(`${workbuddy}.textContent.includes('Waiting for browser sign-in…')`));
  await page.evaluate(click('providers-toggle'));
  await page.evaluate(click('providers-toggle'));
  const waiting = await page.evaluate(`({ shown: !document.getElementById('providers-body').hidden, text: ${workbuddy}.textContent.includes('Waiting for browser sign-in…') })`);
  expect(waiting.shown && waiting.text, `sign-in lost after collapse and expand: ${JSON.stringify(waiting)}`);
  await page.evaluate(click('providers-toggle'));
  await page.evaluate(`window.__bridge.finishLogin('learner@example.com')`);
  await until('the sign-in to land', () => page.evaluate(`document.getElementById('providers-summary').textContent === '2 connected'`));
  await page.evaluate(click('providers-toggle'));
  const signedIn = await page.evaluate(`({ status: ${workbuddy}.querySelector('.status').textContent, state: ${workbuddy}.querySelector('.provider-state').textContent, tutor: ${lastCall('set_role_model')}?.choice?.provider })`);
  expect(signedIn.status === 'Signed in as learner@example.com.' && signedIn.state === 'Signed in as learner@example.com' && signedIn.tutor === 'workbuddy', `WorkBuddy sign-in result: ${JSON.stringify(signedIn)}`);
  await page.evaluate(closeSettings);
  expect(await page.evaluate(`document.getElementById('setup-notice').hidden`), 'the setup notice still shows after signing in');

  // A failing sync_status hides the Home parts quietly.
  await page.evaluate(`window.__bridge.sync.statusFails = true; document.getElementById('home').hidden = true`);
  await page.evaluate(`document.getElementById('home').hidden = false`);
  await until('the icon to hide', () => page.evaluate(`document.getElementById('sync-wrap').hidden`), 3_000).catch(() => null);
  home = await page.evaluate(HOME);
  expect(!home.icon && !home.banner, `failing sync_status still shows: ${JSON.stringify(home)}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // A recorded error: a --danger dot and the banner; the panel lists the error
  // under its target; Try again clears both; Sync now runs from the panel,
  // which stays open.
  page = await openApp('error');
  home = await page.evaluate(HOME);
  expect(home.name === 'Sync: Not synced' && home.dot && !home.busy && oneLine(home)
    && home.error === 'Not synced: the token was rejected; save a new one in Settings → Sync' && !home.attention, `error Home: ${JSON.stringify(home)}`);
  const dotColour = await page.evaluate(`getComputedStyle(document.getElementById('sync-dot')).backgroundColor === getComputedStyle(document.querySelector('.sessions-error')).color`);
  expect(dotColour, 'the dot is not --danger');
  await page.shot('home-error-desktop');
  await page.evaluate(click('sync-indicator'));
  await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
  panel = await page.evaluate(PANEL);
  expect(JSON.stringify(panel.lines) === JSON.stringify([['Git · Not synced · last 5 min ago', 'the token was rejected; save a new one in Settings → Sync'], ['WebDAV · Last synced 12 min ago']])
    && panel.inside, `error panel: ${JSON.stringify(panel)}`);
  await page.shot('panel-error-desktop');
  await page.evaluate(escape);
  await page.evaluate(click('sync-banner-retry'));
  await until('the banner to clear', () => page.evaluate(`document.getElementById('sync-banner').hidden`));
  home = await page.evaluate(HOME);
  expect(home.name === 'Sync: Synced just now' && !home.dot && (await page.evaluate(`document.activeElement?.id`)) === 'sync-indicator', `after Try again: ${JSON.stringify(home)}`);
  await page.evaluate(click('sync-indicator'));
  await page.evaluate(click('sync-panel-now'));
  panel = await page.evaluate(PANEL);
  home = await page.evaluate(HOME);
  expect(panel.open && panel.now === 'Syncing…' && panel.nowDisabled && home.busy && home.name === 'Sync: Syncing…', `panel while syncing: ${JSON.stringify({ panel, home })}`);
  await until('the panel report', () => page.evaluate(`document.getElementById('sync-panel-status').textContent.startsWith('Synced')`));
  panel = await page.evaluate(PANEL);
  home = await page.evaluate(HOME);
  expect(panel.open && panel.now === 'Sync now' && !panel.nowDisabled && panel.focus === 'sync-panel-now' && !home.busy
    && panel.status === 'Synced. Git: 1 pulled, 1 pushed. WebDAV: 3 uploaded, 1 downloaded.', `panel after Sync now: ${JSON.stringify(panel)}`);
  await page.close();

  // Needs attention: banner and a panel line; Review opens the list, Acknowledge clears it.
  page = await openApp('attention');
  home = await page.evaluate(HOME);
  expect(home.name === 'Sync: Synced 5 min ago' && !home.dot && home.attention === '2 files changed on two devices need a look.' && !home.error, `attention Home: ${JSON.stringify(home)}`);
  await page.evaluate(click('sync-indicator'));
  await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
  panel = await page.evaluate(PANEL);
  expect(panel.attention === '2 files need a look', `attention panel: ${JSON.stringify(panel)}`);
  await page.evaluate(escape);
  await page.shot('home-attention-desktop');
  await page.evaluate(click('sync-banner-review'));
  await until('the attention list', () => page.evaluate(`document.activeElement?.id === 'sync-attention-heading'`));
  sections = await page.evaluate(SECTIONS);
  expect(sections.sync.open && sections.sync.summary === 'Synced 5 min ago', `Review left Sync collapsed: ${JSON.stringify(sections.sync)}`);
  section = await page.evaluate(SECTION);
  expect(JSON.stringify(section.paths) === '["graph/topics.json","prompts/tutor.md"]', `attention list: ${JSON.stringify(section.paths)}`);
  await page.shot('settings-attention-desktop');
  await page.evaluate(click('sync-acknowledge'));
  await until('acknowledged', () => page.evaluate(`document.getElementById('sync-attention').hidden`));
  section = await page.evaluate(SECTION);
  expect(section.status === 'Acknowledged.' && section.focus === 'sync-toggle', `after Acknowledge: ${JSON.stringify(section)}`);
  await page.evaluate(closeSettings);
  home = await page.evaluate(HOME);
  expect(!home.banner, `banner after Acknowledge: ${JSON.stringify(home)}`);
  await page.close();

  // Phones at 390 px (both themes) and 320 px: the header is one line, the
  // icon a 44 px target, and the panel opens inside the viewport.
  for (const [width, dark] of [[390, false], [390, true], [320, false]]) {
    const name = `${width}-${dark ? 'dark' : 'light'}`;
    page = await openApp('error', { width, height: 844, mobile: true, dark });
    home = await page.evaluate(HOME);
    expect(!home.overflow && oneLine(home) && home.coarse && home.iconSize[0] >= 44 && home.iconSize[1] >= 44 && home.dot,
      `phone Home ${name}: ${JSON.stringify(home)}`);
    const clipped = await page.evaluate(`(() => { const w = document.querySelector('.home-bar .wordmark'); return w.scrollWidth > w.clientWidth; })()`);
    expect(width < 390 || !clipped, `the wordmark is cut short at ${name}`);
    await page.shot(`home-error-phone-${name}`);
    await page.evaluate(click('sync-indicator'));
    await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
    panel = await page.evaluate(PANEL);
    expect(panel.rightAligned && panel.below && panel.inside, `phone panel ${name}: ${JSON.stringify(panel)}`);
    await page.shot(`panel-phone-${name}`);
    await page.evaluate(click('sync-panel-settings'));
    await until('Settings at Sync', () => page.evaluate(`document.activeElement?.id === 'sync-toggle'`));
    const wide = await page.evaluate(`document.getElementById('settings').scrollWidth > document.getElementById('settings').clientWidth`);
    expect(!wide, `phone Settings ${name} overflows sideways`);
    if (width === 390) await page.shot(`settings-sync-phone-${name}`);
    await page.evaluate(collapseBoth);
    sections = await page.evaluate(SECTIONS);
    expect(!sections.sync.open && sections.sync.summary === 'Not synced', `phone collapsed Sync ${name}: ${JSON.stringify(sections.sync)}`);
    if (width === 390) await page.shot(`settings-collapsed-phone-${name}`);
    await page.close();
  }

  // An edge-to-edge phone: nothing tappable sits under the status bar/notch
  // (top 47) or the gesture bar (bottom 34), and the Settings sheet scrolls
  // under its inset instead of behind the status bar.
  const insets = { top: 47, bottom: 34, left: 0, right: 0 };
  page = await openApp('error', { width: 390, height: 844, mobile: true, insets });
  const notch = await page.evaluate(`(() => {
    const top = (sel) => document.querySelector(sel).getBoundingClientRect().top;
    const out = { homeBar: top('.home-bar'), settingsButton: top('#open-settings') };
    document.getElementById('home').hidden = true;
    document.getElementById('conversation').hidden = false;
    out.head = top('#conversation .head-inner');
    out.deskBottom = document.querySelector('#conversation .desk-inner').getBoundingClientRect().bottom;
    document.getElementById('conversation').hidden = true;
    document.getElementById('home').hidden = false;
    return out;
  })()`);
  expect(notch.homeBar >= insets.top && notch.settingsButton >= insets.top, `notched phone Home under the status bar: ${JSON.stringify(notch)}`);
  expect(notch.head >= insets.top, `notched phone session head under the status bar: ${JSON.stringify(notch)}`);
  expect(notch.deskBottom <= 844 - insets.bottom, `notched phone desk under the gesture bar: ${JSON.stringify(notch)}`);
  await page.shot('home-notch-phone');
  await page.evaluate(click('sync-indicator'));
  await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
  panel = await page.evaluate(PANEL);
  expect(panel.inside && panel.box[1] >= insets.top, `notched phone panel: ${JSON.stringify(panel)}`);
  await page.shot('panel-notch-phone');
  await page.evaluate(escape);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  await sleep(400);
  // The sheet's scrollport (where scrolled content is clipped) must start
  // below the status bar and end above the gesture bar.
  const sheet = await page.evaluate(`(() => {
    const settings = document.getElementById('settings');
    const box = settings.getBoundingClientRect();
    const head = document.querySelector('.settings-head').getBoundingClientRect().top;
    settings.scrollTop = settings.scrollHeight / 2;
    return {
      head,
      scrolled: settings.scrollTop > 0,
      viewportTop: box.top + settings.clientTop,
      viewportBottom: box.top + settings.clientTop + settings.clientHeight,
    };
  })()`);
  expect(sheet.head >= insets.top && sheet.scrolled && sheet.viewportTop >= insets.top && sheet.viewportBottom <= 844 - insets.bottom,
    `notched phone Settings: ${JSON.stringify(sheet)}`);
  await page.shot('settings-notch-phone');
  await page.close();
  page = await openApp('synced', { dark: true });
  home = await page.evaluate(HOME);
  expect(oneLine(home) && home.sameColour && !home.dot && home.name === 'Sync: Synced 5 min ago', `desktop dark Home: ${JSON.stringify(home)}`);
  await page.shot('home-synced-desktop-dark');
  await page.evaluate(click('sync-indicator'));
  await until('the panel', () => page.evaluate(`!document.getElementById('sync-panel').hidden`));
  await page.shot('panel-desktop-dark');
  await page.evaluate(escape);
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  await page.evaluate(`document.getElementById('providers-toggle').scrollIntoView({ block: 'center' })`);
  await page.shot('settings-collapsed-desktop-dark');
  await page.close();
  console.log('sync: sync icon (states, names, one-line header at 1440/390/320), its panel (placement, Escape/icon/outside close, Sync now, Sync settings), banner, collapsible Providers/Sync, Settings → Sync (save, refuse, Sync now, keep secret, disconnect, acknowledge), phone layout and notch/gesture-bar insets checked');
}

const cases = { ipc, placement, sync };
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
