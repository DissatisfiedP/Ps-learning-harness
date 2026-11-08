// Host for a plugin's interactive view (build prompt W7).
//
// The view runs in `<iframe sandbox="allow-scripts">` (never
// allow-same-origin), so it has an opaque origin and cannot reach this
// document or the Tauri IPC bridge. The only path out is this postMessage
// bridge: init/call/result/resize/state/submit, protocol 1.

import { call } from './api.js';

export const PROTOCOL = 1;

// Frame height from the view's `resize`, clamped to 160 px … 80 % of the window.
const MIN_HEIGHT = 160;

const TOKENS = [
  'paper', 'raised', 'ink', 'ink-2', 'ink-3', 'rule', 'rule-strong',
  'accent', 'accent-hover', 'on-accent', 'danger', 'select',
];

/** Theme values for the iframe; it applies them as `--name` properties. */
export function themeTokens() {
  const cs = getComputedStyle(document.documentElement);
  const out = {};
  for (const name of TOKENS) out[name] = cs.getPropertyValue(`--${name}`).trim();
  out['font-ui'] = cs.getPropertyValue('--font-ui').trim();
  return out;
}

function message(err) {
  return typeof err === 'string' ? err : (err?.message ?? String(err));
}

// Live hosts, swept when their element leaves the document: a removed or
// re-rendered figure fires no event of its own, and its window listener,
// observers and iframe reference would otherwise stay behind. One observer
// serves every host; mutations only schedule a sweep.
const live = new Set();
let sweepTimer = 0;
const domWatch = new MutationObserver(() => {
  if (!sweepTimer) sweepTimer = setTimeout(sweep, 500);
});

function sweep() {
  sweepTimer = 0;
  for (const host of live) {
    if (!host.el.isConnected) host.destroy();
  }
}

function track(host) {
  if (!live.size) domWatch.observe(document.documentElement, { childList: true, subtree: true });
  live.add(host);
}

function untrack(host) {
  live.delete(host);
  if (!live.size) {
    domWatch.disconnect();
    clearTimeout(sweepTimer);
    sweepTimer = 0;
  }
}

/**
 * Loads a plugin's view and returns a host element. Resolves after the
 * document is in place; init is sent when the iframe's load event fires.
 *
 * `answer` turns on the submit channel; `onSubmit(value)` is called with the
 * view's answer. `onState(summary)` defaults to recording `view_state` on the
 * session.
 */
export async function createViewHost({
  pluginId,
  artifact,
  alt = '',
  answer = false,
  sessionId = null,
  onSubmit = null,
  onState = null,
  onError = null,
}) {
  const view = await call('plugin_view', { pluginId });
  const wrap = document.createElement('div');
  wrap.className = 'view-host';

  const iframe = document.createElement('iframe');
  iframe.className = 'view-frame';
  iframe.setAttribute('sandbox', 'allow-scripts');
  iframe.setAttribute('referrerpolicy', 'no-referrer');
  iframe.setAttribute('title', alt || 'Interactive figure');
  iframe.style.height = '320px';
  wrap.append(iframe);

  let destroyed = false;
  let submitted = false;
  let calling = false;
  let stateTimer = 0;
  let statePending = null;

  const post = (data) => {
    try {
      iframe.contentWindow?.postMessage(data, '*');
    } catch {
      // The frame went away between events.
    }
  };

  const sendState = (summary) => {
    statePending = summary;
    if (stateTimer) return;
    stateTimer = setTimeout(() => {
      stateTimer = 0;
      const text = statePending;
      statePending = null;
      if (!text || destroyed) return;
      if (onState) onState(text);
      else if (sessionId) {
        call('view_state', { sessionId, artifactId: artifact.artifactId, summary: text }).catch(() => {});
      }
    }, 250);
  };

  const onMessage = (event) => {
    if (destroyed || event.source !== iframe.contentWindow) return;
    const data = event.data;
    if (!data || typeof data !== 'object' || data.protocol !== PROTOCOL) return;
    // Once an answer is submitted the frame is inert: resize only.
    if (submitted && data.type !== 'resize') return;
    switch (data.type) {
      case 'call': {
        const { id, fn } = data;
        if ((typeof id !== 'number' && typeof id !== 'string') || typeof fn !== 'string') return;
        if (!Array.isArray(view.calls) || !view.calls.includes(fn)) {
          post({ protocol: PROTOCOL, type: 'result', id, ok: false, error: `\`${fn}\` is not available to this view` });
          return;
        }
        // One call at a time per frame: each runs Luau in the core, and a view
        // looping `call` must not queue unbounded work.
        if (calling) {
          post({ protocol: PROTOCOL, type: 'result', id, ok: false, error: 'busy' });
          return;
        }
        calling = true;
        call('plugin_view_call', { pluginId, function: fn, args: data.args ?? null })
          .then((value) => post({ protocol: PROTOCOL, type: 'result', id, ok: true, value }))
          .catch((err) => post({ protocol: PROTOCOL, type: 'result', id, ok: false, error: message(err) }))
          .finally(() => {
            calling = false;
          });
        break;
      }
      case 'resize': {
        const height = Number(data.height);
        if (!Number.isFinite(height)) return;
        const max = Math.max(MIN_HEIGHT, Math.round(window.innerHeight * 0.8));
        iframe.style.height = `${Math.min(max, Math.max(MIN_HEIGHT, Math.round(height)))}px`;
        break;
      }
      case 'state': {
        const summary = typeof data.summary === 'string' ? data.summary.slice(0, 500) : '';
        if (summary.trim()) sendState(summary);
        break;
      }
      case 'submit': {
        if (!answer || !onSubmit) return;
        submitted = true;
        iframe.setAttribute('aria-disabled', 'true');
        Promise.resolve()
          .then(() => onSubmit(data.answer))
          .then(() => onSubmitResult(true))
          .catch((err) => {
            submitted = false;
            iframe.removeAttribute('aria-disabled');
            onSubmitResult(false, message(err));
          });
        break;
      }
      default:
        break;
    }
  };

  let resultEl = null;
  function onSubmitResult(ok, text = '') {
    if (!resultEl) {
      resultEl = document.createElement('p');
      resultEl.className = 'view-status';
      resultEl.setAttribute('role', 'status');
      wrap.append(resultEl);
    }
    resultEl.textContent = ok ? 'Answer submitted.' : `Couldn’t send the answer: ${text}`;
    resultEl.classList.toggle('error', !ok);
    if (!ok && onError) onError(text);
  }

  const init = () => {
    if (destroyed) return;
    post({
      protocol: PROTOCOL,
      type: 'init',
      artifact: { artifactId: artifact.artifactId, data: artifact.data ?? {} },
      theme: themeTokens(),
      answer: Boolean(answer),
    });
  };

  // A theme switch while the view is open: send the new tokens.
  const dark = matchMedia('(prefers-color-scheme: dark)');
  const themeWatch = new MutationObserver(() => onTheme());
  const onTheme = () => {
    if (!wrap.isConnected) return destroy();
    post({ protocol: PROTOCOL, type: 'theme', theme: themeTokens() });
  };

  function destroy() {
    if (destroyed) return;
    destroyed = true;
    clearTimeout(stateTimer);
    window.removeEventListener('message', onMessage);
    dark.removeEventListener('change', onTheme);
    themeWatch.disconnect();
    iframe.remove();
    untrack(host);
  }
  const host = { el: wrap, destroy };

  iframe.addEventListener('load', init);
  window.addEventListener('message', onMessage);

  // The document comes from the shell's `pluginview` scheme with its own CSP
  // (VIEW_CSP in crates/app/src/lib.rs). A `srcdoc` frame would inherit the
  // app CSP (`script-src 'self'`), which blocks the view's inline script.
  let url;
  try {
    url = await call('plugin_view_url', { pluginId });
  } catch (err) {
    window.removeEventListener('message', onMessage);
    throw err;
  }
  iframe.src = url;
  dark.addEventListener('change', onTheme);
  themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

  // The caller inserts `el` as soon as this resolves, before a sweep can run.
  track(host);
  return host;
}
