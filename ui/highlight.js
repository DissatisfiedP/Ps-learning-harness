// Syntax highlighting for finished code blocks (build prompt W1).
//
// Prism core and one grammar per language are loaded on demand: startup
// loads no grammar, and only the languages a reply actually uses are fetched.
// Highlighting runs once per block, after the block is final (a completed
// block in the streaming cache, or a finished message), never per delta.
//
// Unknown or plain-text languages are left as plain code; no error.

const BASE = new URL('./vendor/prism/', import.meta.url).href;

// Fence language → Prism grammar, plus the grammars it needs first.
const LANGUAGES = {
  bash: { name: 'bash' },
  sh: { name: 'bash' },
  shell: { name: 'bash' },
  zsh: { name: 'bash' },
  console: { name: 'bash' },
  python: { name: 'python' },
  py: { name: 'python' },
  javascript: { name: 'javascript', deps: ['clike'] },
  js: { name: 'javascript', deps: ['clike'] },
  jsx: { name: 'javascript', deps: ['clike'] },
  mjs: { name: 'javascript', deps: ['clike'] },
  typescript: { name: 'typescript', deps: ['clike', 'javascript'] },
  ts: { name: 'typescript', deps: ['clike', 'javascript'] },
  rust: { name: 'rust' },
  rs: { name: 'rust' },
  c: { name: 'c', deps: ['clike'] },
  h: { name: 'c', deps: ['clike'] },
  cpp: { name: 'cpp', deps: ['clike', 'c'] },
  'c++': { name: 'cpp', deps: ['clike', 'c'] },
  cc: { name: 'cpp', deps: ['clike', 'c'] },
  hpp: { name: 'cpp', deps: ['clike', 'c'] },
  java: { name: 'java', deps: ['clike'] },
  json: { name: 'json' },
  jsonc: { name: 'json' },
  toml: { name: 'toml' },
  yaml: { name: 'yaml' },
  yml: { name: 'yaml' },
  sql: { name: 'sql' },
  html: { name: 'markup' },
  xml: { name: 'markup' },
  svg: { name: 'markup' },
  markup: { name: 'markup' },
  css: { name: 'css' },
  markdown: { name: 'markdown', deps: ['markup'] },
  md: { name: 'markdown', deps: ['markup'] },
  nix: { name: 'nix' },
  lua: { name: 'lua' },
  luau: { name: 'lua' },
  latex: { name: 'latex' },
  tex: { name: 'latex' },
  diff: { name: 'diff' },
  patch: { name: 'diff' },
};

const loading = new Map();
let core = null;

function script(src) {
  return new Promise((resolve, reject) => {
    const el = document.createElement('script');
    el.src = src;
    el.onload = () => resolve();
    el.onerror = () => {
      el.remove();
      reject(new Error(`Prism failed to load ${src}`));
    };
    document.head.append(el);
  });
}

export function ensurePrism() {
  if (window.Prism?.highlight) return Promise.resolve(window.Prism);
  // Manual mode: without it Prism highlights the whole document on load,
  // including a code block that is still streaming.
  window.Prism ??= { manual: true };
  core ??= script(`${BASE}prism-core.min.js`)
    .then(() => window.Prism)
    .catch((e) => {
      core = null;
      throw e;
    });
  return core;
}

function ensureGrammar(name) {
  if (window.Prism?.languages?.[name]) return Promise.resolve();
  let pending = loading.get(name);
  if (!pending) {
    pending = (async () => {
      for (const dep of LANGUAGES[name]?.deps ?? []) await ensureGrammar(dep);
      await ensurePrism();
      await script(`${BASE}components/prism-${name}.min.js`);
    })().catch((e) => {
      loading.delete(name);
      throw e;
    });
    loading.set(name, pending);
  }
  return pending;
}

function idle(fn) {
  const cb = window.requestIdleCallback ?? ((f) => setTimeout(f, 32));
  cb(fn, { timeout: 250 });
}

function languageOf(code) {
  const cls = code.className || '';
  const m = /\blanguage-([\w+#-]+)/.exec(cls);
  if (!m) return null;
  const key = m[1].toLowerCase();
  if (key === 'mermaid' || key === 'math' || key === 'text' || key === 'plain' || key === 'plaintext') return null;
  return LANGUAGES[key]?.name ?? null;
}

function apply(code, lang) {
  const grammar = window.Prism?.languages?.[lang];
  if (!grammar) return;
  try {
    const source = code.textContent;
    code.innerHTML = window.Prism.highlight(source, grammar, lang);
  } catch {
    // A grammar edge case must never cost the learner the code.
  }
  code.dataset.highlighted = 'yes';
}

/**
 * Highlights every finished code block under `root`. Idempotent and safe to
 * call on the same subtree repeatedly (each block is claimed once). A block
 * inside a streaming reply's open tail (`.tail`) is still being written and
 * is left for `finalize`. The work is deferred to an idle callback so a
 * stream frame is never spent on it.
 */
export function highlightAll(root) {
  if (!root?.querySelectorAll) return;
  const blocks = [];
  for (const code of root.querySelectorAll('pre > code[class*="language-"]')) {
    if (code.dataset.highlighted || code.closest('.tail')) continue;
    const lang = languageOf(code);
    if (!lang) {
      code.dataset.highlighted = 'plain';
      continue;
    }
    code.dataset.highlighted = 'pending';
    blocks.push({ code, lang });
  }
  if (!blocks.length) return;
  idle(() => {
    for (const { code, lang } of blocks) {
      if (!code.isConnected) continue;
      ensureGrammar(lang)
        .then(() => {
          if (code.isConnected) apply(code, lang);
        })
        .catch(() => {
          if (code.isConnected) code.dataset.highlighted = 'plain';
        });
    }
  });
}
