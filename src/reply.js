// Message views: learner bubbles, finished tutor replies, and the live
// (streaming) tutor reply that renders only its open tail block per frame.

import { renderBlock, renderMarkdown } from './markdown.js';
import { createSplitter } from './stream.js';

const BRAND_SVG = '<svg viewBox="0 0 24 24"><path d="M12 3 2 8l10 5 8-4v6h2V8L12 3Z"/><path d="M6 11.2V16c0 1.7 2.7 3 6 3s6-1.3 6-3v-4.8l-6 3-6-3Z" opacity=".55"/></svg>';

const ENDINGS = {
  'max-tokens': 'Hit the length limit',
  refusal: 'Declined',
  interrupted: 'Stopped',
};

const TYPING = '<span class="typing" aria-label="Tutor is typing"><i></i><i></i><i></i></span>';

export function userEl(text) {
  const el = document.createElement('div');
  el.className = 'msg user';
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  bubble.textContent = text;
  el.append(bubble);
  return el;
}

function shell() {
  const el = document.createElement('div');
  el.className = 'msg assistant';
  el.innerHTML = `
    <div class="meta"><span class="avatar" aria-hidden="true">${BRAND_SVG}</span><span class="who">Tutor</span><span class="model"></span></div>
    <details class="thinking" hidden><summary>Reasoning</summary><div class="thinking-body"></div></details>
    <div class="prose"></div>`;
  return {
    el,
    meta: el.querySelector('.meta'),
    model: el.querySelector('.model'),
    thinking: el.querySelector('.thinking'),
    thinkingBody: el.querySelector('.thinking-body'),
    prose: el.querySelector('.prose'),
  };
}

function setMeta(parts, msg) {
  parts.model.textContent = msg.model ? `· ${msg.model}` : '';
  parts.meta.querySelector('.badge')?.remove();
  if (msg.ending) {
    const badge = document.createElement('span');
    badge.className = `badge${msg.ending === 'interrupted' ? '' : ' warn'}`;
    badge.textContent = ENDINGS[msg.ending] ?? msg.ending;
    parts.meta.append(badge);
  }
}

/** A finished tutor reply. */
export function assistantEl(msg) {
  const parts = shell();
  setMeta(parts, msg);
  if (msg.thinking) {
    parts.thinking.hidden = false;
    parts.thinkingBody.textContent = msg.thinking;
  }
  parts.prose.innerHTML = renderMarkdown(msg.content ?? '');
  return parts.el;
}

/**
 * A streaming tutor reply. Feed it deltas with `addText` / `addThinking`; it
 * paints at most once per animation frame. Completed Markdown blocks are
 * rendered once and inserted before the open tail; only the tail re-renders.
 * Thinking is plain text, so its deltas are appended to one text node.
 */
export class LiveReply {
  constructor({ onPaint } = {}) {
    this.parts = shell();
    this.el = this.parts.el;
    this.el.classList.add('live');
    this.text = '';
    this.thinking = '';
    this.onPaint = onPaint;
    this.splitter = createSplitter();
    this.tailEl = null;
    this.tailText = '';
    this.thinkingNode = null;
    this.thinkingShown = 0;
    this.frame = 0;
    // Set when the view joins a turn already in progress: its earlier deltas
    // were missed, so it shows only the typing indicator until the message op.
    this.midway = false;
    this.parts.prose.innerHTML = TYPING;
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
    this.schedule();
  }

  schedule() {
    if (!this.frame) this.frame = requestAnimationFrame(() => this.paint());
  }

  paint() {
    this.frame = 0;
    this.paintThinking();
    this.paintText();
    this.onPaint?.();
  }

  paintThinking() {
    if (this.thinking.length === this.thinkingShown) return;
    if (!this.thinkingNode) {
      this.parts.thinking.hidden = false;
      this.thinkingNode = document.createTextNode('');
      this.parts.thinkingBody.append(this.thinkingNode);
    }
    this.thinkingNode.appendData(this.thinking.slice(this.thinkingShown));
    this.thinkingShown = this.thinking.length;
  }

  paintText() {
    if (!this.text) return;
    const prose = this.parts.prose;
    const { completed, tail, reset } = this.splitter.push(this.text);
    if (!this.tailEl || reset) {
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
   * Turns the live bubble into the finished message `msg` and returns its
   * element. When the persisted text equals what was streamed, the rendered
   * blocks are kept and only the tail is closed.
   */
  finalize(msg) {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
    const parts = this.parts;
    this.el.classList.remove('live');
    setMeta(parts, msg);
    const content = msg.content ?? '';
    if (content === this.text && this.tailEl) {
      this.paintText();
      const tail = this.tailText.replace(/\s+$/, '');
      if (tail) this.tailEl.insertAdjacentHTML('beforebegin', renderBlock(tail));
      this.tailEl.remove();
    } else {
      parts.prose.innerHTML = renderMarkdown(content);
    }
    const thinking = msg.thinking ?? '';
    parts.thinking.hidden = !thinking;
    if (thinking !== this.thinking) parts.thinkingBody.textContent = thinking;
    else this.paintThinking();
    return this.el;
  }

  /** Stops painting; the caller removes or keeps the element. */
  dispose() {
    cancelAnimationFrame(this.frame);
    this.frame = 0;
  }
}
