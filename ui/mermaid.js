// Mermaid diagrams (build prompt W1, D4).
//
// The library is a 2.7 MB bundle, so it is loaded only when the first
// fenced ```mermaid block appears. The vendored file is the IIFE build: it
// sets `globalThis.mermaid` only when run as a classic script (as a module
// its top-level `var` is module-scoped and the final assignment throws), so
// it is added as a `<script>` element, not `import()`ed. `startOnLoad` is off
// and `securityLevel` is "strict" (labels are typed by a model, not a
// person). A diagram that fails to parse keeps its source as a code block
// and gains a one-line error.

import DOMPurify from './vendor/purify.es.mjs';

const MERMAID_URL = new URL('./vendor/mermaid/mermaid.min.js', import.meta.url).href;
const SVG_PROFILE = { USE_PROFILES: { svg: true, svgFilters: true } };

let load = null;
let seq = 0;
let queue = Promise.resolve();
/** Figures we rendered, so a theme change can redraw them. */
const rendered = new Set();
/**
 * Render results by diagram source for the current theme: `{ svg }`
 * (sanitized) or `{ error }`. A message remounted by the lazy timeline
 * reuses its drawing instead of running Mermaid again.
 */
const cache = new Map();
const CACHE_MAX = 64;
let watching = false;

function tokens() {
  const cs = getComputedStyle(document.documentElement);
  const v = (name) => cs.getPropertyValue(`--${name}`).trim();
  return {
    background: v('paper'),
    primaryColor: v('raised'),
    primaryTextColor: v('ink'),
    primaryBorderColor: v('rule-strong'),
    lineColor: v('ink-2'),
    secondaryColor: v('raised'),
    secondaryTextColor: v('ink'),
    secondaryBorderColor: v('rule-strong'),
    tertiaryColor: v('paper'),
    tertiaryTextColor: v('ink-2'),
    tertiaryBorderColor: v('rule'),
    textColor: v('ink'),
    mainBkg: v('raised'),
    nodeBorder: v('rule-strong'),
    clusterBkg: v('raised'),
    clusterBorder: v('rule'),
    edgeLabelBackground: v('paper'),
    titleColor: v('ink'),
    fontFamily: v('font-ui'),
    fontSize: '14px',
  };
}

// Labels as SVG <text>, not HTML in <foreignObject>: the sanitizer's SVG
// profile drops foreignObject, which would leave every node empty.
function configure(mermaid) {
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: 'base',
    themeVariables: tokens(),
    htmlLabels: false,
    flowchart: { htmlLabels: false },
    sequence: { useMaxWidth: true },
    gantt: { useMaxWidth: true },
  });
}

/** Loads and configures Mermaid once. */
export function ensureMermaid() {
  if (globalThis.mermaid) return Promise.resolve(globalThis.mermaid);
  load ??= new Promise((resolve, reject) => {
    const el = document.createElement('script');
    el.src = MERMAID_URL;
    el.onload = () => resolve();
    el.onerror = () => {
      el.remove();
      reject(new Error('Mermaid did not load'));
    };
    document.head.append(el);
  })
    .then(() => {
      const mermaid = globalThis.mermaid;
      if (!mermaid) throw new Error('Mermaid did not load');
      configure(mermaid);
      return mermaid;
    })
    .catch((e) => {
      load = null;
      throw e;
    });
  return load;
}

function label(source) {
  const first = source.trim().split(/\s+/)[0] ?? 'Mermaid';
  const known = /^(flowchart|graph|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|pie|mindmap|timeline|journey|quadrantChart|xychart|block|architecture|gitGraph|C4)/i;
  return known.test(first) ? `${first} diagram` : 'Diagram';
}

function errorLine(err) {
  const text = String(err?.message ?? err ?? 'could not parse the diagram').replace(/\s+/g, ' ').trim();
  return text.length > 160 ? `${text.slice(0, 159)}…` : text;
}

function remember(source, entry) {
  cache.delete(source);
  cache.set(source, entry);
  if (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value);
}

function paint(figure, clean) {
  figure.innerHTML = clean;
  const node = figure.querySelector('svg');
  if (node) {
    node.setAttribute('role', 'presentation');
    node.removeAttribute('height');
    node.style.maxWidth = '100%';
    node.style.height = 'auto';
  }
}

/** Mermaid leaves its scratch container (`#d<id>`) behind when a render fails. */
function discard(id) {
  document.getElementById(`d${id}`)?.remove();
}

/** Renders `source` (or fails) and caches the outcome. */
async function draw(mermaid, source) {
  const id = `lh-mermaid-${++seq}`;
  try {
    const result = await mermaid.render(id, source);
    const entry = { svg: DOMPurify.sanitize(result.svg, SVG_PROFILE) };
    remember(source, entry);
    return entry;
  } catch (err) {
    discard(id);
    const entry = { error: errorLine(err) };
    remember(source, entry);
    return entry;
  }
}

function failed(pre, error) {
  // Static content in the timeline (aria-live off): no role=status, or every
  // remount of an old message would be announced again.
  const note = document.createElement('p');
  note.className = 'diagram-error';
  note.textContent = `Diagram error: ${error}`;
  pre.after(note);
  pre.dataset.diagramFailed = 'yes';
}

/** Puts a render outcome in place of (or under) the source block. */
function show(pre, source, entry) {
  if (entry.error !== undefined) {
    failed(pre, entry.error);
    return;
  }
  const figure = document.createElement('figure');
  figure.className = 'diagram';
  figure.setAttribute('aria-label', label(source));
  figure.dataset.mermaidSource = source;
  paint(figure, entry.svg);
  pre.replaceWith(figure);
  rendered.add(figure);
}

function pruneRendered() {
  for (const figure of rendered) if (!figure.isConnected) rendered.delete(figure);
}

function idle(fn) {
  const cb = window.requestIdleCallback ?? ((f) => setTimeout(f, 32));
  cb(fn, { timeout: 500 });
}

function watchTheme() {
  if (watching) return;
  watching = true;
  const redraw = () => {
    cache.clear();
    pruneRendered();
    if (!rendered.size) return;
    queue = queue.then(async () => {
      const mermaid = await ensureMermaid();
      configure(mermaid);
      for (const figure of [...rendered]) {
        const source = figure.dataset.mermaidSource ?? '';
        if (!source || !figure.isConnected) continue;
        const entry = await draw(mermaid, source);
        // Keep the previous drawing if the redraw fails.
        if (entry.svg !== undefined) paint(figure, entry.svg);
      }
    }).catch(() => {});
  };
  try {
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', redraw);
  } catch {
    // Older engines: no live theme redraw.
  }
  new MutationObserver((records) => {
    if (records.some((r) => r.attributeName === 'data-theme')) redraw();
  }).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
}

/**
 * Replaces every finished `pre > code.language-mermaid` under `root` with a
 * figure. A block inside a streaming reply's open tail (`.tail`) is still
 * being written and is skipped. A source drawn before shows its cached
 * drawing at once; a new one is rendered in an idle callback, never in a
 * stream frame. Idempotent: a block is claimed once, and failures are not
 * retried.
 */
export function renderDiagrams(root) {
  if (!root?.querySelectorAll) return;
  pruneRendered();
  const blocks = [];
  for (const code of root.querySelectorAll('pre > code.language-mermaid')) {
    const pre = code.parentElement;
    if (!pre || pre.dataset.diagram || code.dataset.diagramSeen || code.closest('.tail')) continue;
    const source = code.textContent.trim();
    if (!source) continue;
    code.dataset.diagramSeen = 'yes';
    const hit = cache.get(source);
    if (hit) show(pre, source, hit);
    else blocks.push({ pre, code, source });
  }
  watchTheme();
  if (!blocks.length) return;
  idle(() => {
    queue = queue
      .then(async () => {
        const mermaid = await ensureMermaid();
        for (const { pre, code, source } of blocks) {
          if (!pre.isConnected) {
            // Unmounted before its turn: a later pass may claim it again.
            delete code.dataset.diagramSeen;
            continue;
          }
          const entry = cache.get(source) ?? (await draw(mermaid, source));
          if (pre.isConnected) show(pre, source, entry);
          else delete code.dataset.diagramSeen;
        }
      })
      .catch((err) => {
        for (const { pre } of blocks) {
          if (pre.isConnected && !pre.dataset.diagramFailed) failed(pre, errorLine(err));
        }
      });
  });
}
