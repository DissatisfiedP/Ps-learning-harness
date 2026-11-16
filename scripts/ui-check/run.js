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
//              sentence, A2, A3 and A4 follow the message in creation order,
//              keyboard moves of A1's R2 slider re-solve through the bridge,
//              update the probe and post a `state` line, and A4, whose view
//              throws while handling `init`, goes back to its fallback.
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
//   library    Home's "+" menu: a folder imported as a reference library
//              becomes a "Reference library" chip, the menu then lists it as
//              linked, Escape closes the menu; Settings → Libraries lists it.
//   reading    a Teach check that follows the tutor's delivered explanation
//              (`message_send`): the explanation renders as the tutor's own
//              message, the check waits collapsed below it while the desk
//              shows "While you read", the check opens once its bar is
//              scrolled into view (reasoning first and large, answer small),
//              and the desk text rides the answer as `thinkingWhileReading`;
//              a prediction is open at once and has no reading box.
//   offer      an offer in the session state opens the consent screen by
//              itself with the reason (the tutor asked, or the plan lists
//              the tool for the section reached): a registry plugin to
//              install, or a built-in to allow. Cancel, Install and Enable
//              report the decision with `resolve_install_offer`, and the
//              screen doesn't come back.
//
// Exit code 0 when every check passes. Usage: run.js [ipc|placement|sync|library]...

import { mkdtempSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, normalize, extname } from 'node:path';

const ROOT = normalize(join(import.meta.dir, '../..'));
const read = (p) => readFileSync(join(ROOT, p), 'utf8');

const APP_CSP = JSON.parse(read('crates/app/tauri.conf.json')).app.security.csp;
const VIEW_CSP = read('crates/app/src/lib.rs').match(/const VIEW_CSP: &str = "([^"]+)";/)?.[1];
if (!APP_CSP || !VIEW_CSP) throw new Error('could not read the app CSP or VIEW_CSP');

// The core serves every view with its start-up guard in front
// (`with_view_guard` in crates/harness/src/plugins/mod.rs).
const VIEW_GUARD = read('crates/harness/src/plugins/view_guard.js');
const guarded = (html) => html.replace(/<head(\s[^>]*)?>/i, (tag) => `${tag}<script>${VIEW_GUARD}</script>`);

// A view that throws while handling `init`, as circuit 0.1.0's did when a
// result's empty `warnings` list arrived as {}.
const FAULTY_VIEW = `<!doctype html><html><head><title>Faulty</title></head><body><p>half drawn</p><script>
addEventListener('message', (event) => { if (event.data?.type === 'init') event.data.artifact.data.warnings.join(' '); });
</script></body></html>`;

const PLUGIN_DIRS = { circuit: 'plugins/circuit', 'ipc-probe': 'plugins/fixtures/ipc-probe' };
function viewHtml(id) {
  if (id === 'faulty') return guarded(FAULTY_VIEW);
  const dir = PLUGIN_DIRS[id];
  if (!dir) return null;
  const manifest = Bun.TOML.parse(read(`${dir}/plugin.toml`));
  const html = read(`${dir}/${manifest.view[0].html}`);
  // The core inlines package assets; these fixtures have none to inline.
  if (/<script[^>]+\bsrc=|<link[^>]+\bhref=/i.test(html)) throw new Error(`${id}: view references assets; inline them for this check`);
  return guarded(html);
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
/** `until`, but a timeout reports what the page looked like. */
async function untilDump(what, fn, page) {
  try {
    return await until(what, fn);
  } catch (err) {
    const snap = await page.evaluate(`({ confirmOpen: document.getElementById('confirm')?.open, clock: document.querySelector('.exam-clock')?.textContent, paper: Boolean(document.querySelector('.exam-paper')), boxes: document.querySelectorAll('.exam-paper textarea').length, buttons: [...document.querySelectorAll('#practice button')].map((b) => b.textContent.trim() + (b.disabled ? ' (disabled)' : '') + (b.hidden ? ' (hidden)' : '')), status: document.querySelector('#practice .form-status')?.textContent, errs: window.__errors, tail: window.__bridge.log.slice(-6).map((c) => c.cmd) })`);
    throw new Error(`${err.message}: ${JSON.stringify(snap)}`);
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
  expect(JSON.stringify(layout.appended) === '["A2","A3","A4"]', 'A2, A3, A4 are not appended after the message in order');
  expect(JSON.stringify(layout.all) === '["A1","A2","A3","A4"]', 'each artifact must render exactly once');
  expect(layout.fallbackA3, 'A3 (plugin not installed) should show its fallback SVG');

  // A4's view is loaded, throws, and the figure shows the fallback again.
  await until('the faulty view to load', () => hits.some((h) => h.host === 'pluginview.localhost' && h.path === '/faulty'));
  await until('A4 back on its fallback', () => page.evaluate(`(() => {
    const fig = document.querySelector('figure[data-artifact-id="A4"]');
    return !fig.classList.contains('interactive') && !fig.querySelector('iframe') && fig.querySelector('svg')?.textContent === 'fallback A4';
  })()`));

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
  console.log(`placement: A1 in the sentence, A2, A3 and A4 appended in order; A4's failed view replaced by its fallback; slider R2 → Vout ${vout.toFixed(3)} V through the bridge`);
}

// --- sync --------------------------------------------------------------------

const SHOTS = process.env.UI_CHECK_SHOTS;

/** The app page in one sync state (`?sync=`), at a viewport and colour scheme.
 *  `insets` emulates an edge-to-edge phone's status bar/notch and gesture bar
 *  (`env(safe-area-inset-*)`), e.g. `{ top: 47, bottom: 34 }`; `mobile` also
 *  turns on touch, so `(pointer: coarse)` matches as on a phone. */
async function openApp(scenario, { width = 1440, height = 900, mobile = false, dark = false, insets = null, query = '', reducedMotion = false } = {}) {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('Page.enable', {}, sessionId);
  await send('Runtime.enable', {}, sessionId);
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile }, sessionId);
  if (mobile) await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 }, sessionId);
  if (insets) await send('Emulation.setSafeAreaInsetsOverride', { insets }, sessionId);
  await send('Emulation.setEmulatedMedia', { features: [
    { name: 'prefers-color-scheme', value: dark ? 'dark' : 'light' },
    { name: 'prefers-reduced-motion', value: reducedMotion ? 'reduce' : 'no-preference' },
  ] }, sessionId);
  await send('Page.addScriptToEvaluateOnNewDocument', {
    source: `window.__errors = [];
      addEventListener('error', (e) => __errors.push(String(e.message)));
      addEventListener('unhandledrejection', (e) => __errors.push(String(e.reason?.message ?? e.reason)));`,
  }, sessionId);
  await send('Page.navigate', { url: `http://tauri.localhost/index.html?check&sync=${scenario}${query}` }, sessionId);
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
  /** A real pointer (CDP mouse events, which Chromium turns into pointer events). */
  const mouse = (type, x, y, buttons = 0) =>
    send('Input.dispatchMouseEvent', { type, x, y, button: type === 'mouseMoved' && !buttons ? 'none' : 'left', buttons, clickCount: type === 'mouseMoved' ? 0 : 1 }, sessionId);
  /** Real fingers (CDP touch events): `type` is touchStart, touchMove or touchEnd; `points` are [x, y] pairs. */
  const touch = (type, points = []) =>
    send('Input.dispatchTouchEvent', { type, touchPoints: points.map(([x, y], id) => ({ x, y, id })) }, sessionId);
  return { evaluate, shot, close, mouse, touch };
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
  // Settings is tabs: one panel at a time, each holding its own sections.
  const tabs = await page.evaluate(`({
    names: [...document.querySelectorAll('#settings-tabs [role=tab]')].map((t) => t.textContent),
    selected: [...document.querySelectorAll('#settings-tabs [role=tab][aria-selected=true]')].map((t) => t.dataset.tab),
    shown: [...document.querySelectorAll('#settings .settings-panel')].filter((p) => !p.hidden).map((p) => p.id),
    panels: [...document.querySelectorAll('#settings .settings-panel')].map((p) => [p.id, ...[...p.querySelectorAll('.settings-section > h2')].map((h) => (h.querySelector('.section-name') ?? h).textContent)]),
  })`);
  expect(JSON.stringify(tabs.names) === JSON.stringify(['Models', 'Learning', 'Plugins', 'Sync', 'Pen', 'Appearance'])
    && JSON.stringify(tabs.selected) === '["models"]' && JSON.stringify(tabs.shown) === '["tab-models"]'
    && JSON.stringify(tabs.panels) === JSON.stringify([
      ['tab-models', 'Model roles', 'Providers'], ['tab-sync', 'Sync'], ['tab-learning', 'Learning', 'Focus', 'Libraries'],
      ['tab-plugins', 'Plugins', 'Registries'], ['tab-pen', 'Pen', 'Pen buttons', 'Try your pen'], ['tab-appearance', 'Appearance']]),
    `Settings tabs: ${JSON.stringify(tabs)}`);
  const others = await page.evaluate(`({ providers: document.querySelectorAll('#provider-list .provider').length, plugins: document.querySelectorAll('#plugin-list .plugin').length, roles: document.getElementById('model-form').textContent.trim() })`);
  expect(others.providers === 3 && others.plugins === 2 && others.roles.length > 0, `other Settings sections did not render: ${JSON.stringify(others)}`);

  // Providers and Sync start collapsed, each heading carrying its summary.
  let sections = await page.evaluate(SECTIONS);
  expect(!sections.providers.open && !sections.providers.shown && sections.providers.summary === '1 connected'
    && !sections.sync.open && !sections.sync.shown && sections.sync.summary === 'Off', `default sections: ${JSON.stringify(sections)}`);
  await page.evaluate(`document.getElementById('providers-toggle').scrollIntoView({ block: 'center' })`);
  await page.shot('settings-collapsed-desktop');
  await page.evaluate(click('tab-btn-sync'));
  expect(await page.evaluate(`!document.getElementById('sync-section').closest('.settings-panel').hidden && document.getElementById('tab-btn-sync').getAttribute('aria-selected') === 'true' && localStorage.getItem('lh.settings.tab') === 'sync'`), 'the Sync tab did not show its panel');
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
  // The Providers help line names where keys are kept; the 0600-file
  // fallback (no Secret Service) says so in a notice.
  const STORAGE = `({ line: document.getElementById('secret-storage').textContent, notice: document.getElementById('secret-storage-notice').textContent, noticeShown: !document.getElementById('secret-storage-notice').hidden })`;
  let storage = await page.evaluate(STORAGE);
  expect(storage.line === 'Keys stay on this device, in the Secret Service (desktop keyring).' && !storage.noticeShown, `secure storage line: ${JSON.stringify(storage)}`);
  const fallback = 'The Secret Service (desktop keyring) could not be used (no daemon), so keys are kept in /home/l/.config/app/secrets.json, readable only by your account.';
  await page.evaluate(`Object.assign(window.__bridge.secretStorage, { backend: 'file', label: '/home/l/.config/app/secrets.json', secure: false, notice: ${JSON.stringify(fallback)} })`);
  await page.evaluate(closeSettings);
  await page.evaluate(click('open-settings'));
  await until('the key storage notice', () => page.evaluate(`!document.getElementById('secret-storage-notice').hidden`));
  storage = await page.evaluate(STORAGE);
  expect(storage.line === 'Keys stay on this device, in a file only your account can read.' && storage.notice === fallback, `fallback storage line: ${JSON.stringify(storage)}`);
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
    && home.error === 'Git not synced: the token was rejected; save a new one in Settings → Sync' && !home.attention, `error Home: ${JSON.stringify(home)}`);
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

  // Git fails, WebDAV syncs: the run line names the failing target instead of
  // repeating git's raw error under Sync now, where it read as WebDAV's.
  page = await openApp('synced');
  await page.evaluate(click('open-settings'));
  await until('Settings (git fails, WebDAV syncs)', () => page.evaluate(`document.getElementById('settings').open`));
  await page.evaluate(`(t => t.getAttribute('aria-expanded') === 'true' || t.click())(document.getElementById('sync-toggle'))`);
  await page.evaluate(fill('sync-git-remote', 'https://offline.example/notes.git'));
  await page.evaluate(submit('sync-git'));
  await until('git saved', () => page.evaluate(`document.getElementById('sync-git-status').textContent === 'Saved.'`));
  await page.evaluate(closeSettings);
  await page.evaluate(click('sync-indicator'));
  await page.evaluate(click('sync-panel-now'));
  await until('the partial-failure report', () => page.evaluate(`document.getElementById('sync-panel-status').textContent.includes('didn')`));
  panel = await page.evaluate(PANEL);
  home = await page.evaluate(HOME);
  expect(panel.status === 'Git didn’t sync (see above). WebDAV synced.'
    && JSON.stringify(panel.lines[0]) === JSON.stringify(['Git · Not synced · last 5 min ago', 'the network is unreachable; try again when you are online'])
    && panel.lines[1][0] === 'WebDAV · Last synced just now'
    && home.error === 'Git not synced: the network is unreachable; try again when you are online',
    `git fails, WebDAV syncs: ${JSON.stringify({ panel, home })}`);
  await page.shot('panel-git-fails-webdav-syncs');
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
    settings.querySelector('.settings-col').style.minHeight = '3000px'; // a tab can be shorter than the screen
    settings.scrollTop = settings.scrollHeight / 2;
    const top = document.querySelector('.settings-top').getBoundingClientRect().top;
    return {
      head,
      scrolled: settings.scrollTop > 0,
      stuck: Math.abs(top - (box.top + settings.clientTop)) < 1,
      viewportTop: box.top + settings.clientTop,
      viewportBottom: box.top + settings.clientTop + settings.clientHeight,
    };
  })()`);
  expect(sheet.head >= insets.top && sheet.scrolled && sheet.stuck && sheet.viewportTop >= insets.top && sheet.viewportBottom <= 844 - insets.bottom,
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

async function library() {
  const page = await openApp('none');
  const MENU = `(() => {
    const m = document.getElementById('add-menu');
    return { open: !m.hidden, items: [...m.querySelectorAll('.menu-item')].map((b) => [b.firstChild.textContent, b.disabled]),
      heading: m.querySelector('.menu-heading')?.textContent ?? null, expanded: document.getElementById('add-files').getAttribute('aria-expanded'),
      focus: document.activeElement?.id || document.activeElement?.className };
  })()`;
  await page.evaluate(click('add-files'));
  await until('the add menu', () => page.evaluate(`!document.getElementById('add-menu').hidden`));
  let menu = await page.evaluate(MENU);
  expect(menu.expanded === 'true' && JSON.stringify(menu.items) === JSON.stringify([['Files…', false], ['Folder as a reference library…', false]]) && menu.heading === null,
    `empty add menu: ${JSON.stringify(menu)}`);
  await page.shot('library-menu-empty');
  await page.evaluate(`document.querySelectorAll('#add-menu .menu-item')[1].click()`);
  await until('the library chip', () => page.evaluate(`[...document.querySelectorAll('#attach-chips .chip')].some((c) => c.dataset.kind === 'library')`));
  const chip = await page.evaluate(`(() => { const c = document.querySelector('#attach-chips .chip[data-kind="library"]'); return { name: c.querySelector('.chip-name').textContent, kind: c.querySelector('.chip-kind').textContent, status: document.getElementById('attach-error').textContent }; })()`);
  expect(chip.name === 'huawei-ict' && chip.kind === 'Reference library' && chip.status === 'huawei-ict: 2 files (2 added).', `library chip: ${JSON.stringify(chip)}`);
  await page.shot('library-chip');
  await page.evaluate(click('add-files'));
  await until('the library list in the menu', () => page.evaluate(`Boolean(document.querySelector('#add-menu .menu-heading'))`));
  menu = await page.evaluate(MENU);
  expect(menu.heading === 'Link a library' && JSON.stringify(menu.items[2]) === JSON.stringify(['huawei-ict', true]), `menu after import: ${JSON.stringify(menu)}`);
  await page.shot('library-menu-linked');
  await page.evaluate(escape);
  menu = await page.evaluate(MENU);
  expect(!menu.open && menu.focus === 'add-files', `Escape: ${JSON.stringify(menu)}`);
  await page.evaluate(click('open-settings'));
  await until('Settings → Libraries', () => page.evaluate(`document.querySelectorAll('#library-list .library').length === 1`));
  const row = await page.evaluate(`document.querySelector('#library-list .library').textContent`);
  expect(/1 guide · 1 question bank/.test(row) && /From \/home\/learner\/huawei-ict on this device/.test(row) && /Refresh/.test(row), `library row: ${row}`);
  await page.evaluate(`document.querySelector('#library-list .library').scrollIntoView({ block: 'center' })`);
  await page.shot('library-settings');
  // Settings → Libraries → Image sets: empty, import a labelled folder, remove it.
  const SETS = `({ rows: [...document.querySelectorAll('#image-set-list .image-set')].map((r) => r.textContent), help: document.querySelector('#image-set-list .help')?.textContent ?? null, status: document.getElementById('image-set-status').textContent })`;
  let sets = await page.evaluate(SETS);
  expect(sets.rows.length === 0 && sets.help === 'No image sets yet.', `empty image sets: ${JSON.stringify(sets)}`);
  await page.evaluate(click('image-set-import'));
  await until('the imported image set', () => page.evaluate(`document.querySelectorAll('#image-set-list .image-set').length === 1`));
  sets = await page.evaluate(SETS);
  expect(/huawei-ict/.test(sets.rows[0]) && /7 images, 2 labels: crow 4 · raven 3/.test(sets.rows[0])
    && sets.status === 'huawei-ict: 7 images in 2 labels. Skipped 1 file (not an image, or over 5 MB).', `image set row: ${JSON.stringify(sets)}`);
  const imported = await page.evaluate(`${argsOf('image_set_import')}`);
  expect(JSON.stringify(imported) === JSON.stringify([{ path: '/home/learner/huawei-ict', name: null }]), `image_set_import args: ${JSON.stringify(imported)}`);
  await page.evaluate(`document.getElementById('image-set-list').scrollIntoView({ block: 'center' })`);
  await page.shot('image-sets-settings');
  await page.evaluate(`[...document.querySelectorAll('#image-set-list .image-set button')].find((b) => b.textContent === 'Remove').click()`);
  await until('the remove confirmation', () => page.evaluate(`document.getElementById('confirm').open`));
  const confirmTitle = await page.evaluate(`document.getElementById('confirm-title').textContent`);
  expect(confirmTitle === 'Remove huawei-ict?', `confirm title: ${confirmTitle}`);
  await page.evaluate(click('confirm-ok'));
  await until('the set removed', () => page.evaluate(`document.querySelectorAll('#image-set-list .image-set').length === 0`));
  sets = await page.evaluate(SETS);
  expect(sets.status === 'Removed huawei-ict.' && sets.help === 'No image sets yet.', `after remove: ${JSON.stringify(sets)}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('library: Home + menu (files, folder as library, link list), import → chip with result line, linked state, Escape, Settings → Libraries row, Image sets (empty, import, row, remove with confirmation) checked');
}


const FIXTURE = { query: '&fixture=learn' };
const argsOf = (cmd) => `(window.__bridge.log.filter((c) => c.cmd === '${cmd}').map((c) => c.args))`;

/** Where an idea is drawn on the Map, in viewport px: focus its hidden
 *  checkbox (the card then sits beside it, at the idea's height unless the
 *  stage's edge clamps it), then sweep the pointer beside the card until the
 *  canvas reports a hit on that idea. */
async function locate(page, key, title) {
  // The status line sits over the stage's centre and takes the pointer; it
  // was read already, so clear it the way the next map action would.
  await page.evaluate(`document.querySelector('#map-page .map-status').replaceChildren()`);
  await page.evaluate(`document.querySelector('#map-page .map-a11y input[data-key="${key}"]').focus()`);
  await sleep(700); // a focus off-screen pans the camera there
  const box = await page.evaluate(`(() => { const r = document.querySelector('#map-page .map-card').getBoundingClientRect();
    const s = document.getElementById('map-page').getBoundingClientRect(); return { l: r.left, r: r.right, t: r.top, b: r.bottom, st: s.top, sb: s.bottom }; })()`);
  await page.evaluate(`document.activeElement.blur()`);
  const ys = [box.t + 28, box.t + 34, box.t + 22];
  // A card pinned to the stage's bottom (or top) edge no longer marks the
  // idea's height: sweep the band the idea can be in.
  if (box.b >= box.sb - 12) for (let y = box.t + 40; y < box.sb; y += 6) ys.push(y);
  if (box.t <= box.st + 12) for (let y = box.st; y < box.t + 22; y += 6) ys.push(y);
  for (const y of ys) {
    for (let d = 14; d <= 110; d += 4) {
      for (const x of [box.l - d, box.r + d]) {
        await page.mouse('mouseMoved', x, y);
        const ok = await page.evaluate(`(() => { const c = document.querySelector('#map-page .map-card');
          return document.querySelector('#map-page canvas').style.cursor === 'pointer' && !c.hidden && c.querySelector('.map-card-title')?.textContent === ${JSON.stringify(title)}; })()`);
        if (ok) return { x, y };
      }
    }
  }
  throw new Error(`could not find ${title} on the map (card ${JSON.stringify(box)})`);
}

async function map() {
  const page = await openApp('none', { ...FIXTURE, reducedMotion: true });
  await page.evaluate(click('open-map'));
  await until('the map with a split to review', () => page.evaluate(`(() => { const b = document.querySelector('#map-page .map-splits'); return Boolean(b && !b.hidden); })()`));
  const button = await page.evaluate(`document.querySelector('#map-page .map-splits').textContent`);
  expect(button === 'Review 1 topic split', `split button: ${button}`);
  await page.shot('map-split-button');

  // Split review: rename part 1, accept, close → one decide_splits, map reloads.
  await page.evaluate(`document.querySelector('#map-page .map-splits').click()`);
  const dialog = await page.evaluate(`(() => { const d = document.querySelector('dialog.splits');
    return { open: d.open, focus: document.activeElement?.id, topic: d.querySelector('.proposal-edge')?.textContent,
      relation: d.querySelector('.proposal-relation')?.textContent, labels: [...d.querySelectorAll('.split-name-label')].map((l) => l.textContent),
      names: [...d.querySelectorAll('.split-name')].map((i) => i.value), ideas: [...d.querySelectorAll('.split-ideas li')].map((l) => l.textContent) }; })()`);
  expect(dialog.open && dialog.focus === 'splits-heading' && dialog.topic === 'Mathematics › Calculus' && dialog.relation === '4 ideas → 2 new topics inside it'
    && dialog.labels.join('|') === 'Name for part 1|Name for part 2' && dialog.names[0] === 'Limit of a function (1)'
    && dialog.ideas.join('|') === 'Limit of a function|Continuity at a point|Derivative as a limit|Chain rule', `split dialog: ${JSON.stringify(dialog)}`);
  await page.shot('map-split-dialog');
  await page.evaluate(`(() => { const d = document.querySelector('dialog.splits');
    const i = d.querySelector('.split-name'); i.value = 'Limits and continuity'; i.dispatchEvent(new Event('input'));
    [...d.querySelectorAll('.proposal-actions button')].find((b) => b.textContent === 'Accept').click();
    return true; })()`);
  const accepted = await page.evaluate(`document.querySelector('dialog.splits .proposal-state')?.textContent`);
  expect(accepted === 'Accepted — saved when you close this list.', `accepted state: ${accepted}`);
  await page.evaluate(`document.querySelector('dialog.splits .proposals-head button').click()`);
  await until('the split saved and the map reloaded', () => page.evaluate(`[...document.querySelectorAll('#map-page .map-a11y legend')].some((l) => l.textContent === 'Mathematics › Calculus › Limits and continuity')`));
  const decided = await page.evaluate(argsOf('decide_splits'));
  expect(JSON.stringify(decided) === JSON.stringify([{ accept: ['calc'], dismiss: [], titles: { calc: ['Limits and continuity', 'Derivative as a limit (2)'] } }]),
    `decide_splits: ${JSON.stringify(decided)}`);
  let status = await page.evaluate(`document.querySelector('#map-page .map-status')?.textContent`);
  expect(status === 'Split “Mathematics › Calculus” into new topics.', `split status: ${status}`);
  const splitsLeft = await page.evaluate(`document.querySelector('#map-page .map-splits').hidden`);
  expect(splitsLeft, 'the split button is gone once nothing is left to review');
  await page.shot('map-after-split');

  // Drag "Quadratic formula" (Algebra) onto "Limit of a function": the drop
  // asks first, with that topic chosen; Move records a learner place.
  const from = await locate(page, 'quadratic', 'Quadratic formula');
  const to = await locate(page, 'limit', 'Limit of a function');
  await page.mouse('mouseMoved', from.x, from.y);
  await page.mouse('mousePressed', from.x, from.y, 1);
  for (let i = 1; i <= 12; i++) {
    await page.mouse('mouseMoved', from.x + ((to.x - from.x) * i) / 12, from.y + ((to.y - from.y) * i) / 12, 1);
    await sleep(16);
  }
  await page.shot('map-drag-target');
  await page.mouse('mouseReleased', to.x, to.y);
  await until('the move dialog', () => page.evaluate(`document.querySelector('dialog.map-move').open`));
  const move = await page.evaluate(`(() => { const d = document.querySelector('dialog.map-move'); const s = d.querySelector('select');
    return { intro: d.querySelector('.proposals-intro').textContent, value: s.value, chosen: s.selectedOptions[0]?.textContent,
      options: [...s.options].map((o) => o.textContent), focus: document.activeElement?.textContent }; })()`);
  expect(move.intro === '“Quadratic formula” is in Mathematics › Algebra.' && move.value === 'limits-and-continuity'
    && move.chosen === 'Mathematics › Calculus › Limits and continuity' && !move.options.includes('Mathematics › Algebra') && move.focus === 'Move',
    `move dialog after the drop: ${JSON.stringify(move)}`);
  const before = await page.evaluate(`${argsOf('place_node')}.length`);
  expect(before === 0, 'nothing is placed before the learner confirms');
  await page.shot('map-move-dialog');
  await page.evaluate(`document.querySelector('dialog.map-move button[type="submit"]').click()`);
  await until('the move saved and the map reloaded', () => page.evaluate(`(() => { const s = document.querySelector('#map-page .map-status'); return /^Moved/.test(s?.textContent ?? ''); })()`));
  const placed = await page.evaluate(argsOf('place_node'));
  expect(JSON.stringify(placed) === JSON.stringify([{ nodeId: 'quadratic', home: 'limits-and-continuity' }]), `place_node: ${JSON.stringify(placed)}`);
  status = await page.evaluate(`document.querySelector('#map-page .map-status').textContent`);
  expect(status === 'Moved “Quadratic formula” to Mathematics › Calculus › Limits and continuity.', `move status: ${status}`);

  // Provenance on the card: the import and the learner's move…
  await locate(page, 'quadratic', 'Quadratic formula');
  await until('the card’s record', () => page.evaluate(`!document.querySelector('#map-page .map-card-origin-wait')`));
  let origin = await page.evaluate(`[...document.querySelectorAll('#map-page .map-card-origin-item')].map((li) => li.textContent)`);
  expect(origin.length === 2 && /^Imported · /.test(origin[0]) && /^Moved by you to Mathematics › Calculus › Limits and continuity · /.test(origin[1]),
    `quadratic's record: ${JSON.stringify(origin)}`);
  // …and the session it came from (a link back), its quote and a merge.
  await locate(page, 'chain', 'Chain rule');
  await until('the card’s record', () => page.evaluate(`!document.querySelector('#map-page .map-card-origin-wait')`));
  origin = await page.evaluate(`[...document.querySelectorAll('#map-page .map-card-origin-item')].map((li) => li.textContent)`);
  expect(/^From “Derivatives” · .*Chain rule is what the lesson rests on\.$/.test(origin[0]) && /^Merged with “Differentiating a composition”: the same claim/.test(origin[1]),
    `chain's record: ${JSON.stringify(origin)}`);
  await page.shot('map-card-provenance');
  // The learner model's memory on the card, with the open misconception.
  const memory = await page.evaluate(`[...document.querySelectorAll('#map-page .map-card-memory')].map((p) => p.textContent)`);
  expect(memory.length === 2 && memory[0] === 'Due for review · 62% recalled now · shown: recall'
    && memory[1] === 'Misconception (2026-11-10): differentiates the outer function only', `chain's memory: ${JSON.stringify(memory)}`);
  await locate(page, 'limit', 'Limit of a function');
  const held = await page.evaluate(`[...document.querySelectorAll('#map-page .map-card-memory')].map((p) => p.textContent)`);
  expect(held.length === 1 && held[0] === 'Held · 97% recalled now · shown: recall, discrimination', `limit's memory: ${JSON.stringify(held)}`);
  await locate(page, 'chain', 'Chain rule');

  // Without a pointer: the hidden list's "Move …" opens the same dialog.
  await page.evaluate(`(() => { const b = [...document.querySelectorAll('#map-page .map-a11y button')].find((x) => x.textContent === 'Move “Chain rule” to another topic…'); b.focus(); b.click(); })()`);
  const kb = await page.evaluate(`(() => { const d = document.querySelector('dialog.map-move'); return { open: d.open, focus: document.activeElement?.tagName }; })()`);
  expect(kb.open && kb.focus === 'SELECT', `keyboard move dialog: ${JSON.stringify(kb)}`);
  await page.evaluate(`[...document.querySelectorAll('dialog.map-move button')].find((b) => b.textContent === 'Cancel').click()`);
  await until('the dialog closed', () => page.evaluate(`!document.querySelector('dialog.map-move').open && !document.querySelector('dialog.map-move').childElementCount`));
  const back = await page.evaluate(`document.activeElement?.textContent`);
  expect(back === 'Move “Chain rule” to another topic…', `focus after Cancel: ${back}`);
  const unmoved = await page.evaluate(`${argsOf('place_node')}.length`);
  expect(unmoved === 1, 'Cancel places nothing');

  // The record's session link opens that session.
  await locate(page, 'chain', 'Chain rule');
  await page.evaluate(`document.querySelector('#map-page .map-card-origin-link').click()`);
  await until('the source session', () => page.evaluate(`document.getElementById('map').hidden && document.getElementById('session-title')?.textContent === 'Derivatives'`));
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('map: split review (rename, accept → decide_splits, reload), drag onto another topic → move dialog → place_node, card provenance (import, move, session link + quote, merge), keyboard move, link opens the session');
}

async function session() {
  const page = await openApp('none', FIXTURE);
  await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
  await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
  await until('the debrief in the flow', () => page.evaluate(`Boolean(document.querySelector('#timeline .debrief'))`));

  // The debrief's sorted claims, after the debrief's own words.
  const debrief = await page.evaluate(`(() => { const d = document.querySelector('#timeline .debrief');
    const reply = [...document.querySelectorAll('#timeline .entry.tutor')].find((e) => /Good finds/.test(e.textContent));
    return { heads: [...d.querySelectorAll('.debrief-verdict')].map((h) => h.textContent), question: d.querySelector('.debrief-question')?.textContent,
      tip: [...d.querySelectorAll('.debrief-tip .debrief-meta')].map((p) => p.textContent), sources: [...d.querySelectorAll('.debrief-meta')].map((p) => p.textContent).filter((t) => t.startsWith('Source')),
      partial: d.querySelector('.debrief-partial .debrief-note')?.textContent.trim(), after: Boolean(reply && reply.compareDocumentPosition(d) & Node.DOCUMENT_POSITION_FOLLOWING),
      label: document.getElementById(d.getAttribute('aria-labelledby'))?.textContent }; })()`);
  expect(debrief.heads.join('|') === 'Correct ✓ · 1|Partly ◐ · 1|Not right ✗ · 1|Rule of thumb · 1|Saved for later · 1'
    && debrief.question === 'What you brought back on: Find out what people mean by a limit, and where they disagree.'
    && debrief.tip.join('|') === 'Whose: calculus teachers|When it applies: single-variable functions you can sketch'
    && debrief.sources.join('|') === 'Source: lecture notes|Source: a forum thread'
    && debrief.partial === 'It is the value approached; the function need not reach it.' && debrief.after && debrief.label === 'Debrief',
    `debrief list: ${JSON.stringify(debrief)}`);
  await page.evaluate(`document.querySelector('#timeline .debrief').scrollIntoView({ block: 'center' })`);
  await page.shot('session-debrief');

  // The plan sheet while teaching carries the card's structured controls.
  await until('the Plan button', () => page.evaluate(`!document.getElementById('plan-toggle').hidden`));
  await page.evaluate(click('plan-toggle'));
  await until('the plan sheet', () => page.evaluate(`document.getElementById('plan-panel').open`));
  await page.evaluate(`[...document.querySelectorAll('#plan-panel .plan-level')].find((b) => b.textContent === 'Topics').click()`);
  await until('the topic boxes with their ⋯', () => page.evaluate(`document.querySelectorAll('#plan-panel .plan-node-action').length === 2`));
  await page.evaluate(`document.querySelector('#plan-panel .plan-node-action[data-section-id="s2"]').dispatchEvent(new MouseEvent('click', { bubbles: true }))`);
  const drawer = await page.evaluate(`(() => { const d = document.querySelector('#plan-panel .plan-node-drawer');
    return { shown: !d.hidden, title: d.querySelector('.plan-node-drawer-title')?.textContent, focus: document.activeElement?.className,
      legends: [...d.querySelectorAll('legend')].map((l) => l.textContent), labels: [...d.querySelectorAll('label')].map((l) => l.textContent),
      expanded: document.querySelector('#plan-panel .plan-node-action[data-section-id="s2"]').getAttribute('aria-expanded'),
      bar: document.querySelector('#plan-panel .plan-edit-bar').hidden }; })()`);
  expect(drawer.shown && drawer.title === 'Derivatives' && drawer.focus === 'plan-node-drawer-title' && drawer.expanded === 'true'
    && drawer.legends.join('|') === 'How far|Include' && drawer.labels.includes('Parts I already know…') && drawer.labels.includes('Note for the planner') && drawer.bar,
    `sheet drawer: ${JSON.stringify(drawer)}`);
  await page.evaluate(`(() => { const d = document.querySelector('#plan-panel .plan-node-drawer');
    d.querySelector('input[type="radio"][value="analyze"]').click();
    const t = [...d.querySelectorAll('.field')].find((f) => f.querySelector('label')?.textContent === 'Parts I already know…').querySelector('textarea');
    t.value = 'I know the power rule.'; t.dispatchEvent(new Event('input')); return true; })()`);
  const pending = await page.evaluate(`(() => { const p = document.getElementById('plan-panel');
    return { bar: !p.querySelector('.plan-edit-bar').hidden, hint: p.querySelector('.plan-hint').hidden, send: !p.querySelector('.plan-edit-bar .btn.primary').hidden,
      badge: [...p.querySelectorAll('.plan-node')].map((g) => g.textContent).find((t) => /Derivatives/.test(t)) }; })()`);
  expect(pending.bar && pending.hint && pending.send && /Analyze only/.test(pending.badge ?? ''), `pending edits: ${JSON.stringify(pending)}`);
  await page.shot('session-sheet-drawer');
  // Escape closes the drawer, not the sheet, and focus returns to the box's ⋯.
  await page.evaluate(escape);
  const esc = await page.evaluate(`({ open: document.getElementById('plan-panel').open, drawer: document.querySelector('#plan-panel .plan-node-drawer').hidden,
    focus: document.activeElement?.dataset?.sectionId })`);
  expect(esc.open && esc.drawer && esc.focus === 's2', `Escape in the sheet: ${JSON.stringify(esc)}`);
  // Plan-level options live in the Sections drawer; its "Options" reach a section too.
  await page.evaluate(`document.querySelector('#plan-panel .plan-drawer-toggle').click()`);
  await page.evaluate(`document.querySelector('#plan-panel .plan-drawer button[aria-label="Plan options"]').click()`);
  await page.evaluate(`document.querySelector('#plan-panel .plan-drawer input[type="radio"][value="deep"]').click()`);
  await page.evaluate(`[...document.querySelectorAll('#plan-panel .sheet-section-options')].find((b) => b.dataset.sectionId === 's1').click()`);
  const s1 = await page.evaluate(`(() => { const d = document.querySelector('#plan-panel .plan-node-drawer');
    return { title: d.querySelector('.plan-node-drawer-title')?.textContent, hunt: [...d.querySelectorAll('label')].some((l) => l.textContent === 'Hunt') }; })()`);
  expect(s1.title === 'Limits' && s1.hunt, `section from the list: ${JSON.stringify(s1)}`);
  await page.shot('session-sheet-sections');
  await page.evaluate(`document.querySelector('#plan-panel .plan-edit-bar .btn.primary').click()`);
  await until('the edits sent', () => page.evaluate(`document.querySelector('#plan-panel .plan-edit-bar .form-status')?.textContent === 'Sent. Revising the plan…'`));
  const sent = await page.evaluate(argsOf('edit_plan'));
  expect(JSON.stringify(sent) === JSON.stringify([{ sessionId: 'L1', edits: { depth: 'deep', sections: [{ sectionId: 's2', targetLevel: 'analyze', knownNote: 'I know the power rule.' }] }, comment: '' }]),
    `edit_plan: ${JSON.stringify(sent)}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('session: debrief list (five verdict groups, notes, sources, tip attribution/scope, after the reply), plan sheet controls (box ⋯ → drawer, level + known parts, Escape, plan options, list Options, Send → one edit_plan)');
}

async function reading() {
  const openSession = async (role) => {
    const page = await openApp('none', { query: `&fixture=learn&reading=${role}` });
    await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
    await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
    await until('the session', () => page.evaluate(`Boolean(document.querySelector('#timeline .debrief'))`));
    await page.evaluate(`window.__bridge.deliverReading()`);
    await until('the question card', () => page.evaluate(`Boolean(document.querySelector('#timeline .exercise.pending .ex-form'))`));
    return page;
  };
  const card = `document.querySelector('#timeline .exercise.pending')`;
  const look = `(() => { const card = ${card}; const desk = document.getElementById('desk-reading');
    const sent = [...document.querySelectorAll('#timeline .entry.tutor')].filter((e) => /secant slopes/.test(e.textContent));
    return { sent: sent.length, bold: sent[0]?.querySelector('strong')?.textContent, before: Boolean(sent[0] && sent[0].compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING),
      collapsed: card.classList.contains('collapsed'), form: card.querySelector('.ex-form').getClientRects().length > 0, prompt: card.querySelector('.prompt').getClientRects().length > 0,
      opener: card.querySelector('.ex-open')?.textContent, desk: !desk.hidden && desk.getClientRects().length > 0, label: desk.querySelector('label').textContent,
      fields: [...card.querySelectorAll('.ex-form .field')].map((f) => [f.querySelector('label').textContent, f.querySelector('textarea')?.rows]) }; })()`;

  // Reading: the check is closed below the fold, the reading box is in the desk.
  const page = await openSession('check');
  await sleep(300); // an open-on-arrival would have happened by now
  const closed = await page.evaluate(look);
  expect(closed.sent === 1 && closed.bold === 'shrink the step' && closed.before && closed.collapsed && !closed.form && !closed.prompt
    && closed.opener === 'Show the question' && closed.desk && closed.label === 'While you read (optional)', `while reading: ${JSON.stringify(closed)}`);
  await page.evaluate(`document.getElementById('desk-reading-input').value = '  Why shrink the step?  '`);
  await page.shot('reading-closed');

  // Reaching the end opens it; the reading box stays, with its text.
  await page.evaluate(`${card}.scrollIntoView({ block: 'end', behavior: 'instant' })`);
  await until('the check to open', () => page.evaluate(`!${card}.classList.contains('collapsed')`));
  const open = await page.evaluate(look);
  expect(open.form && open.prompt && !open.opener && open.desk
    && JSON.stringify(open.fields.slice(0, 2)) === JSON.stringify([['Your reasoning (optional)', 6], ['Your answer', 2]]), `opened: ${JSON.stringify(open)}`);
  expect(await page.evaluate(`document.getElementById('desk-reading-input').value`) === '  Why shrink the step?  ', 'the reading draft was lost when the check opened');
  await page.shot('reading-open');
  await page.evaluate(`(() => { const [reasoning, answer] = ${card}.querySelectorAll('.ex-form textarea');
    reasoning.value = 'The step goes to zero, so the slopes close in.';
    answer.value = 'It approaches one value.';
    ${card}.querySelector('.ex-form .confidence input[value="70"]').click();
    ${card}.querySelector('.ex-form button[type="submit"]').click(); })()`);
  await until('the answer sent', () => page.evaluate(`${argsOf('answer')}.length === 1`));
  const sent = (await page.evaluate(argsOf('answer')))[0];
  expect(sent.callId === 'c1' && sent.payload.thinkingWhileReading === 'Why shrink the step?' && sent.payload.answer === 'It approaches one value.'
    && sent.payload.thinking === 'The step goes to zero, so the slopes close in.' && sent.payload.confidence === 70 && sent.payload.dontKnow === false, `answer: ${JSON.stringify(sent)}`);
  let errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // A prediction comes before the teaching: open at once, nothing to read.
  const predict = await openSession('predict');
  await sleep(300);
  const p = await predict.evaluate(look);
  expect(!p.collapsed && !p.opener && !p.desk, `a prediction: ${JSON.stringify(p)}`);
  errors = await predict.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await predict.close();
  console.log('reading: a delivered explanation renders once, its check waits collapsed while the desk shows "While you read", scrolling to it opens it (reasoning large and first, answer small), the desk text rides the answer as thinkingWhileReading, a prediction is open with no reading box');
}

// The backend's turn status decides the "reply owed" offer; the page doesn't
// guess it from the last message.
async function turn() {
  const page = await openApp('none', { query: '&fixture=learn&turnError=DeepSeek%20is%20not%20connected' });
  await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
  await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
  await until('the offer', () => page.evaluate(`/The last reply failed: DeepSeek is not connected/.test(document.body.textContent)`));
  const offer = await page.evaluate(`Boolean([...document.querySelectorAll('button')].find((x) => x.textContent === 'Try again'))`);
  expect(offer, 'the retry button names the backend error');
  await page.evaluate(`[...document.querySelectorAll('.turn-error button')].find((x) => x.textContent === 'Try again').click()`);
  await sleep(400);
  const cmds = await page.evaluate(`window.__bridge.log.map((c) => c.cmd).slice(-12)`);
  expect(cmds.includes('resume_turn'), `resume_turn after the click; last commands: ${JSON.stringify(cmds)}`);
  await page.close();
  console.log('turn: a failed last turn is offered with the backend reason and retries through resume_turn');
}

// Install offers: the consent screen opens from the session state, and the
// learner's decision goes back to the backend.
async function offer() {
  const consent = `(() => { const d = document.getElementById('consent'); const why = document.getElementById('consent-why');
    return { open: d.open, title: document.getElementById('consent-title').textContent, why: why.hidden ? null : why.textContent,
      version: document.getElementById('consent-version').textContent }; })()`;
  const decisions = `window.__bridge.log.filter((c) => c.cmd === 'resolve_install_offer').map((c) => c.args)`;
  const openSession = async (by) => {
    const page = await openApp('none', { query: `&fixture=learn&offer=${by}` });
    await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
    await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
    await until(`the consent screen (${by})`, () => page.evaluate(`document.getElementById('consent').open`));
    return page;
  };

  // The tutor asked: the reason is on the screen; Cancel declines, once.
  let page = await openSession('tutor');
  let shown = await page.evaluate(consent);
  expect(shown.title === 'Tally' && shown.version === '0.3.0' && shown.why === 'The tutor would like to use this tool: to count along with you',
    `tutor offer: ${JSON.stringify(shown)}`);
  await page.shot('offer-tutor');
  await page.evaluate(click('consent-cancel'));
  await until('the refusal', () => page.evaluate(`${decisions}.length === 1`));
  await sleep(400);
  let after = await page.evaluate(`({ open: document.getElementById('consent').open, decisions: ${decisions}, installs: window.__bridge.log.filter((c) => c.cmd === 'install_plugin').length })`);
  expect(!after.open && after.installs === 0 && JSON.stringify(after.decisions) === JSON.stringify([{ sessionId: 'L1', plugin: 'tally', installed: false }]),
    `after Cancel: ${JSON.stringify(after)}`);
  let errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // The plan lists it for the section reached: Install installs, then reports it.
  page = await openSession('plan');
  shown = await page.evaluate(consent);
  expect(shown.why === 'The plan uses this tool in the section you have reached.', `plan offer: ${JSON.stringify(shown)}`);
  await page.evaluate(click('consent-install'));
  await until('the decision', () => page.evaluate(`${decisions}.length === 1`));
  await sleep(400);
  after = await page.evaluate(`({ open: document.getElementById('consent').open, decisions: ${decisions},
    installs: window.__bridge.log.filter((c) => c.cmd === 'install_plugin').map((c) => c.args) })`);
  expect(!after.open && JSON.stringify(after.decisions) === JSON.stringify([{ sessionId: 'L1', plugin: 'tally', installed: true }])
    && after.installs.length === 1 && after.installs[0].id === 'tally' && after.installs[0].version === '0.3.0', `after Install: ${JSON.stringify(after)}`);
  errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // A built-in waiting for consent: the same screen, with its permissions; Enable turns it on.
  page = await openSession('builtin&builtins=1');
  const builtin = await page.evaluate(`({ title: document.getElementById('consent-title').textContent, why: document.getElementById('consent-why').textContent,
    perms: document.getElementById('consent-perms').textContent, button: document.getElementById('consent-install').textContent })`);
  expect(builtin.title === 'Enable Computer algebra?' && builtin.why === 'The tutor would like to use this tool: to check the algebra'
    && builtin.button === 'Enable' && builtin.perms !== 'No files, network or programs.', `built-in offer: ${JSON.stringify(builtin)}`);
  await page.shot('offer-builtin');
  await page.evaluate(click('consent-install'));
  await until('the decision', () => page.evaluate(`${decisions}.length === 1`));
  await sleep(400);
  after = await page.evaluate(`({ open: document.getElementById('consent').open, decisions: ${decisions},
    enabled: window.__bridge.log.filter((c) => c.cmd === 'set_plugin_enabled').map((c) => c.args),
    installs: window.__bridge.log.filter((c) => c.cmd === 'install_plugin').length })`);
  expect(!after.open && after.installs === 0 && JSON.stringify(after.enabled) === JSON.stringify([{ id: 'cas', enabled: true }])
    && JSON.stringify(after.decisions) === JSON.stringify([{ sessionId: 'L1', plugin: 'cas', installed: true }]), `after Enable: ${JSON.stringify(after)}`);
  errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('offer: an offer opens the consent screen with its reason (tutor, plan), for a registry plugin and for a built-in waiting for consent; Cancel, Install and Enable report the decision once and the screen stays closed');
}

// Built-in plugins in Settings → Plugins: never removable, labelled "built
// in", and one that declares permissions stays off until the consent screen
// naming them is accepted (the core records it; plugins/load.rs).
async function pluginsCase() {
  const page = await openApp('none', { query: '&builtins=1' });
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  await until('the plugin rows', () => page.evaluate(`document.querySelectorAll('#plugin-list .plugin').length === 5`));
  const row = (id) => `document.querySelector('#plugin-list .plugin[data-id="${id}"]')`;
  const buttons = (id) => page.evaluate(`[...${row(id)}.querySelectorAll('.actions button')].map((b) => b.textContent)`);
  const state = (id) => page.evaluate(`({ meta: ${row(id)}.querySelector('.meta').textContent, access: ${row(id)}.querySelectorAll('.meta')[1].textContent })`);
  const press = (id, label) => page.evaluate(`[...${row(id)}.querySelectorAll('.actions button')].find((b) => b.textContent === '${label}').click()`);
  const consent = `(() => { const d = document.getElementById('consent'); return { open: d.open, title: document.getElementById('consent-title').textContent, perms: document.getElementById('consent-perms').textContent, source: document.getElementById('consent-source').textContent, action: document.getElementById('consent-install').textContent }; })()`;

  // Built-ins carry no Remove; a package plugin still does.
  for (const id of ['code-parse', 'cas', 'live-search']) {
    const b = await buttons(id);
    expect(!b.includes('Remove'), `${id} (built in) offers Remove: ${b}`);
  }
  expect((await buttons('circuit')).includes('Remove'), 'a package plugin lost its Remove button');
  const parse = await state('code-parse');
  expect(/Enabled · built in/.test(parse.meta), `code-parse row: ${parse.meta}`);
  expect((await buttons('code-parse')).includes('Disable'), 'a permission-free built-in should start enabled');

  // A built-in that declares permissions starts off, naming them.
  const cas = await state('cas');
  expect(/Disabled · built in/.test(cas.meta) && /Run programs on your device/.test(cas.access), `cas row: ${JSON.stringify(cas)}`);
  expect((await buttons('cas')).includes('Enable'), 'cas should offer Enable');

  // Enable → the consent screen first; Cancel changes nothing.
  await press('cas', 'Enable');
  await until('the consent screen', () => page.evaluate(`document.getElementById('consent').open`));
  let c = await page.evaluate(consent);
  expect(/Enable Computer algebra/.test(c.title) && /Run programs on your device/.test(c.perms) && /Built in/.test(c.source) && c.action === 'Enable', `cas consent: ${JSON.stringify(c)}`);
  expect((await page.evaluate(argsOf('set_plugin_enabled'))).length === 0, 'enabling reached the core before consent');
  await page.evaluate(`document.getElementById('consent-cancel').click()`);
  await until('the screen closed', () => page.evaluate(`!document.getElementById('consent').open`));
  expect((await page.evaluate(argsOf('set_plugin_enabled'))).length === 0, 'Cancel still enabled the plugin');
  expect(/Disabled/.test((await state('cas')).meta) && (await buttons('cas')).includes('Enable'), 'Cancel changed the row');

  // Accept: exactly one enable call, the row turns on.
  await press('cas', 'Enable');
  await until('the consent screen again', () => page.evaluate(`document.getElementById('consent').open`));
  await page.evaluate(`document.getElementById('consent-install').click()`);
  await until('cas enabled', () => page.evaluate(`/Enabled · built in/.test(${row('cas')}?.querySelector('.meta')?.textContent ?? '')`));
  expect(JSON.stringify(await page.evaluate(argsOf('set_plugin_enabled'))) === JSON.stringify([{ id: 'cas', enabled: true }]), `enable calls: ${JSON.stringify(await page.evaluate(argsOf('set_plugin_enabled')))}`);
  expect((await buttons('cas')).includes('Disable'), 'cas should now offer Disable');

  // Turning one off needs no consent.
  await press('cas', 'Disable');
  await until('cas disabled', () => page.evaluate(`/Disabled · built in/.test(${row('cas')}?.querySelector('.meta')?.textContent ?? '')`));
  expect(!(await page.evaluate(`document.getElementById('consent').open`)), 'Disable asked for consent');

  // The network one names its own permission.
  await press('live-search', 'Enable');
  await until('live-search consent', () => page.evaluate(`document.getElementById('consent').open`));
  c = await page.evaluate(consent);
  expect(/Use the network/.test(c.perms) && !/programs/.test(c.perms), `live-search consent: ${JSON.stringify(c)}`);
  await page.evaluate(`document.getElementById('consent-cancel').click()`);

  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('plugins: built-ins have no Remove and read "built in"; one with permissions starts off, Enable shows the consent screen naming them (Cancel changes nothing), Disable needs none');
}

// --- practice ----------------------------------------------------------------

const PRACTICE = { query: '&fixture=learn&practice=1', reducedMotion: true };
const q = (sel) => `document.querySelector(${JSON.stringify(sel)})`;
const qa = (sel) => `[...document.querySelectorAll(${JSON.stringify(sel)})]`;
const textOf = (sel) => `(${q(sel)}?.textContent ?? '')`;
const shown = (sel) => `(() => { const e = ${q(sel)}; return Boolean(e && !e.hidden && e.getClientRects().length > 0); })()`;
const press = (label, scope = 'body') => `(() => { const b = ${qa(`${scope} button`)}.find((x) => x.textContent.trim() === ${JSON.stringify(label)} && !x.hidden); if (!b) throw new Error('no button ${label}'); b.click(); return true; })()`;
/** Chooses a format in a chooser by its label. */
const choose = (host, label) => `(() => { const l = ${qa(`#${host} label.mode`)}.find((x) => x.textContent.trim() === ${JSON.stringify(label)}); l.querySelector('input').click(); return l.querySelector('input').value; })()`;
const type = (value) => `(() => { const p = document.getElementById('prompt'); p.value = ${JSON.stringify(value)}; p.dispatchEvent(new Event('input')); return true; })()`;
const note = () => textOf('#practice-note');

async function practice() {
  const page = await openApp('none', PRACTICE);
  await until('Home', () => page.evaluate(shown('#start')));

  // Home: the review queue is one click away; the old Practice mode toggle is gone.
  await until('the review cue', () => page.evaluate(shown('#review-cue')));
  const home = await page.evaluate(`({ cue: ${textOf('#review-cue-text')}, toggle: Boolean(document.getElementById('mode-toggle')),
    hero: ${textOf('#hero')}, formats: ${qa('#review-cue-format input')}.map((r) => r.value), mapFormats: ${qa('#map-format input')}.map((r) => r.value) })`);
  expect(home.cue === '2 ideas are due: Chain rule, Limit of a function.' && !home.toggle && home.hero === 'What do you want to learn?'
    && home.formats.join() === 'stream,set' && home.mapFormats.join() === 'stream,set', `Home: ${JSON.stringify(home)}`);

  // Map: pick an idea, choose the format, Practise → start_practice with the idea's id.
  await page.evaluate(choose('review-cue-format', 'One at a time'));
  await page.evaluate(click('open-map'));
  await until('the map', () => page.evaluate(`Boolean(${q('#map-page .map-a11y input[data-key="chain"]')})`));
  await page.evaluate(`${q('#map-page .map-a11y input[data-key="chain"]')}.click()`);
  const desk = await page.evaluate(`({ count: ${textOf('#map-count')}, button: ${textOf('#map-practise')}, off: document.getElementById('map-practise').disabled,
    chosen: ${qa('#map-format input')}.find((r) => r.checked)?.value })`);
  expect(desk.count === '1 selected' && desk.button === 'Practise 1 idea' && !desk.off && desk.chosen === 'stream', `Map desk: ${JSON.stringify(desk)}`);
  await page.evaluate(click('map-practise'));
  await until('the Practice screen', () => page.evaluate(`${shown('#practice')} && document.getElementById('map').hidden`));
  expect(JSON.stringify(await page.evaluate(argsOf('start_practice'))) === JSON.stringify([{ targets: ['chain'], format: 'stream' }]), 'start_practice carries the picked idea and the format');
  const writing = await page.evaluate(`({ line: ${textOf('#practice-live .writing')}, stop: ${shown('#practice-stop')}, picked: ${textOf('#practice-picked')}, answer: ${shown('#composer')} })`);
  expect(writing.line === 'Writing a problem…' && writing.stop && writing.picked === 'On: Chain rule' && !writing.answer, `while the coach writes: ${JSON.stringify(writing)}`);

  // The problem: Markdown + KaTeX, the idea not named; the answer card with the confidence scale, Hint and Give up.
  await until('the problem', () => page.evaluate(shown('#practice-flow .problem')));
  await page.shot('practice-problem');
  const open = await page.evaluate(`(() => { const card = ${q('#practice-flow .problem')};
    return { kicker: card.querySelector('.kicker').textContent, math: card.querySelectorAll('.katex').length, named: card.textContent.includes('Chain rule') || ${textOf('#practice-flow')}.includes('Chain rule'),
      confidence: ${qa('#practice-answer .confidence input')}.map((r) => r.value), composerIn: Boolean(${q('#practice-answer #composer')}), focus: document.activeElement?.id,
      label: document.getElementById('prompt-label').textContent, buttons: ${qa('#practice-answer .actions button')}.map((b) => b.textContent), note: ${note()},
      primary: ${shown('#practice-primary')}, stop: ${shown('#practice-stop')}, send: document.getElementById('send').className }; })()`);
  expect(open.kicker === 'Problem 1' && open.math > 0 && !open.named && open.confidence.join() === '10,30,50,70,90' && open.composerIn && open.focus === 'prompt'
    && open.label === 'Your working or answer' && open.buttons.join('|') === 'Hint|I give up — show me' && open.note === 'Problem 1' && !open.primary && !open.stop
    && /primary/.test(open.send), `open problem: ${JSON.stringify(open)}`);

  // A hint: the count is shown, never a rung; the coach's reply arrives once (the streamed copy gives way to the feed).
  await page.evaluate(press('Hint', '#practice-answer'));
  await until('the hint reply', () => page.evaluate(`/Look at what sits inside/.test(${textOf('#practice-flow')})`));
  await until('the turn to end', () => page.evaluate(`!${shown('#practice-stop')} && !${q('#practice-live .entry')}`));
  const hint = await page.evaluate(`({ coach: document.querySelectorAll('#practice-flow .entry.tutor, #practice-live .entry.tutor').length, button: ${qa('#practice-answer .actions button')}[0].textContent,
      note: ${note()}, you: ${qa('#practice-flow .entry.user .you-text')}.map((e) => e.textContent), calls: ${argsOf('practice_hint')}.length })`);
  expect(hint.coach === 1 && hint.button === 'Hint (1 used)' && hint.note === 'Problem 1 · 1 hint used' && hint.you.join() === 'Asked for a hint.' && hint.calls === 1, `after a hint: ${JSON.stringify(hint)}`);

  // An answer with its confidence → feedback, the verdict, the revealed solution; Next is the one primary.
  // "+" is there in Practice, as in Learn: a picked file becomes a chip, is staged against this session and goes with the answer.
  expect(await page.evaluate(shown('#add-files')), '"+" is shown in Practice');
  await page.evaluate(click('add-files'));
  await until('the file chip', () => page.evaluate(`${qa('#attach-chips .chip')}.length === 1`));
  expect(JSON.stringify((await page.evaluate(argsOf('stage_file')))[0]) === JSON.stringify({ sessionId: 'P1', path: '/home/learner/worksheet.txt' }), 'the file is staged against the Practice session');
  await page.evaluate(type('The derivative is 6x cos(3x^2)'));
  await page.evaluate(`${q('#practice-answer .confidence input[value="70"]')}.click()`);
  await page.evaluate(click('send'));
  await until('the verdict', () => page.evaluate(shown('#practice-flow .concluded')));
  await until('the coach reply', () => page.evaluate(`/Yes\\. That is the answer/.test(${textOf('#practice-flow')})`));
  const sent = (await page.evaluate(argsOf('practice_submit')))[0];
  expect(sent.sessionId === 'P1' && sent.text === 'The derivative is 6x cos(3x^2)' && sent.confidence === 70 && sent.attachments?.length === 1 && sent.attachments[0].kind === 'file', `practice_submit: ${JSON.stringify(sent)}`);
  expect(JSON.stringify(await page.evaluate(`${qa('#practice-flow .entry.user .message-chips .chip-name')}.map((e) => e.textContent)`)) === JSON.stringify(['worksheet.txt']), 'the sent submission shows its attachment');
  await page.shot('practice-concluded');
  const done = await page.evaluate(`(() => { const c = ${q('#practice-flow .concluded')};
    return { verdict: c.querySelector('.verdict').textContent, meta: c.querySelector('.result-meta').textContent, open: c.querySelector('details.reveal').open,
      moves: c.querySelectorAll('.reveal-moves li').length, key: c.querySelectorAll('.reveal-key').length, answer: c.querySelector('.reveal-body').textContent.includes('6x'),
      math: c.querySelectorAll('.katex').length, conf: ${qa('#practice-flow .entry.user .you-meta')}.map((e) => e.textContent), kicker: ${textOf('#practice-answer .kicker')},
      scale: Boolean(${q('#practice-answer .confidence')}), focus: document.activeElement?.className, note: ${note()}, primary: ${textOf('#practice-primary')},
      secondary: ${textOf('#practice-secondary')}, send: document.getElementById('send').className }; })()`);
  expect(done.verdict === '✓ Correct.' && done.meta === 'Tested: Chain rule · 1 hint used · Rated good · 2 min' && done.open && done.moves === 2 && done.key === 1 && done.answer && done.math > 0
    && done.conf.join() === 'Fairly sure (≈70%)' && done.kicker === 'Ask about this problem' && !done.scale && /verdict/.test(done.focus)
    && done.note === 'Problem 1 done.' && done.primary === 'Next problem' && done.secondary === 'End practice' && /secondary/.test(done.send) && !/primary/.test(done.send),
    `concluded: ${JSON.stringify(done)}`);

  // A question after the verdict goes through the same composer.
  await page.evaluate(type('Why the inner derivative?'));
  await page.evaluate(click('send'));
  await until('the answer to the question', () => page.evaluate(`/Good question/.test(${textOf('#practice-flow')})`));
  const asked = (await page.evaluate(argsOf('practice_submit'))).at(-1);
  expect(asked.text === 'Why the inner derivative?' && asked.confidence === undefined, `a question carries no confidence: ${JSON.stringify(asked)}`);

  // Next → problem 2; Skip needs a second click and records a dismissal.
  await until('Next enabled', () => page.evaluate(`!document.getElementById('practice-primary').disabled`));
  await page.evaluate(click('practice-primary'));
  await until('problem 2', () => page.evaluate(`${qa('#practice-flow .problem')}.length === 2 && ${shown('#practice-answer .confidence')}`));
  expect((await page.evaluate(argsOf('practice_next'))).length === 1, 'practice_next once');
  await page.evaluate(press('I give up — show me', '#practice-answer'));
  const armed = await page.evaluate(`({ label: ${qa('#practice-answer .actions button')}[1].textContent, calls: ${argsOf('practice_dismiss')}.length })`);
  expect(armed.label === 'Sure? It counts as missed' && armed.calls === 0, `give up asks first: ${JSON.stringify(armed)}`);
  await page.evaluate(press('Sure? It counts as missed', '#practice-answer'));
  await until('the skipped problem', () => page.evaluate(`${qa('#practice-flow .concluded .verdict')}.map((v) => v.textContent.replace(/^\\S+ /, '')).join() === 'Correct.,Skipped.'`));
  await until('Next enabled', () => page.evaluate(`!document.getElementById('practice-primary').disabled`));

  // End: what was missed is retested first (wrap-up), then End finishes with a short summary.
  await page.evaluate(press('End practice', '.practice-desk'));
  await until('the retest offer', () => page.evaluate(`${note()} === 'Retesting what you missed: 1 left.'`));
  const wrap = await page.evaluate(`({ primary: ${textOf('#practice-primary')}, secondary: ${textOf('#practice-secondary')}, ends: ${argsOf('practice_end')} })`);
  expect(wrap.primary === 'Next retest' && wrap.secondary === 'Skip retests and end' && wrap.ends.length === 1 && wrap.ends[0].skipRetests === undefined, `wrap-up: ${JSON.stringify(wrap)}`);
  await page.evaluate(click('practice-primary'));
  await until('the retest', () => page.evaluate(`${qa('#practice-flow .problem')}.length === 3 && ${shown('#practice-answer .confidence')}`));
  await page.evaluate(type('The answer is 4'));
  await page.evaluate(click('send'));
  await until('the retest verdict', () => page.evaluate(`${qa('#practice-flow .concluded')}.length === 3`));
  await until('End offered', () => page.evaluate(`${note()} === 'Retests done.' && ${textOf('#practice-primary')} === 'End practice' && !document.getElementById('practice-primary').disabled`));
  const kickers = await page.evaluate(`${qa('#practice-flow .problem .kicker')}.map((k) => k.textContent)`);
  expect(kickers.join('|') === 'Problem 1|Problem 2|Problem 3 · Retest', `kickers: ${JSON.stringify(kickers)}`);
  await page.evaluate(click('practice-primary'));
  await until('the summary', () => page.evaluate(shown('#practice-flow .practice-summary')));
  await page.shot('practice-summary');
  const end = await page.evaluate(`({ title: ${textOf('.practice-summary .summary-title')}, line: ${textOf('.practice-summary .summary-line')}, ideas: ${textOf('.practice-summary .summary-ideas')},
      confidence: ${textOf('.practice-summary .calibration summary')}, note: ${note()}, primary: ${textOf('#practice-primary')}, secondary: ${textOf('#practice-secondary')},
      composer: document.getElementById('composer').hidden, answers: document.getElementById('practice-answer').childElementCount, word: ${textOf('#practice-word')}, ends: ${argsOf('practice_end')}.length })`);
  expect(end.title === 'Practice finished' && end.line === '3 problems: 2 correct, 1 skipped.' && end.ideas === 'Ideas: Chain rule' && end.confidence === 'Your confidence'
    && end.note === 'Practice ended.' && end.primary === 'Practise again' && end.secondary === 'Back to sessions' && end.composer && end.answers === 0
    && end.word === 'Practice · ended' && end.ends === 2, `ended: ${JSON.stringify(end)}`);

  // The session is in Home's list with its mode; opened again it is read-only.
  await page.evaluate(click('practice-back'));
  await until('Home', () => page.evaluate(shown('#start')));
  const row = await page.evaluate(`${qa('#session-list .session-open')}.map((b) => b.textContent)`);
  expect(row.some((t) => /Practice/.test(t)), `the session list: ${JSON.stringify(row)}`);
  await page.evaluate(`${qa('#session-list .session-open')}.find((b) => /Practice/.test(b.textContent)).click()`);
  await until('the ended session', () => page.evaluate(shown('#practice-flow .practice-summary')));
  const again = await page.evaluate(`({ composer: document.getElementById('composer').hidden, scale: Boolean(${q('#practice-answer .confidence')}), problems: ${qa('#practice-flow .problem')}.length,
      concluded: ${qa('#practice-flow .concluded')}.length, read: ${shown('#practice-primary')} })`);
  expect(again.composer && !again.scale && again.problems === 3 && again.concluded === 3 && again.read, `reopened: ${JSON.stringify(again)}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // A phone: nothing overflows and the controls are touch-sized.
  const phone = await openApp('none', { ...PRACTICE, mobile: true, width: 390, height: 844 });
  await until('the review cue', () => phone.evaluate(shown('#review-cue')));
  await phone.evaluate(choose('review-cue-format', 'One at a time'));
  await phone.evaluate(click('practise-due'));
  await until('the problem', () => phone.evaluate(shown('#practice-flow .problem') + ` && ${shown('#practice-answer .confidence')}`));
  const small = await phone.evaluate(`(() => { const s = document.getElementById('practice-scroll'); const box = (sel) => ${q('#practice')}.querySelector(sel).getBoundingClientRect();
    return { overflow: s.scrollWidth > s.clientWidth, doc: document.documentElement.scrollWidth > innerWidth, send: box('#send').height, hint: ${qa('#practice-answer .actions button')}[0].getBoundingClientRect().height,
      seg: ${qa('#practice-answer .seg')}[0].getBoundingClientRect().height, back: box('#practice-back').height }; })()`);
  expect(!small.overflow && !small.doc && small.send >= 44 && small.hint >= 44 && small.seg >= 44 && small.back >= 44, `phone layout: ${JSON.stringify(small)}`);
  await phone.shot('practice-phone');
  await phone.close();
  console.log('practice: Home cue + format chooser (no mode toggle), Map → start_practice, problem without the idea named, hint count, answer + confidence → verdict + revealed solution, question after it, give up (asks twice), End → retest → summary, session list, read-only reopen, phone layout');
}

async function practiceSet() {
  const page = await openApp('none', PRACTICE);
  await until('the review cue', () => page.evaluate(shown('#review-cue')));
  await page.evaluate(choose('review-cue-format', 'A set of problems'));
  await page.evaluate(click('practise-due'));
  await until('the set screen', () => page.evaluate(shown('#practice')));
  expect(JSON.stringify(await page.evaluate(argsOf('start_practice'))) === JSON.stringify([{ targets: [], format: 'set' }]), 'start_practice carries the set format');
  await until('the writing line', () => page.evaluate(`${textOf('#practice-live .writing')} === 'Writing the set…'`));
  await until('every problem', () => page.evaluate(`${qa('#practice-flow .problem')}.length === 3`));

  // Open set: all statements, one answer box and confidence each; nothing else to act on.
  const listed = await page.evaluate(`({ kickers: ${qa('#practice-flow .problem .kicker')}.map((k) => k.textContent), boxes: ${qa('#practice-flow textarea')}.length,
      scales: ${qa('#practice-flow .confidence')}.length, composer: ${shown('#composer')}, hint: Boolean(${qa('button')}.find((b) => /^Hint/.test(b.textContent))),
      note: ${note()}, primary: ${textOf('#practice-primary')}, secondary: ${shown('#practice-secondary')}, focus: document.activeElement?.tagName,
      labels: ${qa('#practice-flow textarea')}.map((t) => document.querySelector('label[for="' + t.id + '"]')?.textContent) })`);
  expect(listed.kickers.join('|') === 'Problem 1 of 3|Problem 2 of 3|Problem 3 of 3' && listed.boxes === 3 && listed.scales === 3 && !listed.composer && !listed.hint
    && listed.note === '0 of 3 answered. Unanswered ones count as “don’t know”.' && listed.primary === 'Hand in the set' && !listed.secondary && listed.focus === 'TEXTAREA'
    && listed.labels.join('|') === 'Your answer to problem 1|Your answer to problem 2|Your answer to problem 3', `set listed: ${JSON.stringify(listed)}`);
  await page.shot('practice-set-open');

  // Answers are held locally: no command, no feedback, and they survive leaving the screen.
  const fillSet = (i, value) => `(() => { const t = ${qa('#practice-flow textarea')}[${i}]; t.value = ${JSON.stringify(value)}; t.dispatchEvent(new Event('input')); return true; })()`;
  await page.evaluate(fillSet(0, 'It is 6x cos(3x^2)'));
  await page.evaluate(`${qa('#practice-flow .exercise')}[0].querySelector('.confidence input[value="70"]').click()`);
  await page.evaluate(fillSet(1, 'wrong: I got 6x'));
  const held = await page.evaluate(`({ note: ${note()}, submitted: ${argsOf('practice_submit_set')}.length, other: ${argsOf('practice_submit')}.length + ${argsOf('practice_hint')}.length,
      feedback: document.querySelectorAll('#practice-flow .verdict, #practice-flow .entry.tutor, #practice-flow .reveal').length })`);
  expect(held.note === '2 of 3 answered. Unanswered ones count as “don’t know”.' && held.submitted === 0 && held.other === 0 && held.feedback === 0, `answers held: ${JSON.stringify(held)}`);
  await page.evaluate(click('practice-back'));
  await until('Home', () => page.evaluate(shown('#start')));
  await page.evaluate(`${qa('#session-list .session-open')}.find((b) => /Practice/.test(b.textContent)).click()`);
  await until('the set again', () => page.evaluate(`${qa('#practice-flow textarea')}.length === 3`));
  const back = await page.evaluate(`({ boxes: ${qa('#practice-flow textarea')}.map((t) => t.value), conf: ${qa('#practice-flow .confidence')}.map((f) => f.querySelector('input:checked')?.value ?? null), note: ${note()} })`);
  expect(back.boxes.join('|') === 'It is 6x cos(3x^2)|wrong: I got 6x|' && back.conf.join() === '70,,' && back.note === '2 of 3 answered. Unanswered ones count as “don’t know”.', `answers after reopening: ${JSON.stringify(back)}`);

  // Hand in: one command with the answers given; then the review, problem by problem.
  await page.evaluate(click('practice-primary'));
  await until('the review', () => page.evaluate(`${qa('#practice-flow .result')}.length === 3 && /Review: problem 1 is right/.test(${textOf('#practice-flow')})`));
  const handed = await page.evaluate(argsOf('practice_submit_set'));
  expect(handed.length === 1 && JSON.stringify(handed[0].answers) === JSON.stringify([
    { problemId: 'p1', text: 'It is 6x cos(3x^2)', confidence: 70 }, { problemId: 'p2', text: 'wrong: I got 6x' },
  ]), `practice_submit_set: ${JSON.stringify(handed)}`);
  await page.shot('practice-set-review');
  const review = await page.evaluate(`(() => { const cards = ${qa('#practice-flow .problem')};
    return { verdicts: cards.map((c) => c.querySelector('.verdict')?.textContent.replace(/^\\S+ /, '')), you: cards.map((c) => c.querySelector('.you .you-text')?.textContent), conf: cards.map((c) => c.querySelector('.you .you-meta')?.textContent ?? null),
      miss: cards.map((c) => c.querySelector('.result-miss')?.textContent ?? null), reveals: cards.map((c) => Boolean(c.querySelector('details.reveal'))),
      boxes: ${qa('#practice-flow textarea')}.length, coach: ${qa('#practice-flow .entry.tutor')}.length, composer: ${shown('#composer')}, kicker: ${textOf('#practice-answer .kicker')},
      note: ${note()}, primary: ${textOf('#practice-primary')}, send: document.getElementById('send').className, status: document.getElementById('practice-word').textContent }; })()`);
  expect(review.verdicts.join('|') === 'Correct.|Not quite.|Don’t know.' && review.you.join('|') === 'It is 6x cos(3x^2)|wrong: I got 6x|No answer.'
    && review.conf[0] === 'Fairly sure (≈70%)' && review.miss[1] === 'What went wrong: Right idea, slip in the working.' && review.miss[0] === null && review.reveals.every(Boolean)
    && review.boxes === 0 && review.coach === 1 && review.composer && review.kicker === 'Ask about this review' && review.note === 'Review.' && review.primary === 'End practice'
    && /secondary/.test(review.send) && review.status === 'Practice · set', `set review: ${JSON.stringify(review)}`);

  // The review survives a reopen (the answers come from the op log), then End closes it.
  await page.evaluate(click('practice-back'));
  await until('Home', () => page.evaluate(shown('#start')));
  await page.evaluate(`${qa('#session-list .session-open')}.find((b) => /Practice/.test(b.textContent)).click()`);
  await until('the review again', () => page.evaluate(`${qa('#practice-flow .result')}.length === 3`));
  const kept = await page.evaluate(`${qa('#practice-flow .problem .you .you-text')}.map((e) => e.textContent).join('|')`);
  expect(kept === 'It is 6x cos(3x^2)|wrong: I got 6x|No answer.', `answers read back from the log: ${kept}`);
  await page.evaluate(click('practice-primary'));
  await until('the summary', () => page.evaluate(shown('#practice-flow .practice-summary')));
  const line = await page.evaluate(textOf('.practice-summary .summary-line'));
  expect(line === '3 problems: 1 correct, 1 not quite, 1 don’t know.', `set summary: ${line}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('practice-set: start from Home, every problem listed with its own answer box + confidence, answers held locally (no command, no feedback, survive leaving), one practice_submit_set, per-problem review (verdict, miss class, answer, solution), review survives a reopen, summary');
}

// The backend's turn status and the session's own state decide what the screen offers.
async function practiceResume() {
  const page = await openApp('none', { query: '&fixture=learn&practice=1&practiceSeed=failed,owed,cut,legacy', reducedMotion: true });
  await until('the session list', () => page.evaluate(`${qa('#session-list .session-open')}.length === 5`));
  const openRow = (title) => `${qa('#session-list .session-open')}.find((b) => b.textContent.includes(${JSON.stringify(title)})).click()`;
  const back = async () => {
    await page.evaluate(click('practice-back'));
    await until('Home', () => page.evaluate(shown('#start')));
  };

  // A failed last turn: the backend reason, and Try again → practice_resume.
  await page.evaluate(openRow('failed turn'));
  await until('the failure', () => page.evaluate(`/DeepSeek is not connected/.test(${textOf('#practice-live .turn-error')})`));
  const failed = await page.evaluate(`({ title: ${textOf('#practice-live .turn-error-title')}, buttons: ${qa('#practice-live .turn-error button')}.map((b) => b.textContent), role: ${q('#practice-live .turn-error')}.getAttribute('role') })`);
  expect(failed.title === 'The last reply failed.' && failed.buttons.join() === 'Try again,Settings' && failed.role === 'alert', `failed turn: ${JSON.stringify(failed)}`);
  await page.evaluate(press('Try again', '#practice-live'));
  await until('the retried reply', () => page.evaluate(`/Sorry about that/.test(${textOf('#practice-flow')}) && !${q('#practice-live .turn-error')}`));
  expect((await page.evaluate(argsOf('practice_resume'))).length === 1, 'practice_resume after Try again');
  await back();

  // A learner message with no reply (a restart cut the turn): offered, not guessed from the page.
  await page.evaluate(openRow('reply owed'));
  await until('the offer', () => page.evaluate(`${textOf('#practice-live .turn-error-title')} === 'The coach hasn’t answered this yet.'`));
  expect(await page.evaluate(`${q('#practice-live .turn-error')}.getAttribute('role') === 'status' && ${textOf('#practice-live .turn-error button')} === 'Get a reply'`), 'the offer is a status with "Get a reply"');
  await back();

  // A problem that was never written: the same offer; it writes the problem.
  await page.evaluate(openRow('problem not written'));
  await until('the offer', () => page.evaluate(`${textOf('#practice-live .turn-error button')} === 'Get a reply'`));
  expect(await page.evaluate(`${q('#practice-flow .problem')} === null`), 'nothing is readable before the coach has posed the problem');
  await page.evaluate(press('Get a reply', '#practice-live'));
  await until('the problem', () => page.evaluate(shown('#practice-flow .problem') + ` && !${q('#practice-live .turn-error')}`));
  await back();

  // The old single-prompt Practice: read-only, with "Start a new practice session".
  await page.evaluate(openRow('older session'));
  await until('the transcript', () => page.evaluate(`${qa('#practice-flow .entry')}.length === 2`));
  const old = await page.evaluate(`({ word: ${textOf('#practice-word')}, note: ${note()}, primary: ${textOf('#practice-primary')}, composer: document.getElementById('composer').hidden,
      answer: document.getElementById('practice-answer').childElementCount, math: ${qa('#practice-flow .katex')}.length > 0 })`);
  expect(old.word === 'Read-only' && old.note === 'Made with an older tutor. Read-only.' && old.primary === 'Start a new practice session' && old.composer && old.answer === 0 && old.math, `legacy: ${JSON.stringify(old)}`);
  await page.evaluate(click('practice-primary'));
  await until('a new session', () => page.evaluate(`${shown('#practice-flow .problem')}`));
  const started = await page.evaluate(argsOf('start_practice'));
  expect(started.length === 1 && started[0].targets.length === 0 && ['stream', 'set'].includes(started[0].format), `start_practice from a legacy session: ${JSON.stringify(started)}`);
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // A start whose first turn fails after the session exists: the screen is open with the error and a way on.
  const failing = await openApp('none', { query: '&fixture=learn&practice=1&practiceFail=start', reducedMotion: true });
  await until('the review cue', () => failing.evaluate(shown('#review-cue')));
  await failing.evaluate(choose('review-cue-format', 'One at a time'));
  await failing.evaluate(click('practise-due'));
  await until('the failure', () => failing.evaluate(`/DeepSeek is not connected/.test(${textOf('#practice-live .turn-error')})`));
  expect(await failing.evaluate(shown('#practice')), 'the session screen stays open on a failed first turn');
  await failing.evaluate(press('Try again', '#practice-live'));
  await until('the problem', () => failing.evaluate(shown('#practice-flow .problem')));
  await failing.close();
  console.log('practice-resume: a failed turn offers Try again with the backend reason, an unanswered message and an unwritten problem offer "Get a reply" (practice_resume), an older session opens read-only with "Start a new practice session", a failed first turn keeps the screen open');
}

// --- exam mode ------------------------------------------------------------------

const EXAM = { query: '&fixture=learn&practice=1&exam=1', reducedMotion: true };
const noOverflow = `document.documentElement.scrollWidth <= innerWidth`;

/** Opens the exam goal's Learn session (L1) and its dashboard. */
async function openDashboard(page) {
  await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
  await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
  await until('the Exam button', () => page.evaluate(shown('#exam-toggle')));
  await page.evaluate(click('exam-toggle'));
  await until('the dashboard', () => page.evaluate(`document.getElementById('exam-panel').open && ${qa('#exam-panel .exam-section')}.length === 7`));
}

async function exam() {
  const page = await openApp('none', EXAM);
  await openDashboard(page);

  // The dashboard: days left, today's sitting, the estimate (labelled), coverage, ideas in priority order.
  const dash = await page.evaluate(`(() => ({
    title: ${textOf('#exam-heading')}, days: ${textOf('.exam-days')}, focus: document.activeElement?.id, calls: window.__bridge.log.filter((c) => c.cmd === 'exam_plan').map((c) => c.args),
    today: ${textOf('.exam-today-lead')}, sections: ${qa('#exam-panel .exam-section > h2')}.map((x) => x.textContent),
    label: ${textOf('.exam-label')}, figures: ${qa('.exam-figure')}.map((x) => x.textContent), note: ${textOf('.exam-estimate .help')},
    raise: ${qa('.exam-estimate .exam-list li')}.map((x) => x.textContent),
    bars: ${qa('.exam-meter')}.map((m) => m.getAttribute('aria-label')), shares: ${qa('.exam-bar-label')}.map((x) => x.textContent),
    ideas: ${qa('.exam-idea-row .exam-idea')}.map((x) => x.textContent), chips: ${qa('.exam-chip')}.map((x) => x.textContent),
    terms: ${qa('.exam-idea-row .you-meta')}[0].textContent, unmapped: ${qa('.exam-ideas ~ .help')}[0].textContent,
    papers: ${qa('.exam-section')}[5].textContent, budget: document.querySelector('.exam-start input').value,
    help: ${textOf('.exam-start .help')}, go: ${q('.exam-start button[type=submit]')}.disabled, overflow: !(${noOverflow}),
  }))()`);
  expect(dash.title === 'MATH101 final' && /^10 days left$/.test(dash.days) && dash.focus === 'exam-heading' && JSON.stringify(dash.calls) === JSON.stringify([{ sessionId: 'L1' }]),
    `dashboard head: ${JSON.stringify(dash)}`);
  expect(dash.sections.join('|') === 'Today|Expected score|Coverage by topic|Ideas in priority order|Teaching and practice|Past papers|Sit a timed paper', `sections: ${dash.sections}`);
  expect(dash.today === 'Teach Derivative as a limit.' && dash.label === 'Estimate' && dash.figures.join('|') === '≈ 46%|≈ 33%' && Boolean(dash.note)
    && dash.raise[0].startsWith('Derivative as a limit · Teach · about +7.1 points'), `today and estimate: ${JSON.stringify(dash)}`);
  expect(dash.bars.length === 2 && /^Calculus: expected 60% now, 43% on the exam day, 88% of the exam$/.test(dash.bars[0]) && /^Algebra: expected 0% now/.test(dash.bars[1]), `coverage bars: ${JSON.stringify(dash.bars)}`);
  expect(dash.ideas.join('|') === 'Derivative as a limit|Quadratic formula|Chain rule|Limit of a function' && dash.chips.join('|') === 'Teach|Practise|Practise|Practise'
    && dash.terms === 'weight 0.80 · recall 50% · 3 layers not shown · priority 1.60', `ideas: ${JSON.stringify({ i: dash.ideas, c: dash.chips, t: dash.terms })}`);
  expect(/Series/.test(dash.unmapped) && /1 past paper in your libraries, 1 with a mark scheme/.test(dash.papers)
    && dash.budget === '45' && /^About 3 problems, 15 minutes each/.test(dash.help) && /can’t be paused/.test(dash.help) && !dash.go && !dash.overflow, `papers and budget: ${JSON.stringify(dash)}`);
  await page.shot('exam-dashboard');

  // A budget outside the limits stops the start.
  await page.evaluate(fill2('.exam-start input', '3'));
  const bad = await page.evaluate(`({ help: ${textOf('.exam-start .help')}, go: ${q('.exam-start button[type=submit]')}.disabled })`);
  expect(bad.help === 'Between 5 and 360 minutes.' && bad.go, `bad budget: ${JSON.stringify(bad)}`);
  await page.evaluate(fill2('.exam-start input', '45'));

  // Start: the paper opens on the Practice screen and the clock starts when the whole paper is posed.
  await page.evaluate(`${q('.exam-start button[type=submit]')}.click()`);
  await until('the paper screen', () => page.evaluate(shown('#practice')));
  await until('the clock', () => page.evaluate(shown('.exam-clock')));
  const started = await page.evaluate(argsOf('start_exam'));
  expect(started.length === 1 && JSON.stringify(started[0]) === JSON.stringify({ goalSessionId: 'L1', budgetMin: 45 }) && (await page.evaluate(argsOf('start_practice'))).length === 0, `start_exam: ${JSON.stringify(started)}`);
  const paper = await page.evaluate(`(() => ({
    word: ${textOf('#practice-word')}, clock: ${textOf('.exam-clock')}, role: ${q('.exam-clock')}.getAttribute('role'), label: ${textOf('.exam-clock-label')},
    kickers: ${qa('.exam-paper .problem .kicker')}.map((k) => k.textContent), boxes: ${qa('.exam-paper textarea')}.length, scales: ${qa('.exam-paper .confidence')}.length,
    labels: ${qa('.exam-paper textarea')}.map((t) => document.querySelector('label[for="' + t.id + '"]')?.textContent),
    buttons: ${qa('#practice button')}.filter((b) => !b.hidden && b.getClientRects().length).map((b) => b.textContent.trim()),
    flow: ${q('#practice-flow')}.childElementCount, note: ${note()}, composer: ${shown('#composer')}, panel: document.getElementById('exam-panel').open, toggle: ${shown('#exam-toggle')},
    count: ${textOf('.exam-count')}, focus: document.activeElement?.tagName, bar: getComputedStyle(${q('.exam-bar')}).position, overflow: !(${noOverflow}),
  }))()`);
  expect(paper.word === 'Practice · exam paper' && /^(44:5\d|45:00)$/.test(paper.clock) && paper.role === 'timer' && paper.label === 'left of 45 minutes · 15 marks',
    `paper clock: ${JSON.stringify(paper)}`);
  expect(paper.kickers.join('|') === 'Problem 1 of 3 · 5 marks|Problem 2 of 3 · 4 marks|Problem 3 of 3 · 6 marks' && paper.boxes === 3 && paper.scales === 3
    && paper.labels.join('|') === 'Your answer to problem 1|Your answer to problem 2|Your answer to problem 3', `paper problems: ${JSON.stringify(paper)}`);
  // No hints, no skipping, no Next, no feedback; the clock bar is the one thing that sticks.
  expect(paper.buttons.every((b) => !/Hint|give up|Next|End practice|Stop/.test(b)) && paper.buttons.includes('Hand in the paper') && paper.flow === 0 && paper.note === ''
    && !paper.composer && !paper.panel && !paper.toggle && paper.count === '0 of 3 answered.' && paper.focus === 'TEXTAREA' && paper.bar === 'sticky' && !paper.overflow, `paper controls: ${JSON.stringify(paper)}`);
  const refused = await page.evaluate(`window.__TAURI__.core.invoke('practice_hint', { sessionId: 'X1' }).then(() => 'allowed', (e) => String(e?.message ?? e))`);
  expect(/no hints/.test(refused), `a hint on a paper: ${refused}`);
  await page.shot('exam-paper');

  // Typing is saved by the app (the deadline hands in the latest save).
  const fillAnswer = (i, value) => `(() => { const t = ${qa('.exam-paper textarea')}[${i}]; t.value = ${JSON.stringify(value)}; t.dispatchEvent(new Event('input')); return true; })()`;
  await page.evaluate(fillAnswer(0, 'It is 6x cos(3x^2)'));
  await page.evaluate(`${qa('.exam-paper .exercise')}[0].querySelector('.confidence input[value="70"]').click()`);
  await page.evaluate(fillAnswer(1, 'wrong: I got 6x'));
  await until('the draft save', () => page.evaluate(`${argsOf('practice_exam_save')}.length > 0 && ${textOf('.exam-saved')} === 'Saved.'`));
  const saved = (await page.evaluate(argsOf('practice_exam_save'))).at(-1);
  expect(JSON.stringify(saved) === JSON.stringify({ sessionId: 'X1', answers: [
    { problemId: 'p1', text: 'It is 6x cos(3x^2)', confidence: 70 }, { problemId: 'p2', text: 'wrong: I got 6x' },
  ] }) && (await page.evaluate(textOf('.exam-count'))) === '2 of 3 answered.', `practice_exam_save: ${JSON.stringify(saved)}`);

  // Leaving and coming back: the saved answers return and the clock kept running.
  await page.evaluate(click('practice-back'));
  await until('Home', () => page.evaluate(shown('#start')));
  await page.evaluate(`${qa('#session-list .session-open')}.find((b) => /Exam paper/.test(b.textContent)).click()`);
  await until('the paper again', () => page.evaluate(`${qa('.exam-paper textarea')}.length === 3`));
  const back = await page.evaluate(`({ boxes: ${qa('.exam-paper textarea')}.map((t) => t.value), conf: ${qa('.exam-paper .confidence')}.map((f) => f.querySelector('input:checked')?.value ?? null), clock: ${textOf('.exam-clock')} })`);
  expect(back.boxes.join('|') === 'It is 6x cos(3x^2)|wrong: I got 6x|' && back.conf.join() === '70,,' && /^(44:|45:)/.test(back.clock), `paper after reopening: ${JSON.stringify(back)}`);

  // Handing in with a problem unanswered asks first.
  await page.evaluate(press('Hand in the paper', '#practice'));
  await untilDump('the first confirmation', () => page.evaluate(`document.getElementById('confirm').open`), page);
  const ask = await page.evaluate(`({ title: ${textOf('#confirm-title')}, text: ${textOf('#confirm-text')}, ok: ${textOf('#confirm-ok')}, cancel: ${textOf('#confirm-cancel')}, calls: ${argsOf('practice_submit_set')}.length })`);
  expect(ask.title === 'Hand in the paper?' && /1 problem has no answer/.test(ask.text) && ask.ok === 'Hand in' && ask.cancel === 'Keep working' && ask.calls === 0, `hand-in confirmation: ${JSON.stringify(ask)}`);
  await page.evaluate(click('confirm-cancel'));
  await until('the dialog closed', () => page.evaluate(`!document.getElementById('confirm').open`));
  expect((await page.evaluate(argsOf('practice_submit_set'))).length === 0 && (await page.evaluate(`${qa('.exam-paper textarea')}.length === 3`)), 'Keep working keeps the paper open');
  // A state refresh landing right after the dialog closed can swallow one
  // click, as it can for a learner: press again until the dialog opens.
  let pressed = 0;
  await untilDump('the second confirmation', async () => {
    if (await page.evaluate(`document.getElementById('confirm').open`)) return true;
    if (pressed++ < 6) await page.evaluate(press('Hand in the paper', '#practice'));
    await sleep(250);
    return page.evaluate(`document.getElementById('confirm').open`);
  }, page);
  await page.evaluate(click('confirm-ok'));
  try {
    await until('the marked paper', () => page.evaluate(`${qa('.exam-paper .result')}.length === 3 && ${shown('.exam-score-line')} && ${qa('.exam-paper .entry.tutor')}.length === 1 && Boolean(${qa('.exam-actions button')}.find((b) => b.textContent === 'Finish' && !b.disabled))`));
  } catch (err) {
    const snap = await page.evaluate(`({ results: ${qa('.exam-paper .result')}.length, score: ${shown('.exam-score-line')}, tutor: ${qa('.exam-paper .entry.tutor')}.length, actions: ${qa('.exam-actions button')}.map((b) => b.textContent + (b.disabled ? ' (disabled)' : '')), status: document.querySelector('.exam-paper .form-status, .exam-status')?.textContent, errs: window.__errors, tail: window.__bridge.log.slice(-8).map((c) => c.cmd) })`);
    throw new Error(`${err.message}: ${JSON.stringify(snap)}`);
  }
  const handed = await page.evaluate(argsOf('practice_submit_set'));
  expect(handed.length === 1 && JSON.stringify(handed[0]) === JSON.stringify({ sessionId: 'X1', answers: [
    { problemId: 'p1', text: 'It is 6x cos(3x^2)', confidence: 70 }, { problemId: 'p2', text: 'wrong: I got 6x' },
  ] }), `practice_submit_set: ${JSON.stringify(handed)}`);
  await page.shot('exam-review');
  const review = await page.evaluate(`(() => { const cards = ${qa('.exam-paper .problem')};
    return { score: ${textOf('.exam-score-line')}, when: ${textOf('.exam-score .you-meta')}, verdicts: cards.map((c) => c.querySelector('.verdict')?.textContent.replace(/^\\S+ /, '')),
      marks: cards.map((c) => c.querySelector('.exam-marks')?.textContent), you: cards.map((c) => c.querySelector('.you .you-text')?.textContent),
      conf: cards.map((c) => c.querySelector('.you .you-meta')?.textContent ?? null), miss: cards.map((c) => c.querySelector('.result-miss')?.textContent ?? null),
      scheme: cards.map((c) => c.querySelector('details.reveal')?.textContent.includes('Names the outer and the inner function')), boxes: ${qa('.exam-paper textarea')}.length,
      clock: ${shown('.exam-clock')}, coach: ${qa('.exam-paper .entry.tutor')}.length, buttons: ${qa('.exam-actions button')}.map((b) => b.textContent), word: ${textOf('#practice-word')} };
  })()`);
  expect(review.score === '7 of 15 marks (47%)' && /^Handed in after \d+ of 45 minutes\.$/.test(review.when) && review.verdicts.join('|') === 'Correct.|Partly right.|Don’t know.'
    && review.marks.join('|') === '5 of 5 marks|2 of 4 marks|0 of 6 marks' && review.you.join('|') === 'It is 6x cos(3x^2)|wrong: I got 6x|No answer.' && review.conf[0] === 'Fairly sure (≈70%)'
    && review.miss[1] === 'What went wrong: Right idea, slip in the working.' && review.scheme.every(Boolean) && review.boxes === 0 && !review.clock && review.coach === 1
    && review.buttons.join() === 'Finish', `review: ${JSON.stringify(review)}`);
  await page.evaluate(press('Finish', '#practice'));
  await until('the paper ended', () => page.evaluate(`${qa('.exam-actions button')}.map((b) => b.textContent).join() === 'Back to sessions'`));
  await page.evaluate(press('Back to sessions', '#practice'));
  await until('Home', () => page.evaluate(shown('#start')));
  let errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // The deadline: the clock reaches 0, the boxes lock, and the app hands in what was saved.
  const late = await openApp('none', { ...EXAM, query: `${EXAM.query}&examShortMs=3500` });
  await openDashboard(late);
  await late.evaluate(`${q('.exam-start button[type=submit]')}.click()`);
  await until('the clock', () => late.evaluate(shown('.exam-clock')));
  await late.evaluate(`(() => { const t = ${qa('.exam-paper textarea')}[0]; t.value = 'Auto answer'; t.dispatchEvent(new Event('input')); return true; })()`);
  await until('the draft save', () => late.evaluate(`${argsOf('practice_exam_save')}.length > 0`));
  await until('time up', () => late.evaluate(`${textOf('.exam-clock')} === '0:00' && ${q('.exam-paper textarea')}.disabled`));
  const lock = await late.evaluate(`({ status: ${textOf('.exam-status')}, boxes: ${qa('.exam-paper textarea')}.every((t) => t.disabled), hand: ${q('.exam-bar .btn')}.disabled, low: ${q('.exam-bar')}.classList.contains('low') })`);
  expect(/^Time is up\. Your saved answers are being handed in\.$/.test(lock.status) && lock.boxes && lock.hand && lock.low, `locked at the deadline: ${JSON.stringify(lock)}`);
  await until('the auto hand-in', () => late.evaluate(`${qa('.exam-paper .result')}.length === 3 && ${shown('.exam-score-line')}`), 20_000);
  const auto = await late.evaluate(`({ score: ${textOf('.exam-score-line')}, when: ${textOf('.exam-score .you-meta')}, you: ${qa('.exam-paper .problem .you .you-text')}.map((e) => e.textContent),
      handIn: ${argsOf('practice_submit_set')}.length, saved: ${argsOf('practice_exam_save')}.at(-1).answers })`);
  expect(auto.score === '5 of 15 marks (33%)' && auto.when === 'Time ran out: the app handed in your last saved answers.' && auto.you.join('|') === 'Auto answer|No answer.|No answer.'
    && auto.handIn === 0 && JSON.stringify(auto.saved) === JSON.stringify([{ problemId: 'p1', text: 'Auto answer' }]), `auto hand-in: ${JSON.stringify(auto)}`);
  errors = await late.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await late.close();

  // A phone: the dashboard and the paper fit, the clock bar stacks.
  const phone = await openApp('none', { ...EXAM, width: 390, height: 844, mobile: true, dark: true });
  await openDashboard(phone);
  expect(await phone.evaluate(noOverflow), 'the dashboard overflows a 390 px screen');
  await phone.shot('exam-dashboard-phone');
  await phone.evaluate(`${q('.exam-start button[type=submit]')}.click()`);
  await until('the clock', () => phone.evaluate(shown('.exam-clock')));
  expect(await phone.evaluate(`${noOverflow} && ${q('.exam-bar .btn')}.getBoundingClientRect().width > 300`), 'the paper overflows a 390 px screen, or its hand-in button is not full width');
  await phone.shot('exam-paper-phone');
  errors = await phone.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await phone.close();
  console.log('exam: dashboard (days left, today, labelled estimate, coverage bars, ideas by priority, papers, budget limits), start_exam, timed paper (clock, marks, no hints/skip/next, autosave, draft survives a reopen), hand-in confirmation, marked review with mark scheme and score, deadline lock and the app handing in the saved draft, phone layout');
}
const fill2 = (sel, value) => `(() => { const el = ${q(sel)}; el.value = ${JSON.stringify(value)}; el.dispatchEvent(new Event('input')); return true; })()`;


// The scratch pad and the Pen settings tab (ui/scratchpad.js, ui/pen.js):
// a session's pad docks beside the lesson on a wide screen and is a full
// overlay on a narrow one, a phone has no pad, strokes / eraser / undo /
// clear, the pen's buttons, zoom (buttons, Ctrl+wheel, a pinch, fit) and
// panning, a figure as the background, a drawing sent with the reasoning
// and the answer (and with a message) as its bounding box with a margin,
// the overlay showing more board at the same scale, kept per session.
async function pad() {
  const openSession = async (opts = {}) => {
    const page = await openApp('none', { query: '&fixture=learn&reading=predict&padFigure=1', ...opts });
    await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
    await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
    await until('the session', () => page.evaluate(`Boolean(document.querySelector('#timeline .debrief'))`));
    await page.evaluate(`window.__bridge.deliverReading()`);
    await until('the question card', () => page.evaluate(`Boolean(document.querySelector('#timeline .exercise.pending .ex-form'))`));
    return page;
  };
  const PIXELS = `(() => { const c = document.getElementById('pad-canvas'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++; return n; })()`;
  const geometry = `(() => { const r = (id) => { const b = document.getElementById(id).getBoundingClientRect(); return [b.left, b.top, b.width, b.height].map(Math.round); };
    return { mode: document.getElementById('pad').dataset.mode, docked: document.getElementById('conversation').classList.contains('pad-docked'),
      pad: r('pad'), messages: r('messages'), stage: r('pad-stage'), board: r('pad-board'), modeShown: !document.getElementById('pad-mode').hidden, vw: innerWidth, vh: innerHeight }; })()`;
  const draw = async (page, points) => {
    const [x, y, w, h] = await page.evaluate(`(() => { const b = document.getElementById('pad-canvas').getBoundingClientRect(); return [b.left, b.top, b.width, b.height]; })()`);
    const at = ([fx, fy]) => [x + fx * w, y + fy * h];
    const [x0, y0] = at(points[0]);
    await page.mouse('mouseMoved', x0, y0);
    await page.mouse('mousePressed', x0, y0, 1);
    for (const p of points.slice(1)) await page.mouse('mouseMoved', ...at(p), 1);
    await page.mouse('mouseReleased', ...at(points.at(-1)), 0);
  };
  /** A synthetic pen event (the headless browser has no pen); `at` is a fraction of the canvas. */
  const penEvent = (target, type, buttons, at = [0.5, 0.5]) => `(() => { const el = ${target}; const b = document.getElementById('pad-canvas').getBoundingClientRect();
    el.dispatchEvent(new PointerEvent('${type}', { pointerType: 'pen', pointerId: 7, isPrimary: true, buttons: ${buttons}, pressure: ${buttons & 1 ? 0.6 : 0},
      clientX: b.left + ${at[0]} * b.width, clientY: b.top + ${at[1]} * b.height, bubbles: true, cancelable: true })); })()`;
  const nonWhite = (dataUrl) => `new Promise((done) => { const img = new Image(); img.onload = () => { const c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
    const x = c.getContext('2d'); x.drawImage(img, 0, 0); const d = x.getImageData(0, 0, c.width, c.height).data; let n = 0, x0 = Infinity, y0 = Infinity, x1 = -1, y1 = -1;
    for (let i = 0; i < d.length; i += 4) if (d[i] < 250 || d[i + 1] < 250 || d[i + 2] < 250) { n++; const px = (i / 4) % c.width, py = Math.floor(i / 4 / c.width);
      x0 = Math.min(x0, px); y0 = Math.min(y0, py); x1 = Math.max(x1, px); y1 = Math.max(y1, py); }
    done({ n, corner: [...x.getImageData(0, 0, 1, 1).data], size: [c.width, c.height], margins: [x0, y0, c.width - 1 - x1, c.height - 1 - y1] }); }; img.src = ${JSON.stringify(dataUrl)}; })`;
  const zoomLabel = () => page.evaluate(`document.getElementById('pad-zoom-fit').textContent`);
  const line = [[0.2, 0.5], [0.4, 0.45], [0.6, 0.55], [0.8, 0.5]];

  // --- a wide screen: docked beside the lesson ---
  let page = await openSession();
  expect(await page.evaluate(shown('#pad-toggle')) && !(await page.evaluate(shown('#pad'))), 'the pad toggle is not offered in a learn session, or the pad starts open');
  await page.evaluate(click('pad-toggle'));
  await until('the pad', () => page.evaluate(shown('#pad')));
  let g = await page.evaluate(geometry);
  expect(g.mode === 'dock' && g.docked && g.pad[0] + g.pad[2] === g.vw && g.messages[0] + g.messages[2] <= g.pad[0] + 1 && g.modeShown
    && g.board[2] <= g.stage[2] && g.board[3] <= g.stage[3] && g.board[2] > 300, `docked pad: ${JSON.stringify(g)}`);
  expect(await page.evaluate(`document.getElementById('pad-toggle').getAttribute('aria-expanded') === 'true' && document.activeElement === document.getElementById('pad-title')`), 'the pad did not take focus when opened');
  await page.shot('pad-docked');

  // Strokes, undo, clear, eraser.
  expect(await page.evaluate(PIXELS) === 0 && await page.evaluate(`document.getElementById('pad-undo').disabled && document.getElementById('pad-clear').disabled`), 'a new pad is not empty');
  await draw(page, line);
  const inked = await page.evaluate(PIXELS);
  expect(inked > 300 && await page.evaluate(`!document.getElementById('pad-undo').disabled && !document.getElementById('pad-clear').disabled`), `a stroke left ${inked} pixels`);
  await page.evaluate(click('pad-undo'));
  expect(await page.evaluate(PIXELS) === 0, 'undo did not remove the stroke');
  await draw(page, line);
  await draw(page, [[0.3, 0.2], [0.3, 0.8]]);
  const both = await page.evaluate(PIXELS);
  await page.evaluate(click('pad-clear'));
  expect(await page.evaluate(PIXELS) === 0 && /Cleared/.test(await page.evaluate(`document.getElementById('pad-status').textContent`)), 'clear did not empty the pad');
  await page.evaluate(click('pad-undo'));
  const restored = await page.evaluate(PIXELS);
  expect(restored === both && restored > inked, `undo after clear brings both strokes back (${restored} vs ${both})`);
  await page.evaluate(click('pad-eraser'));
  await draw(page, [[0.1, 0.25], [0.5, 0.25]]); // across the vertical stroke only
  const erased = await page.evaluate(PIXELS);
  expect(erased > 0 && erased < restored, `the eraser removes a whole stroke (${erased} of ${restored})`);
  await page.evaluate(click('pad-pen'));
  await page.evaluate(click('pad-undo'));
  expect(await page.evaluate(PIXELS) === restored, 'undoing an erase did not bring the stroke back');

  // Pen buttons: the barrel button held erases; as an action it undoes; touch and mouse follow the setting.
  // A mouse that doesn't draw (Settings says pen only) drags the board instead.
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ inputs: 'pen' }))`);
  await draw(page, [[0.1, 0.9], [0.9, 0.9]]);
  const panned = await page.evaluate(PIXELS);
  expect(panned < restored / 2, `a pen-only mouse drag did not move the board (${panned} of ${restored} pixels left)`);
  await draw(page, [[0.9, 0.9], [0.1, 0.9]]);
  expect(await page.evaluate(PIXELS) === restored, 'the mouse drew while Settings says pen only, or dragging back did not bring the drawing back');
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ inputs: 'mouse' }))`);
  await page.evaluate(penEvent('document.getElementById("pad-canvas")', 'pointerdown', 3, [0.3, 0.25]));
  await page.evaluate(penEvent('document.getElementById("pad-canvas")', 'pointerup', 0, [0.3, 0.25]));
  const penErased = await page.evaluate(PIXELS);
  expect(penErased > 0 && penErased < restored, `the pen's barrel button held erases (${penErased} of ${restored})`);
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ barrel: 'undo' }))`);
  await page.evaluate(penEvent('document.body', 'pointermove', 0));
  await page.evaluate(penEvent('document.body', 'pointerdown', 2));
  expect(await page.evaluate(PIXELS) === restored, 'the barrel button set to Undo did not undo');
  await page.evaluate(`localStorage.removeItem('lh.pen')`);

  // WebKitGTK gives the page a pen as a mouse: pressure 0.5, the barrels as the middle and right buttons (and GTK on
  // Wayland drops a held barrel from `buttons` once the tip is down), the eraser as the left button, and the tip touching
  // while a barrel is held as a chorded pointermove. The shell's `app-pen` events (crates/app/src/pen.rs) say which events
  // are the pen and which barrels are down; these replay what a Wacom AES pen sent through GTK on Wayland (2026-11-16).
  const gtkPen = (detail) => page.evaluate(`window.dispatchEvent(new CustomEvent('app-pen', { detail: ${JSON.stringify({ pen: true, eraser: false, eraserButton: null, barrel: false, barrel2: false, pressure: 0, ...detail })} }))`);
  const gtkEvent = (type, button, buttons, at = [0.3, 0.25]) => `(() => { const c = document.getElementById('pad-canvas'); const b = c.getBoundingClientRect();
    c.dispatchEvent(new PointerEvent('${type}', { pointerType: 'mouse', pointerId: 1, isPrimary: true, button: ${button}, buttons: ${buttons}, pressure: ${buttons ? 0.5 : 0},
      clientX: b.left + ${at[0]} * b.width, clientY: b.top + ${at[1]} * b.height, bubbles: true, cancelable: true })); })()`;
  const INK_AT = `(() => { const c = document.getElementById('pad-canvas'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let s = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 0) s = (s + i) % 1e9; return s; })()`;
  const unchanged = await page.evaluate(INK_AT);
  await gtkPen({});
  await page.evaluate(gtkEvent('pointermove', -1, 0));
  await gtkPen({ barrel: true });
  await page.evaluate(gtkEvent('pointerdown', 1, 4)); // the barrel (BTN_STYLUS, the middle button), hovering
  await gtkPen({ barrel: true, pressure: 0.4 });
  await page.evaluate(gtkEvent('pointermove', 0, 1)); // the tip touches; the held barrel is gone from `buttons`
  await gtkPen({ barrel: true, pressure: 0.5 });
  await page.evaluate(gtkEvent('pointermove', -1, 1));
  await gtkPen({ barrel: true });
  await page.evaluate(gtkEvent('pointerup', 0, 0)); // the tip lifts
  await gtkPen({});
  await page.evaluate(gtkEvent('pointerup', 1, 0)); // the barrel is let go
  const gtkErased = await page.evaluate(PIXELS);
  expect(gtkErased === penErased, `WebKitGTK: drawing with the barrel held did not erase, or the middle button dragged the board (${gtkErased}, a pen's barrel erase leaves ${penErased})`);
  // An eraser button (libwacom's EraserType=Button): GTK switches to the eraser tool while it is held, with no button
  // event, and the shell sees the same tool ids as the pen's. It is the second barrel, whose default action is Undo.
  await gtkPen({ eraser: true, eraserButton: true });
  await page.evaluate(gtkEvent('pointermove', -1, 0, [0.5, 0.5]));
  expect(await page.evaluate(INK_AT) === unchanged, 'WebKitGTK: pressing the eraser button (the second barrel) did not undo');
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ barrel2: 'erase', eraser: 'none' }))`);
  await gtkPen({ eraser: true, eraserButton: true, pressure: 0.3 });
  await page.evaluate(gtkEvent('pointerdown', 0, 1));
  await gtkPen({ eraser: true, eraserButton: true });
  await page.evaluate(gtkEvent('pointerup', 0, 0));
  expect(await page.evaluate(PIXELS) === penErased, 'WebKitGTK: drawing with the eraser button held (second barrel: Erase while held) did not erase, or it read as the eraser end');
  await gtkPen({});
  await page.evaluate(gtkEvent('pointermove', -1, 0));
  await page.evaluate(`localStorage.removeItem('lh.pen')`);
  await page.evaluate(click('pad-undo'));
  expect(await page.evaluate(INK_AT) === unchanged, 'undoing the eraser button’s erase did not bring the stroke back');
  await gtkPen({ pressure: 0.9 });
  await page.evaluate(gtkEvent('pointerdown', 0, 1, [0.6, 0.7]));
  await gtkPen({ pressure: 0.2 });
  await page.evaluate(gtkEvent('pointermove', -1, 1, [0.8, 0.7]));
  await gtkPen({});
  await page.evaluate(gtkEvent('pointerup', 0, 0, [0.8, 0.7]));
  await until('WebKitGTK: the stroke to keep the pen’s pressure', () => page.evaluate(`(JSON.parse(localStorage.getItem('lh.pad.L1'))?.strokes.at(-1)?.p ?? [])
    .filter((_, i) => i % 3 === 2).join() === '0.9,0.2'`));
  // "The eraser is: An end of the pen" overrides the shell's guess: the eraser end erases.
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ eraserIs: 'end' }))`);
  await gtkPen({ eraser: true, eraserButton: true, pressure: 0.6 });
  await page.evaluate(gtkEvent('pointerdown', 0, 1, [0.7, 0.7]));
  await page.evaluate(gtkEvent('pointerup', 0, 0, [0.7, 0.7]));
  expect(await page.evaluate(INK_AT) === unchanged, 'WebKitGTK: the eraser end (set in Settings) did not erase');
  await page.evaluate(`localStorage.removeItem('lh.pen')`);
  await gtkPen({ pen: false });

  // Zoom: the buttons zoom about the centre, Ctrl+wheel at the pointer, the label fits the drawing; strokes are redrawn, not stretched pixels.
  expect(await zoomLabel() === '100%', `a new pad is not at 100%: ${await zoomLabel()}`);
  await page.evaluate(click('pad-zoom-in'));
  const zoomed = await page.evaluate(PIXELS);
  expect(await zoomLabel() === '141%' && zoomed > restored * 1.5, `zooming in: ${await zoomLabel()}, ${zoomed} vs ${restored} pixels`);
  await page.evaluate(click('pad-zoom-out'));
  const unzoomed = await page.evaluate(PIXELS);
  expect(await zoomLabel() === '100%' && Math.abs(unzoomed - restored) <= restored * 0.01, `zooming back out: ${await zoomLabel()}, ${unzoomed} vs ${restored} pixels`);
  await page.evaluate(`(() => { const c = document.getElementById('pad-canvas'); const b = c.getBoundingClientRect();
    c.dispatchEvent(new WheelEvent('wheel', { deltaY: -100, ctrlKey: true, clientX: b.left + 0.2 * b.width, clientY: b.top + 0.5 * b.height, bubbles: true, cancelable: true })); })()`);
  expect(await zoomLabel() === '141%', `Ctrl+wheel did not zoom in: ${await zoomLabel()}`);
  // Two fingers pinch about their midpoint (palm rejection off: the synthetic pen was just here).
  await page.evaluate(`localStorage.setItem('lh.pen', JSON.stringify({ palm: false }))`);
  const [px, py] = await page.evaluate(`(() => { const b = document.getElementById('pad-canvas').getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2]; })()`);
  await page.touch('touchStart', [[px - 40, py], [px + 40, py]]);
  await page.touch('touchMove', [[px - 60, py], [px + 60, py]]);
  await page.touch('touchMove', [[px - 80, py], [px + 80, py]]);
  await page.touch('touchEnd', []);
  await page.evaluate(`localStorage.removeItem('lh.pen')`);
  expect(await zoomLabel() === '283%', `a pinch did not zoom about the fingers: ${await zoomLabel()}`);
  await page.evaluate(click('pad-zoom-fit'));
  const fitted = await page.evaluate(PIXELS);
  expect(await zoomLabel() !== '141%' && fitted > restored, `fitting the drawing: ${await zoomLabel()}, ${fitted} vs ${restored} pixels`);

  // The figure the tutor made is a background; the picture sent has it under the strokes.
  expect(await page.evaluate(shown('#pad-bg-pick')) && await page.evaluate(`[...document.getElementById('pad-bg').options].map((o) => o.textContent).join('|')`) === 'Plain paper|Figure 1: Voltage divider', 'the figure is not offered as a background');
  expect(await page.evaluate(`document.querySelector('#pad-bg-layer svg') === null`), 'a background is showing before it was picked');

  // Sending with the open question: reasoning or answer.
  await page.evaluate(click('pad-send'));
  const items = await page.evaluate(`[...document.querySelectorAll('#pad-send-menu .menu-item')].map((b) => b.textContent)`);
  expect(JSON.stringify(items) === JSON.stringify(['With my reasoning', 'With my answer']), `send targets for a free question: ${JSON.stringify(items)}`);
  await page.evaluate(`document.querySelector('#pad-send-menu .menu-item').click()`);
  await until('the sketch on the reasoning', () => page.evaluate(`document.querySelectorAll('#timeline .ex-sketch:not([hidden])').length === 1`));
  let staged = await page.evaluate(argsOf('stage_image'));
  expect(staged.length === 1 && staged[0].data.startsWith('data:image/png;base64,') && staged[0].name === 'Scratch pad.png', `staged: ${JSON.stringify(staged.map((s) => s.name))}`);
  const plain = await page.evaluate(nonWhite(staged[0].data));
  expect(plain.corner.join() === '255,255,255,255' && plain.n > 300, `the picture is ink on white paper: ${JSON.stringify(plain)}`);
  expect(Math.max(...plain.size) === 1600 && plain.margins.every((m) => m >= 28 && m <= 36), `the picture is the drawing's bounding box with a margin: ${JSON.stringify(plain)}`);

  await page.evaluate(`(() => { const s = document.getElementById('pad-bg'); s.value = 'A1'; s.dispatchEvent(new Event('change')); })()`);
  expect(await page.evaluate(`Boolean(document.querySelector('#pad-bg-layer svg')) && document.getElementById('pad-bg-layer').getClientRects().length > 0`), 'the figure did not become the background');
  await page.shot('pad-figure');
  await page.evaluate(click('pad-send'));
  await page.evaluate(`document.querySelectorAll('#pad-send-menu .menu-item')[1].click()`);
  await until('the sketch on the answer', () => page.evaluate(`document.querySelectorAll('#timeline .ex-sketch:not([hidden])').length === 2`));
  staged = await page.evaluate(argsOf('stage_image'));
  const withFigure = await page.evaluate(nonWhite(staged[1].data));
  // Picked while the view fits the strokes, the figure lands inside their box: same picture size, more ink.
  expect(staged[1].name === 'Scratch pad on Figure 1: Voltage divider.png' && withFigure.size.join() === plain.size.join() && withFigure.n > plain.n && withFigure.corner.join() === '255,255,255,255',
    `the figure is in the picture: ${JSON.stringify({ name: staged[1].name, plain, withFigure })}`);
  await page.shot('pad-sketches-attached');

  // Replacing a sketch drops the staged one.
  await page.evaluate(click('pad-send'));
  await page.evaluate(`document.querySelector('#pad-send-menu .menu-item').click()`);
  await until('the replacement', () => page.evaluate(`${argsOf('stage_image')}.length === 3`));
  await until('the replaced sketch dropped', () => page.evaluate(`${argsOf('unstage_image')}.length === 1`));

  // The answer goes with both drawings and no typed answer.
  await page.evaluate(`(() => { const card = document.querySelector('#timeline .exercise.pending');
    card.querySelector('.ex-form .confidence input[value="70"]').click();
    card.querySelector('.ex-form button[type="submit"]').click(); })()`);
  await until('the answer sent', () => page.evaluate(`${argsOf('answer')}.length === 1`));
  const sent = (await page.evaluate(argsOf('answer')))[0].payload;
  expect(sent.answer === '' && sent.sketches.length === 2 && sent.sketches[0].in === 'reasoning' && sent.sketches[1].in === 'answer'
    && sent.sketches.every((s) => s.attachment.kind === 'image' && /^attachments\/2026\/STAGED\d+\.png$/.test(s.attachment.path)), `answer payload: ${JSON.stringify(sent)}`);
  await until('the answered card shows both sketches', () => page.evaluate(`document.querySelectorAll('#timeline .you-sketch img[src^="data:image/png"]').length === 2`));
  expect(await page.evaluate(`[...document.querySelectorAll('#timeline .you-sketch figcaption')].map((f) => f.textContent).join('|')`) === 'Sketch · with your reasoning|Sketch · with your answer', 'the sent sketches are not captioned');

  // With no question open, a drawing goes to the next message as a chip.
  await page.evaluate(click('pad-send'));
  await until('the chip', () => page.evaluate(`document.querySelectorAll('#attach-chips li').length === 1 && !document.getElementById('composer-attach').hidden`));
  expect(await page.evaluate(`document.querySelector('#attach-chips .chip-name').textContent`) === 'Scratch pad on Figure 1: Voltage divider.png', 'the message chip is not the drawing');

  // Dock / overlay switch (the bigger panel shows more of the board at the same scale), Escape only closes the overlay, the drawing outlives closing.
  const dockedInk = await page.evaluate(PIXELS);
  const dockedWidth = await page.evaluate(`document.getElementById('pad-canvas').width`);
  await page.evaluate(click('pad-mode'));
  await until('the canvas to grow', () => page.evaluate(`document.getElementById('pad-canvas').width > ${dockedWidth}`));
  g = await page.evaluate(geometry);
  expect(g.mode === 'overlay' && !g.docked && g.pad[2] === g.vw && g.pad[3] === g.vh, `overlay: ${JSON.stringify(g)}`);
  const overlayInk = await page.evaluate(PIXELS);
  expect(overlayInk === dockedInk && await zoomLabel() !== '100%', `the overlay scaled the drawing instead of showing more board: ${overlayInk} vs ${dockedInk} pixels`);
  expect(await page.evaluate(`document.querySelector('.pad-mode-dock').getClientRects().length > 0 && document.querySelector('.pad-mode-expand').getClientRects().length === 0
    && document.getElementById('pad-mode').getAttribute('aria-label') === 'Dock beside the lesson'`), 'the mode switch shows the wrong icon or name in the overlay');
  await page.shot('pad-overlay');
  await page.evaluate(click('pad-mode'));
  expect((await page.evaluate(geometry)).mode === 'dock', 'the switch did not dock the pad again');
  expect(await page.evaluate(`document.querySelector('.pad-mode-expand').getClientRects().length > 0 && document.querySelector('.pad-mode-dock').getClientRects().length === 0
    && document.getElementById('pad-mode').getAttribute('aria-label') === 'Show over the lesson'`), 'the mode switch shows the wrong icon or name when docked');
  await page.evaluate(`document.getElementById('pad-pen').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  expect(await page.evaluate(shown('#pad')), 'Escape closed a docked pad');
  await page.evaluate(click('pad-close'));
  g = await page.evaluate(geometry);
  expect(!(await page.evaluate(shown('#pad'))) && !g.docked && g.messages[2] >= g.vw - 20, `after closing: ${JSON.stringify(g)}`);
  expect(await page.evaluate(`document.activeElement === document.getElementById('pad-toggle') && document.getElementById('pad-toggle').getAttribute('aria-expanded') === 'false'`), 'focus did not return to the toggle');
  await page.evaluate(click('pad-toggle'));
  await until('the pad again', () => page.evaluate(shown('#pad')));
  expect(await page.evaluate(PIXELS) > 0 && await page.evaluate(`localStorage.getItem('lh.pad.L1') !== null`), 'the drawing was lost on closing');
  expect(await page.evaluate(`document.getElementById('pad-bg').value`) === 'A1' && await page.evaluate(shown('#pad-bg-layer svg')), 'the background was lost on closing');
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // --- a narrow tablet: always the full overlay, Escape closes it ---
  page = await openSession({ width: 800, height: 1000, dark: true });
  await page.evaluate(click('pad-toggle'));
  await until('the pad', () => page.evaluate(shown('#pad')));
  g = await page.evaluate(geometry);
  expect(g.mode === 'overlay' && !g.modeShown && !g.docked && g.pad[2] === 800 && g.pad[3] === 1000, `narrow pad: ${JSON.stringify(g)}`);
  await draw(page, line);
  expect(await page.evaluate(PIXELS) > 300, 'nothing drew on the narrow pad');
  // Dark theme: the ink is light on the dark paper (the picture sent is still dark on white).
  const lum = await page.evaluate(`(() => { const c = document.getElementById('pad-canvas'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    for (let i = 0; i < d.length; i += 4) if (d[i + 3] > 250) return (d[i] + d[i + 1] + d[i + 2]) / 3; return -1; })()`);
  expect(lum > 150, `the ink is not light in the dark theme: ${lum}`);
  await page.shot('pad-overlay-dark');
  await page.evaluate(`document.getElementById('pad-pen').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  expect(!(await page.evaluate(shown('#pad'))), 'Escape did not close the overlay');
  await page.close();

  // --- a phone: no pad ---
  page = await openSession({ width: 390, height: 844, mobile: true });
  expect(!(await page.evaluate(shown('#pad-toggle'))) && !(await page.evaluate(shown('#pad'))), 'a phone offers the scratch pad');
  await page.close();

  // --- Settings → Pen ---
  page = await openApp('none');
  await page.evaluate(click('open-settings'));
  await until('Settings', () => page.evaluate(`document.getElementById('settings').open`));
  await page.evaluate(`document.getElementById('tab-btn-models').focus()`);
  const key = (name) => page.evaluate(`document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: '${name}', bubbles: true }))`);
  const selected = () => page.evaluate(`document.querySelector('#settings-tabs [aria-selected=true]').dataset.tab + ':' + [...document.querySelectorAll('#settings .settings-panel')].filter((p) => !p.hidden).length + ':' + document.activeElement.dataset.tab`);
  await key('ArrowRight');
  expect(await selected() === 'learning:1:learning', 'ArrowRight did not move to the next tab');
  await key('End');
  expect(await selected() === 'appearance:1:appearance', 'End did not go to the last tab');
  await key('ArrowRight');
  expect(await selected() === 'models:1:models', 'ArrowRight did not wrap to the first tab');
  await page.evaluate(click('tab-btn-pen'));
  await page.shot('settings-pen');
  await page.evaluate(`(() => { const s = document.getElementById('pen-barrel'); s.value = 'color'; s.dispatchEvent(new Event('change'));
    document.querySelector('#pen-inputs input[value="all"]').click(); document.querySelector('#pen-toggles input[data-pen="palm"]').click(); })()`);
  const saved = await page.evaluate(`JSON.parse(localStorage.getItem('lh.pen'))`);
  expect(saved.barrel === 'color' && saved.inputs === 'all' && saved.palm === false, `saved pen settings: ${JSON.stringify(saved)}`);
  expect(await page.evaluate(`[...document.getElementById('pen-eraser').options].map((o) => o.value).join()`) === 'none,erase,undo,color,width,pad'
    && await page.evaluate(`document.getElementById('pen-eraser').value`) === 'erase', 'the eraser end does not default to erasing');
  await page.evaluate(`document.getElementById('pen-test').dispatchEvent(new PointerEvent('pointermove', { pointerType: 'pen', pointerId: 3, buttons: 34, pressure: 0.4, bubbles: true }))`);
  const readout = await page.evaluate(`document.getElementById('pen-test-readout').textContent`);
  expect(/pen · pressure 0\.40/.test(readout) && /barrel button \+ eraser end down/.test(readout), `the pen test readout: ${readout}`);
  // WebKitGTK: mouse events the shell says are the pen read as the pen: an eraser end touching, then a pen whose eraser is
  // a button (the second barrel) drawing with it and the barrel held, which GTK leaves out of `buttons`; then the
  // setting "The eraser is: An end of the pen" over the shell's guess.
  const gtkReadout = (detail) => page.evaluate(`window.dispatchEvent(new CustomEvent('app-pen', { detail: ${JSON.stringify({ pen: true, eraser: false, eraserButton: null, barrel: false, barrel2: false, ...detail })} }));
    document.getElementById('pen-test').dispatchEvent(new PointerEvent('pointermove', { pointerType: 'mouse', pointerId: 1, buttons: 1, pressure: 0.5, bubbles: true }));
    window.dispatchEvent(new CustomEvent('app-pen', { detail: { pen: false } }));
    document.getElementById('pen-test-readout').textContent`);
  const eraserEnd = await gtkReadout({ eraser: true, eraserButton: false, pressure: 0.25 });
  expect(/^pen · pressure 0\.25 · hovering · eraser end down/.test(eraserEnd), `the pen test readout of a WebKitGTK eraser end: ${eraserEnd}`);
  const eraserButton = await gtkReadout({ eraser: true, eraserButton: true, barrel: true, pressure: 0.5 });
  expect(/^pen · pressure 0\.50 · touching · barrel button \+ second barrel button \(the eraser button\) down/.test(eraserButton), `the pen test readout of a WebKitGTK pen with an eraser button: ${eraserButton}`);
  expect(await page.evaluate(`document.querySelector('#pen-eraser-kind input:checked')?.value`) === 'auto', 'The eraser is: does not default to Automatic');
  await page.evaluate(`document.querySelector('#pen-eraser-kind input[value="end"]').click()`);
  const overridden = await gtkReadout({ eraser: true, eraserButton: true, pressure: 0.5 });
  expect(JSON.parse(await page.evaluate(`localStorage.getItem('lh.pen')`)).eraserIs === 'end' && /hovering · eraser end down/.test(overridden),
    `The eraser is: An end of the pen did not override the shell's guess: ${overridden}`);
  await page.evaluate(`document.querySelector('#pen-eraser-kind input[value="button"]').click();
    document.getElementById('pen-test').dispatchEvent(new PointerEvent('pointermove', { pointerType: 'pen', pointerId: 3, buttons: 32, pressure: 0.4, bubbles: true }))`);
  const nativeButton = await page.evaluate(`document.getElementById('pen-test-readout').textContent`);
  expect(/^pen · pressure 0\.40 · touching · second barrel button \(the eraser button\) down/.test(nativeButton), `The eraser is: A button on the barrel, for a pen the browser reports: ${nativeButton}`);
  await page.evaluate(`document.getElementById('settings').close()`);
  await page.evaluate(click('open-settings'));
  await until('Settings again', () => page.evaluate(`document.getElementById('settings').open`));
  expect(await page.evaluate(`document.querySelector('#settings-tabs [aria-selected=true]').dataset.tab`) === 'pen', 'Settings did not reopen on the last tab');
  await page.evaluate(`localStorage.removeItem('lh.settings.tab')`); // the next case starts on the first tab
  const penErrors = await page.evaluate('window.__errors');
  expect(penErrors.length === 0, `page errors: ${penErrors.join(' | ')}`);
  await page.close();
  console.log('pad: toggle in a learn session (not on a phone); docked beside the lesson on a wide screen with a dock/overlay switch (the overlay shows more board at the same scale), always an overlay on a narrow one; strokes, undo, clear, eraser; pen-only setting (the mouse pans) and the pen buttons (hold to erase, undo); a WebKitGTK pen (mouse events plus the shell’s app-pen: pressure, a barrel held while drawing erases without panning though GTK drops it from buttons, an eraser button is the second barrel, an eraser end); zoom by buttons, Ctrl+wheel and a pinch, fit to the drawing; the tutor’s figure as the background and in the picture; the picture is the drawing’s bounding box with a margin; a drawing goes with the reasoning, the answer or the next message and shows on the answered card; kept per session; Settings tabs with arrow keys and Settings → Pen');
}

// Touch and the graphs: the head names only the current phase, a title's
// formula is drawn as Unicode on the Map and the plan diagram, and two
// fingers zoom the plan diagram (a pinch never zooms the page).
async function touchCase() {
  const page = await openApp('none', { query: '&fixture=learn&mathTitle=1', reducedMotion: true });
  await until('the session list', () => page.evaluate(`Boolean(document.querySelector('#session-list .session-open'))`));
  await page.evaluate(`document.querySelector('#session-list .session-open').click()`);
  await until('the Plan button', () => page.evaluate(`!document.getElementById('plan-toggle').hidden`));

  // The head: one phase on screen, all four for a screen reader.
  const phase = await page.evaluate(`({ shown: [...document.querySelectorAll('#phase li')].filter((li) => li.getBoundingClientRect().width > 4).map((li) => li.textContent),
    all: document.querySelectorAll('#phase li').length, current: document.querySelector('#phase li[aria-current="step"]')?.textContent })`);
  expect(phase.shown.join('|') === 'Teach 1/2' && phase.all === 4 && phase.current === 'Teach 1/2', `phase nav: ${JSON.stringify(phase)}`);
  await page.shot('head-phase');

  // The plan diagram at idea level: the formula is Unicode.
  await page.evaluate(click('plan-toggle'));
  await until('the plan sheet', () => page.evaluate(`document.getElementById('plan-panel').open`));
  await page.evaluate(`[...document.querySelectorAll('#plan-panel .plan-level')].find((b) => /idea/i.test(b.textContent)).click()`);
  await until('the idea boxes', () => page.evaluate(`[...document.querySelectorAll('#plan-panel .plan-node')].some((g) => /Limit/.test(g.textContent))`));
  const label = await page.evaluate(`[...document.querySelectorAll('#plan-panel .plan-node text')].map((t) => t.textContent).join(' ')`);
  expect(/αₙ/.test(label) && !/\\$/.test(label) && !/\\\\alpha/.test(label), `plan label: ${label}`);

  // Two fingers zoom the diagram about their midpoint.
  const scaleOf = `parseFloat(/scale\\(([\\d.]+)\\)/.exec(document.querySelector('#plan-panel .plan-view').style.transform)?.[1])`;
  const [sx, sy, sw, sh] = await page.evaluate(`(() => { const b = document.querySelector('#plan-panel .plan-stage').getBoundingClientRect(); return [b.left, b.top, b.width, b.height]; })()`);
  const [cx, cy] = [sx + sw / 2, sy + sh / 2];
  const before = await page.evaluate(scaleOf);
  await page.touch('touchStart', [[cx - 40, cy], [cx + 40, cy]]);
  await page.touch('touchMove', [[cx - 90, cy], [cx + 90, cy]]);
  await page.touch('touchMove', [[cx - 140, cy], [cx + 140, cy]]);
  const during = await page.evaluate(scaleOf);
  await page.touch('touchEnd', []);
  expect(during > before * 1.5 && (await page.evaluate(`document.querySelector('#plan-panel .plan-stage').dataset.dragging`)) === undefined, `a pinch zooms the diagram (${before} → ${during})`);
  // Ctrl+wheel (a touchpad pinch) is the diagram's over it, and nobody's elsewhere.
  const wheel = (target) => `(() => { const e = new WheelEvent('wheel', { ctrlKey: true, deltaY: -10, cancelable: true, bubbles: true }); ${target}.dispatchEvent(e); return e.defaultPrevented; })()`;
  expect(await page.evaluate(wheel(`document.querySelector('#plan-panel .plan-stage')`)) && await page.evaluate(wheel('document.body')), 'ctrl+wheel was left to the page');
  const pinchEvent = (type) => `(() => { const e = new Event('${type}', { cancelable: true, bubbles: true }); document.body.dispatchEvent(e); return e.defaultPrevented; })()`;
  expect(await page.evaluate(pinchEvent('gesturestart')), 'a gesture over the page was left to the engine');
  await page.evaluate(`document.getElementById('plan-panel').close()`);

  // The Map: the formula in an idea's title.
  await page.evaluate(click('back'));
  await until('Home', () => page.evaluate(shown('#start')));
  await page.evaluate(click('open-map'));
  await until('the map', () => page.evaluate(`Boolean(${q('#map-page .map-a11y input[data-key="limit"]')})`));
  const entry = await page.evaluate(`${q('#map-page .map-a11y input[data-key="limit"]')}.closest('label').textContent`);
  expect(/Limit αₙ → 0/.test(entry), `map title: ${entry}`);
  // KaTeX typesets the label over the canvas and the card's title and statement.
  await page.evaluate(`${q('#map-page .map-a11y input[data-key="limit"]')}.click()`); // selected: its label is always drawn
  await until('the typeset label', () => page.evaluate(`Boolean(document.querySelector('#map-page .map-math-label .katex')) && document.querySelector('#map-page .map-math-label').style.visibility === 'visible'`));
  const typeset = await page.evaluate(`(() => { const l = document.querySelector('#map-page .map-math-label'); const b = l.getBoundingClientRect();
    const c = l.cloneNode(true); c.querySelectorAll('.katex-mathml').forEach((x) => x.remove());
    return { text: c.textContent, size: [Math.round(b.width), Math.round(b.height)], hidden: l.closest('[aria-hidden]')?.className }; })()`);
  expect(!/[$\\]/.test(typeset.text) && /^Limit/.test(typeset.text) && typeset.size[0] > 20 && typeset.size[1] > 8 && typeset.hidden === 'map-math-layer', `typeset label: ${JSON.stringify(typeset)}`);
  await page.evaluate(`${q('#map-page .map-a11y input[data-key="limit"]')}.focus()`);
  await until('the card', () => page.evaluate(`Boolean(document.querySelector('#map-page .map-card-statement .katex'))`));
  const cardText = await page.evaluate(`(() => { const seen = (el) => { const c = el.cloneNode(true); c.querySelectorAll('.katex-mathml').forEach((x) => x.remove()); return c.textContent; };
    const card = document.querySelector('#map-page .map-card'); return { title: Boolean(card.querySelector('.map-card-title .katex')), shown: seen(card.querySelector('.map-card-title')) + ' | ' + seen(card.querySelector('.map-card-statement')) }; })()`);
  expect(cardText.title && !/[$\\]/.test(cardText.shown), `card math: ${JSON.stringify(cardText)}`);
  await until('the origin quote typeset', () => page.evaluate(`Boolean(document.querySelector('#map-page .map-card-origin-quote .katex'))`));
  await page.shot('map-math');
  const errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('touch: the head shows only the current phase (all four for screen readers), a formula in a title is Unicode on the plan diagram and the Map, two fingers zoom the plan diagram, pinch gestures and ctrl+wheel are cancelled for the page');
}
// The scratch pad on the Practice screen: beside a set's problems (a drawing
// goes with one problem's answer, kept with the draft, handed in with the
// set and shown on the review), with a one-at-a-time answer (a chip in the
// composer), and on a timed exam paper (saved with the draft).
async function padPractice() {
  const PIXELS = `(() => { const c = document.getElementById('pad-canvas'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++; return n; })()`;
  const draw = async (page) => {
    const [x, y, w, h] = await page.evaluate(`(() => { const b = document.getElementById('pad-canvas').getBoundingClientRect(); return [b.left, b.top, b.width, b.height]; })()`);
    await page.mouse('mouseMoved', x + 0.2 * w, y + 0.5 * h);
    await page.mouse('mousePressed', x + 0.2 * w, y + 0.5 * h, 1);
    for (const f of [0.4, 0.6, 0.8]) await page.mouse('mouseMoved', x + f * w, y + (0.4 + f / 5) * h, 1);
    await page.mouse('mouseReleased', x + 0.8 * w, y + 0.56 * h, 0);
  };
  const openPad = async (page, id) => {
    await until('the pad toggle', () => page.evaluate(shown(`#${id}`)));
    await page.evaluate(click(id));
    await until('the pad', () => page.evaluate(shown('#pad')));
    await draw(page);
    expect(await page.evaluate(PIXELS) > 300, 'nothing drew on the pad');
  };
  const menu = async (page) => {
    await page.evaluate(click('pad-send'));
    return page.evaluate(`[...document.querySelectorAll('#pad-send-menu .menu-item')].map((b) => b.textContent)`);
  };

  // --- a set: a drawing with problem 2's answer ---
  let page = await openApp('none', PRACTICE);
  await until('the review cue', () => page.evaluate(shown('#review-cue')));
  await page.evaluate(choose('review-cue-format', 'A set of problems'));
  await page.evaluate(click('practise-due'));
  await until('every problem', () => page.evaluate(`${qa('#practice-flow .problem')}.length === 3`));
  await openPad(page, 'practice-pad-toggle');
  const docked = await page.evaluate(`({ docked: document.getElementById('practice').classList.contains('pad-docked'), toggle: document.getElementById('practice-pad-toggle').getAttribute('aria-expanded'),
    learnToggle: ${shown('#pad-toggle')}, pad: document.getElementById('pad').getBoundingClientRect().right === innerWidth })`);
  expect(docked.docked && docked.toggle === 'true' && !docked.learnToggle && docked.pad, `docked beside the set: ${JSON.stringify(docked)}`);
  await page.shot('pad-practice-set');
  const items = await menu(page);
  expect(items.join('|') === 'With my answer to problem 1|With my answer to problem 2|With my answer to problem 3', `set targets: ${JSON.stringify(items)}`);
  await page.evaluate(`document.querySelectorAll('#pad-send-menu .menu-item')[1].click()`);
  await until('the sketch on problem 2', () => page.evaluate(`${qa('#practice-flow .problem')}[1].querySelectorAll('.ex-sketch:not([hidden])').length === 1`));
  const kept = await page.evaluate(`Object.values(JSON.parse(localStorage.getItem(Object.keys(localStorage).find((k) => k.startsWith('lh.practice.draft.'))))).map((d) => d.sketch?.kind)`);
  expect(kept.join() === 'image' && /^1 of 3 answered/.test(await page.evaluate(`${textOf('#practice-note')}`)), `the draft keeps the drawing: ${JSON.stringify(kept)}`);
  // Reopening the set restores the drawing from the draft.
  await page.evaluate(click('pad-close'));
  await page.evaluate(click('practice-back'));
  await until('Home', () => page.evaluate(shown('#start')));
  await page.evaluate(`${qa('#session-list .session-open')}.find((b) => /Practice/.test(b.textContent)).click()`);
  await until('the drawing restored', () => page.evaluate(`${qa('#practice-flow .problem')}[1]?.querySelectorAll('.ex-sketch:not([hidden]) img[src^="data:image/png"]').length === 1`));
  // Hand in: problem 2 goes with a drawing and no text.
  await page.evaluate(click('practice-primary'));
  await until('the review', () => page.evaluate(`${qa('#practice-flow .result')}.length === 3`));
  const handed = (await page.evaluate(argsOf('practice_submit_set')))[0].answers;
  expect(handed.length === 1 && handed[0].problemId === 'p2' && handed[0].text === '' && handed[0].sketch.kind === 'image', `hand-in: ${JSON.stringify(handed)}`);
  await until('the drawing on the review', () => page.evaluate(`document.querySelectorAll('#practice-flow .you-sketch img[src^="data:image/png"]').length === 1`));
  await until('the draft cleared', () => page.evaluate(`localStorage.getItem(Object.keys(localStorage).find((k) => k.startsWith('lh.practice.draft.')) ?? 'x') === null`));
  let errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // --- one at a time: the drawing becomes a chip on the answer ---
  page = await openApp('none', PRACTICE);
  await until('the review cue', () => page.evaluate(shown('#review-cue')));
  await page.evaluate(choose('review-cue-format', 'One at a time'));
  await page.evaluate(click('practise-due'));
  await until('the answer box', () => page.evaluate(shown('#composer')));
  await openPad(page, 'practice-pad-toggle');
  // One target: the first click sends at once and there is no menu.
  const one = await menu(page);
  expect(one.length === 0, `stream targets: ${JSON.stringify(one)}`);
  await until('the chip', () => page.evaluate(`document.querySelectorAll('#attach-chips li').length === 1 && !document.getElementById('composer-attach').hidden`));
  errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();

  // --- an exam paper: saved with the draft ---
  page = await openApp('none', EXAM);
  await openDashboard(page);
  await page.evaluate(`${q('.exam-start button[type=submit]')}.click()`);
  await until('the clock', () => page.evaluate(shown('.exam-clock')));
  await openPad(page, 'practice-pad-toggle');
  const paper = await menu(page);
  expect(paper.length === 3 && /^With my answer to problem 1$/.test(paper[0]), `paper targets: ${JSON.stringify(paper)}`);
  await page.evaluate(`document.querySelectorAll('#pad-send-menu .menu-item')[0].click()`);
  await until('the sketch on problem 1', () => page.evaluate(`document.querySelectorAll('.exam-paper .ex-sketch:not([hidden])').length === 1`));
  await until('the draft saved with the drawing', () => page.evaluate(`${argsOf('practice_exam_save')}.some((a) => a.answers.some((x) => x.sketch?.kind === 'image'))`));
  expect(/1 of 3 answered/.test(await page.evaluate(textOf('.exam-count'))), 'a drawn answer does not count as answered');
  await page.shot('pad-practice-exam');
  errors = await page.evaluate('window.__errors');
  expect(errors.length === 0, `page errors: ${errors.join(' | ')}`);
  await page.close();
  console.log('pad-practice: the pad beside a set (a drawing goes with one problem, kept with the draft, restored on reopening, handed in without text and shown on the review), a one-at-a-time answer (a composer chip) and an exam paper (saved with the draft)');
}
const cases = { ipc, placement, sync, library, map, session, reading, turn, offer, plugins: pluginsCase, practice, 'practice-set': practiceSet, 'practice-resume': practiceResume, exam, pad, 'pad-practice': padPractice, touch: touchCase };
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
