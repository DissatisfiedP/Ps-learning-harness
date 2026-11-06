// Splits growing Markdown into blocks so a streaming reply re-renders only its
// open tail. Pure: no DOM, no imports.
//
// A block is complete once a blank line follows it and the next line starts
// at column 0 (an indented line after a blank still belongs to the block:
// list continuations, indented code). Blank lines inside a ``` / ~~~ fence or
// a `$$` display-math block never end a block.

const FENCE = /^ {0,3}(`{3,}|~{3,})/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})\s*$/;
const INLINE_CODE = /`[^`]*`/g;

/** Number of `$$` delimiters on a line, ignoring inline code spans. */
function displayDelims(line) {
  if (!line.includes('$$')) return 0;
  return line.replace(INLINE_CODE, '').split('$$').length - 1;
}

/**
 * Incremental splitter. `push(text)` takes the whole text so far (normally
 * the previous text plus a delta) and returns the blocks completed since the
 * last push plus the current open tail. Each completed block is returned
 * exactly once; if `text` is not an extension of the previous text the
 * splitter starts over (`reset: true`) and returns every block again.
 */
export function createSplitter() {
  let text = '';
  let pos = 0; // start of the next unscanned line
  let start = 0; // start of the open block
  let fence = null; // opening fence marker while inside a fence
  let math = false; // inside $$ … $$
  let blank = false; // previous line was a blank line outside fence/math

  const restart = () => {
    text = '';
    pos = 0;
    start = 0;
    fence = null;
    math = false;
    blank = false;
  };

  return {
    push(next) {
      let reset = false;
      if (next.length < text.length || !next.startsWith(text)) {
        restart();
        reset = true;
      }
      text = next;
      const completed = [];
      let nl;
      while ((nl = text.indexOf('\n', pos)) !== -1) {
        const line = text.slice(pos, nl);
        const isBlank = line.trim() === '';
        if (blank && !isBlank && !fence && !math && !/^\s/.test(line)) {
          const block = text.slice(start, pos).replace(/\s+$/, '');
          if (block) completed.push(block);
          start = pos;
        }
        if (fence) {
          const m = FENCE_CLOSE.exec(line);
          if (m && m[1][0] === fence[0] && m[1].length >= fence.length) fence = null;
        } else {
          const m = FENCE.exec(line);
          if (m) fence = m[1];
          else if (displayDelims(line) % 2 === 1) math = !math;
        }
        blank = isBlank && !fence && !math;
        pos = nl + 1;
      }
      return { completed, tail: text.slice(start), reset };
    },
  };
}

/** One-shot split of a finished text: completed blocks plus the tail block. */
export function splitBlocks(text) {
  const { completed, tail } = createSplitter().push(text);
  const trimmed = tail.replace(/\s+$/, '');
  if (trimmed) completed.push(trimmed);
  return completed;
}
