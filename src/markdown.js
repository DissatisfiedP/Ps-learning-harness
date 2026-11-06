// Markdown → sanitised HTML, with KaTeX math.
//
// Math is lifted out before Markdown parsing (so `_` and `*` inside formulas
// are not read as emphasis), and put back after sanitising as KaTeX output.
//
// Text is rendered block by block (see stream.js) and each block's HTML is
// cached by its source, so a streaming reply or a re-opened session only pays
// for blocks it has not rendered before. KaTeX output is cached by
// (tex, display). KaTeX itself is loaded on first need, not at startup.

import { marked } from './vendor/marked.esm.js';
import DOMPurify from './vendor/purify.es.mjs';
import { splitBlocks } from './stream.js';

marked.setOptions({ gfm: true, breaks: false });

// Code spans and fences are copied through untouched so `$` in code stays literal.
const CODE = /(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g;
// $$…$$, \[…\], \(…\), and $…$ where the inner text does not start or end
// with a space and the closing $ is not followed by a digit (so "$5 and $10"
// stays text).
const MATH = /\$\$([\s\S]+?)\$\$|\\\[([\s\S]+?)\\\]|\\\(([\s\S]+?)\\\)|\$(?=\S)((?:\\\$|[^$\n])+?)(?<=\S)\$(?!\d)/g;

const token = (i) => `@@MATH${i}@@`;

/** Small LRU over a Map (insertion order = recency). */
function lru(limit) {
  const map = new Map();
  return {
    get(key) {
      const value = map.get(key);
      if (value !== undefined) {
        map.delete(key);
        map.set(key, value);
      }
      return value;
    },
    set(key, value) {
      map.set(key, value);
      if (map.size > limit) map.delete(map.keys().next().value);
    },
  };
}

const blockCache = lru(1500);
const mathCache = lru(3000);

// ---------- KaTeX, loaded lazily ----------

const KATEX_URL = new URL('./vendor/katex/katex.min.js', import.meta.url).href;
const KATEX_CSS = new URL('./vendor/katex/katex.min.css', import.meta.url).href;
let cssLoad = null;
let jsLoad = null;

/** Inserts `el` (before `before`, or at the end of <head>) and waits for it. */
function load(el, before = null) {
  return new Promise((resolve, reject) => {
    el.onload = () => resolve();
    el.onerror = () => {
      el.remove();
      reject(new Error('KaTeX failed to load'));
    };
    document.head.insertBefore(el, before);
  });
}

/**
 * Loads KaTeX (script and stylesheet) once; resolves when `window.katex` is
 * ready and its CSS applies. Neither is on the startup path: Home renders no
 * math, so the stylesheet does not block the first paint. The stylesheet goes
 * before styles.css, whose `.katex` size override must keep winning. A failed
 * part is retried on the next call.
 */
export function ensureKatex() {
  cssLoad ??= load(
    Object.assign(document.createElement('link'), { rel: 'stylesheet', href: KATEX_CSS }),
    document.querySelector('link[rel="stylesheet"]'),
  ).catch((e) => {
    cssLoad = null;
    throw e;
  });
  if (!window.katex) {
    jsLoad ??= load(Object.assign(document.createElement('script'), { src: KATEX_URL })).catch((e) => {
      jsLoad = null;
      throw e;
    });
  }
  return Promise.all([cssLoad, jsLoad]);
}

/** Typeset one formula. `exact` is false when KaTeX was not available yet. */
function typeset(tex, display) {
  const key = (display ? 'D' : 'I') + tex;
  const hit = mathCache.get(key);
  if (hit !== undefined) return { html: hit, exact: true };
  if (!window.katex) return { html: escapeHtml(display ? `$$${tex}$$` : `$${tex}$`), exact: false };
  let html;
  try {
    html = window.katex.renderToString(tex, { displayMode: display, throwOnError: false, trust: false, strict: 'ignore' });
  } catch {
    html = escapeHtml(tex);
  }
  mathCache.set(key, html);
  return { html, exact: true };
}

// ---------- rendering ----------

function renderSource(source) {
  const formulas = [];
  const protectedText = source
    .split(CODE)
    .map((part, i) => {
      if (i % 2 === 1) return part;
      return part.replace(MATH, (_, block, bracket, paren, inline) => {
        const display = block !== undefined || bracket !== undefined;
        formulas.push({ tex: block ?? bracket ?? paren ?? inline, display });
        return token(formulas.length - 1);
      });
    })
    .join('');

  let html = DOMPurify.sanitize(marked.parse(protectedText), {
    FORBID_TAGS: ['style', 'form', 'input', 'button'],
  });
  let exact = true;
  if (formulas.length) {
    html = html.replace(/@@MATH(\d+)@@/g, (whole, i) => {
      const f = formulas[Number(i)];
      if (!f) return whole;
      const out = typeset(f.tex, f.display);
      exact &&= out.exact;
      return out.html;
    });
  }
  return { html, exact };
}

/**
 * HTML for one Markdown block. Cached by source unless `cache` is false (the
 * open tail of a stream changes every frame, so caching it only churns).
 */
export function renderBlock(source, cache = true) {
  const hit = blockCache.get(source);
  if (hit !== undefined) return hit;
  const { html, exact } = renderSource(source);
  // Output typeset without KaTeX is a placeholder; render again once it loads.
  if (cache && exact) blockCache.set(source, html);
  return html;
}

/** HTML for a whole Markdown text, rendered block by block through the cache. */
export function renderMarkdown(source) {
  if (!source) return '';
  let html = '';
  for (const block of splitBlocks(source)) html += renderBlock(block);
  return html;
}

export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

/** Rough plain text of a Markdown snippet, for short screen-reader messages. */
export function plainText(source, max = 140) {
  const text = String(source ?? '')
    .replace(/```[\s\S]*?(```|$)/g, ' ')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[*_`#>~|$\\]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}
