// Math in labels that are drawn as plain text: the Map's canvas and the plan
// diagram's SVG cannot typeset KaTeX, so `$x^2 + \alpha$` there reads as
// "x² + α". Only what has a plain-text form is converted (Greek letters,
// operators, arrows, super- and subscripts, fractions, roots); anything else
// keeps its TeX without the dollar signs. Chat text goes through KaTeX
// (markdown.js) instead; this is for short titles.

const SYMBOLS = {
  alpha: 'α', beta: 'β', gamma: 'γ', delta: 'δ', epsilon: 'ε', varepsilon: 'ε', zeta: 'ζ', eta: 'η', theta: 'θ',
  vartheta: 'ϑ', iota: 'ι', kappa: 'κ', lambda: 'λ', mu: 'μ', nu: 'ν', xi: 'ξ', pi: 'π', varpi: 'ϖ', rho: 'ρ',
  varrho: 'ϱ', sigma: 'σ', varsigma: 'ς', tau: 'τ', upsilon: 'υ', phi: 'φ', varphi: 'φ', chi: 'χ', psi: 'ψ',
  omega: 'ω', Gamma: 'Γ', Delta: 'Δ', Theta: 'Θ', Lambda: 'Λ', Xi: 'Ξ', Pi: 'Π', Sigma: 'Σ', Upsilon: 'Υ',
  Phi: 'Φ', Psi: 'Ψ', Omega: 'Ω',
  cdot: '·', times: '×', div: '÷', pm: '±', mp: '∓', ast: '∗', star: '⋆', circ: '∘', bullet: '•', oplus: '⊕',
  otimes: '⊗', le: '≤', leq: '≤', ge: '≥', geq: '≥', ne: '≠', neq: '≠', approx: '≈', equiv: '≡', sim: '∼',
  simeq: '≃', cong: '≅', propto: '∝', ll: '≪', gg: '≫', to: '→', rightarrow: '→', leftarrow: '←', gets: '←',
  leftrightarrow: '↔', Rightarrow: '⇒', Leftarrow: '⇐', Leftrightarrow: '⇔', iff: '⇔', implies: '⇒', mapsto: '↦',
  uparrow: '↑', downarrow: '↓', infty: '∞', partial: '∂', nabla: '∇', sum: 'Σ', prod: 'Π', int: '∫', oint: '∮',
  in: '∈', notin: '∉', ni: '∋', subset: '⊂', supset: '⊃', subseteq: '⊆', supseteq: '⊇', cup: '∪', cap: '∩',
  setminus: '∖', emptyset: '∅', varnothing: '∅', forall: '∀', exists: '∃', neg: '¬', lnot: '¬', land: '∧',
  lor: '∨', wedge: '∧', vee: '∨', angle: '∠', perp: '⊥', parallel: '∥', triangle: '△', degree: '°', hbar: 'ħ',
  ell: 'ℓ', Re: 'ℜ', Im: 'ℑ', aleph: 'ℵ', prime: '′', ldots: '…', cdots: '⋯', dots: '…', vdots: '⋮', therefore: '∴',
  because: '∵', langle: '⟨', rangle: '⟩', lceil: '⌈', rceil: '⌉', lfloor: '⌊', rfloor: '⌋', mid: '∣', lbrace: '{',
  rbrace: '}', quad: ' ', qquad: '  ', ',': ' ', ';': ' ', ':': ' ', '!': '', ' ': ' ', '%': '%', '&': '&', '#': '#',
  _: '_', '{': '{', '}': '}', '|': '‖', '\\': ' ',
};

const SUP = { 0: '⁰', 1: '¹', 2: '²', 3: '³', 4: '⁴', 5: '⁵', 6: '⁶', 7: '⁷', 8: '⁸', 9: '⁹', '+': '⁺', '-': '⁻', '−': '⁻', '=': '⁼', '(': '⁽', ')': '⁾', n: 'ⁿ', i: 'ⁱ', x: 'ˣ', y: 'ʸ', a: 'ᵃ', b: 'ᵇ', c: 'ᶜ', d: 'ᵈ', e: 'ᵉ', k: 'ᵏ', m: 'ᵐ', p: 'ᵖ', t: 'ᵗ', T: 'ᵀ' };
const SUB = { 0: '₀', 1: '₁', 2: '₂', 3: '₃', 4: '₄', 5: '₅', 6: '₆', 7: '₇', 8: '₈', 9: '₉', '+': '₊', '-': '₋', '−': '₋', '=': '₌', '(': '₍', ')': '₎', a: 'ₐ', e: 'ₑ', h: 'ₕ', i: 'ᵢ', j: 'ⱼ', k: 'ₖ', l: 'ₗ', m: 'ₘ', n: 'ₙ', o: 'ₒ', p: 'ₚ', r: 'ᵣ', s: 'ₛ', t: 'ₜ', u: 'ᵤ', v: 'ᵥ', x: 'ₓ' };

const MATH = /\$\$([^$]+?)\$\$|\\\[([\s\S]+?)\\\]|\\\(([\s\S]+?)\\\)|\$(?![\s\d])([^$\n]*?[^$\s\n])\$(?!\d)|\$(\d)\$(?!\d)/g;
const WRAPPERS = new Set(['text', 'textbf', 'textit', 'mathrm', 'mathbf', 'mathit', 'mathcal', 'mathbb', 'mathsf', 'mathtt', 'operatorname', 'boldsymbol', 'bm', 'vec', 'hat', 'bar', 'tilde', 'overline', 'underline', 'displaystyle']);
const DROPPED = new Set(['left', 'right', 'big', 'Big', 'bigg', 'Bigg', 'limits', 'nolimits']);

/** The `{…}` group starting at `s[i]` (which is `{`): its text and the index after it. */
function group(s, i) {
  let depth = 0;
  for (let j = i; j < s.length; j++) {
    if (s[j] === '\\') j++;
    else if (s[j] === '{') depth++;
    else if (s[j] === '}' && --depth === 0) return [s.slice(i + 1, j), j + 1];
  }
  return [s.slice(i + 1), s.length];
}

/** One argument: a `{group}`, a command, or a single character. */
function argument(s, i) {
  while (s[i] === ' ') i++;
  if (s[i] === '{') return group(s, i);
  if (s[i] === '\\') {
    const m = /^\\([A-Za-z]+|.)/.exec(s.slice(i));
    return m ? [m[0], i + m[0].length] : ['', i + 1];
  }
  return s[i] === undefined ? ['', i] : [s[i], i + 1];
}

function script(text, table, mark) {
  const chars = [...text];
  if (chars.every((c) => table[c] !== undefined)) return chars.map((c) => table[c]).join('');
  return chars.length === 1 ? `${mark}${text}` : `${mark}(${text})`;
}

const paren = (t) => (/^[\p{L}\p{N}.]+$/u.test(t) ? t : `(${t})`);

function tex(s) {
  let out = '';
  for (let i = 0; i < s.length; ) {
    const c = s[i];
    if (c === '\\') {
      const m = /^\\([A-Za-z]+|.)/s.exec(s.slice(i));
      if (!m) {
        i++;
        continue;
      }
      i += m[0].length;
      const name = m[1];
      if (name === 'frac' || name === 'dfrac' || name === 'tfrac') {
        const [a, j] = argument(s, i);
        const [b, k] = argument(s, j);
        i = k;
        out += `${paren(tex(a))}/${paren(tex(b))}`;
      } else if (name === 'sqrt') {
        let root = '';
        if (s[i] === '[') {
          const end = s.indexOf(']', i);
          root = s.slice(i + 1, end < 0 ? s.length : end);
          i = end < 0 ? s.length : end + 1;
        }
        const [a, j] = argument(s, i);
        i = j;
        const body = tex(a);
        const sign = root === '3' ? '∛' : root === '4' ? '∜' : '√';
        out += /^[\p{L}\p{N}]$/u.test(body) ? `${sign}${body}` : `${sign}(${body})`;
      } else if (WRAPPERS.has(name)) {
        const [a, j] = argument(s, i);
        i = j;
        out += tex(a);
      } else if (DROPPED.has(name)) {
        if (s[i] === '.') i++;
      } else if (name in SYMBOLS) {
        out += SYMBOLS[name];
      } else {
        out += name.length > 1 ? name : '';
      }
    } else if (c === '^' || c === '_') {
      const [a, j] = argument(s, i + 1);
      i = j;
      out += script(tex(a), c === '^' ? SUP : SUB, c);
    } else if (c === '{' || c === '}') {
      i++;
    } else if (c === '~') {
      out += ' ';
      i++;
    } else if (c === '-') {
      out += '−';
      i++;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

/** `text` with each `$…$`, `$$…$$`, `\(…\)` and `\[…\]` formula as plain Unicode. */
export function mathText(text) {
  const s = String(text ?? '');
  if (!s.includes('$') && !s.includes('\\')) return s;
  return s.replace(MATH, (_, a, b, c, d, e) => tex(a ?? b ?? c ?? d ?? e).replace(/\s+/g, ' ').trim());
}
