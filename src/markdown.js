// Markdown → sanitised HTML, with KaTeX math.
//
// Math is lifted out before Markdown parsing (so `_` and `*` inside formulas
// are not read as emphasis), and put back after sanitising as KaTeX output.

import { marked } from './vendor/marked.esm.js';
import DOMPurify from './vendor/purify.es.mjs';

marked.setOptions({ gfm: true, breaks: false });

// Code spans and fences are copied through untouched so `$` in code stays literal.
const CODE = /(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g;
// $$…$$, \[…\], \(…\), and $…$ where the inner text does not start or end
// with a space and the closing $ is not followed by a digit (so "$5 and $10"
// stays text).
const MATH = /\$\$([\s\S]+?)\$\$|\\\[([\s\S]+?)\\\]|\\\(([\s\S]+?)\\\)|\$(?=\S)((?:\\\$|[^$\n])+?)(?<=\S)\$(?!\d)/g;

const token = (i) => `@@MATH${i}@@`;

export function render(source) {
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
  if (formulas.length) {
    html = html.replace(/@@MATH(\d+)@@/g, (whole, i) => {
      const f = formulas[Number(i)];
      return f ? typeset(f.tex, f.display) : whole;
    });
  }
  return html;
}

function typeset(tex, display) {
  if (!window.katex) return escapeHtml(display ? `$$${tex}$$` : `$${tex}$`);
  try {
    return window.katex.renderToString(tex, { displayMode: display, throwOnError: false, trust: false, strict: 'ignore' });
  } catch {
    return escapeHtml(tex);
  }
}

export function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}
