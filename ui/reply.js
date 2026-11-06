// Flow entries for the conversation: learner notes, finished tutor replies,
// and the live (streaming) tutor reply that renders only its open tail block
// per frame. Nothing here scrolls: the text grows in place.

import { renderBlock, renderMarkdown } from './markdown.js';
import { createSplitter } from './stream.js';

const ENDINGS = {
  interrupted: 'Stopped here.',
  'max-tokens': 'The reply was cut off at the length limit.',
  refusal: 'The tutor declined to continue.',
};

const WRITING = 'Writing…';
const MIDWAY = 'The tutor is writing. The reply will appear when it’s finished.';
const SLOW = 'Still working. You can stop it below.';
const SLOW_MS = 30000;

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/** A learner entry: small-caps "You", then the text as written. */
export function userEl(text) {
  const entry = el('div', 'entry user');
  entry.append(el('p', 'you-label', 'You'), el('p', 'you-text', text));
  return entry;
}

function endingEl(ending) {
  return ending ? el('p', 'ending', ENDINGS[ending] ?? `The reply ended early (${ending}).`) : null;
}

/** Practice only: the tutor's reasoning, closed, after the reply text. */
function workingEl() {
  const d = el('details', 'working');
  d.append(el('summary', '', 'Tutor’s working'), el('div', 'working-body'));
  return d;
}

/** A finished tutor reply. `showThinking` is false in Learn (spoiler risk). */
export function assistantEl(msg, { showThinking = false } = {}) {
  const entry = el('div', 'entry tutor');
  const prose = el('div', 'prose');
  prose.innerHTML = renderMarkdown(msg.content ?? '');
  entry.append(prose);
  const end = endingEl(msg.ending);
  if (end) entry.append(end);
  if (showThinking && msg.thinking) {
    const w = workingEl();
    w.lastChild.textContent = msg.thinking;
    entry.append(w);
  }
  return entry;
}

/**
 * A streaming tutor reply. Feed it deltas with `addText` / `addThinking`; it
 * paints at most once per animation frame. Completed Markdown blocks are
 * rendered once and inserted before the open tail; only the tail re-renders.
 * Thinking is always accumulated (so `finalize` can compare it) but painted
 * only when `showThinking` is set (Practice).
 */
export class LiveReply {
  constructor({ showThinking = false } = {}) {
    this.el = el('div', 'entry tutor streaming');
    this.prose = el('div', 'prose');
    this.status = el('p', 'writing', WRITING);
    this.el.append(this.status, this.prose);
    this.showThinking = showThinking;
    this.text = '';
    this.thinking = '';
    this.splitter = createSplitter();
    this.tailEl = null;
    this.tailText = '';
    this.working = null;
    this.thinkingNode = null;
    this.thinkingShown = 0;
    this.frame = 0;
    // Blocks painted before KaTeX was loaded hold raw TeX; finalize re-renders.
    this.rawMath = false;
    // Set when the view joins a turn already in progress: its earlier deltas
    // were missed, so it shows only a status line until the message op.
    this._midway = false;
    this.slowTimer = setTimeout(() => {
      this.slowTimer = 0;
      if (!this.text && !this._midway && this.status.isConnected) this.status.after(el('p', 'writing', SLOW));
    }, SLOW_MS);
  }

  get midway() {
    return this._midway;
  }

  set midway(on) {
    this._midway = on;
    this.status.textContent = on ? MIDWAY : WRITING;
  }

  get hasContent() {
    return Boolean(this.text.trim());
  }

  addText(delta) {
    this.text += delta;
    this.schedule();
  }

  addThinking(delta) {
    this.thinking += delta;
    if (this.showThinking) this.schedule();
  }

  schedule() {
    if (!this.frame) this.frame = requestAnimationFrame(() => this.paint());
  }

  paint() {
    this.frame = 0;
    if (this.showThinking) this.paintThinking();
    this.paintText();
  }

  clearStatus() {
    clearTimeout(this.slowTimer);
    this.slowTimer = 0;
    if (!this.status.isConnected) return;
    while (this.status.nextSibling !== this.prose) this.status.nextSibling.remove();
    this.status.remove();
  }

  paintThinking() {
    if (this.thinking.length === this.thinkingShown) return;
    if (!this.working) {
      this.working = workingEl();
      this.thinkingNode = document.createTextNode('');
      this.working.lastChild.append(this.thinkingNode);
      this.el.append(this.working);
    }
    this.thinkingNode.appendData(this.thinking.slice(this.thinkingShown));
    this.thinkingShown = this.thinking.length;
  }

  paintText() {
    if (!this.text) return;
    const prose = this.prose;
    if (!window.katex) this.rawMath = true;
    const { completed, tail, reset } = this.splitter.push(this.text);
    if (!this.tailEl || reset) {
      this.clearStatus();
      prose.textContent = '';
      this.tailEl = document.createElement('div');
      this.tailEl.className = 'tail';
      prose.append(this.tailEl);
      this.tailText = '';
    }
    if (completed.length) {
      let html = '';
      for (const block of completed) html += renderBlock(block);
      this.tailEl.insertAdjacentHTML('beforebegin', html);
    }
    if (tail !== this.tailText) {
      this.tailText = tail;
      this.tailEl.innerHTML = tail.trim() ? renderBlock(tail, false) : '';
    }
  }

  /**
   * Turns the live reply into the finished message `msg` and returns its
   * element. When the persisted text equals what was streamed, the rendered
   * blocks are kept and only the tail is closed.
   */
  finalize(msg) {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
    this.clearStatus();
    this.el.classList.remove('streaming');
    const content = msg.content ?? '';
    if (content === this.text && this.tailEl && !this.rawMath) {
      this.paintText();
      const tail = this.tailText.replace(/\s+$/, '');
      if (tail) this.tailEl.insertAdjacentHTML('beforebegin', renderBlock(tail));
      this.tailEl.remove();
    } else {
      this.prose.innerHTML = renderMarkdown(content);
    }
    const end = endingEl(msg.ending);
    if (end) this.prose.after(end);
    if (this.showThinking) {
      const thinking = msg.thinking ?? '';
      if (thinking !== this.thinking) {
        if (!this.working && thinking) {
          this.working = workingEl();
          this.el.append(this.working);
        }
        if (this.working) this.working.lastChild.textContent = thinking;
      } else this.paintThinking();
      if (this.working && !thinking) this.working.remove();
    }
    return this.el;
  }

  /** Stops painting; the caller removes or keeps the element. */
  dispose() {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
    clearTimeout(this.slowTimer);
    this.slowTimer = 0;
  }
}
