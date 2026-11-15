// Attachment chips and image thumbnails.
//
// Images live in the data repo; `attachment_data` returns a data URL for a
// chip or a reopened learner message. Data URLs are cached for the window.

import { call } from './api.js';

const thumbs = new Map();

export function attachmentName(a) {
  if (a?.name) return a.name;
  const path = String(a?.path ?? '');
  const base = path.split(/[\\/]/).filter(Boolean).at(-1);
  return base || (a?.kind === 'folder' ? 'Folder' : 'File');
}

export function kindLabel(kind) {
  if (kind === 'image') return 'Image';
  if (kind === 'folder') return 'Folder';
  if (kind === 'library') return 'Reference library';
  return 'File';
}

export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return '';
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

function meta(a) {
  const size = a?.kind === 'folder' ? '' : formatBytes(a?.bytes);
  return [kindLabel(a?.kind), size].filter(Boolean).join(' · ');
}

function loadThumb(a, img) {
  if (a?.kind !== 'image') return;
  const cached = thumbs.get(a.path);
  if (cached) {
    img.src = cached;
    img.hidden = false;
    return;
  }
  call('attachment_data', { path: a.path })
    .then((url) => {
      if (typeof url !== 'string' || !url) return;
      thumbs.set(a.path, url);
      if (img.isConnected) {
        img.src = url;
        img.hidden = false;
      }
    })
    .catch(() => {
      // No bytes available: the chip still names the image.
    });
}

/**
 * One chip. `thumb` is a data URL when the composer already has one;
 * otherwise an image loads its thumbnail asynchronously.
 */
export function attachmentChip(a, { thumb = null, onRemove = null, onUseWorkspace = null } = {}) {
  const li = document.createElement('li');
  li.className = 'chip';
  li.dataset.kind = a.kind;

  if (a.kind === 'image') {
    const img = document.createElement('img');
    img.className = 'chip-thumb';
    img.alt = '';
    img.setAttribute('aria-hidden', 'true');
    img.hidden = true;
    if (thumb) {
      img.src = thumb;
      img.hidden = false;
    } else {
      loadThumb(a, img);
    }
    li.append(img);
  }

  const text = document.createElement('span');
  text.className = 'chip-text';
  text.append(
    Object.assign(document.createElement('span'), { className: 'chip-name', textContent: attachmentName(a) }),
    Object.assign(document.createElement('span'), { className: 'chip-kind', textContent: meta(a) }),
  );
  li.append(text);

  if (onUseWorkspace && a.kind === 'folder') {
    const use = document.createElement('button');
    use.type = 'button';
    use.className = 'btn text chip-action';
    use.textContent = 'Use as workspace';
    use.setAttribute('aria-label', `Use as workspace: ${attachmentName(a)}`);
    use.addEventListener('click', () => onUseWorkspace(a));
    li.append(use);
  }

  if (onRemove) {
    const x = document.createElement('button');
    x.type = 'button';
    x.className = 'chip-remove';
    x.textContent = '×';
    x.setAttribute('aria-label', `Remove ${attachmentName(a)}`);
    x.addEventListener('click', () => onRemove(a));
    li.append(x);
  }
  return li;
}

/** A read-only list of a learner message's attachments. */
export function attachmentList(attachments) {
  const ul = document.createElement('ul');
  ul.className = 'chips message-chips';
  ul.setAttribute('aria-label', 'Attachments');
  for (const a of attachments ?? []) ul.append(attachmentChip(a));
  if (!ul.children.length) return null;
  return ul;
}

/**
 * A drawing sent with an answer, `{ in: "reasoning" | "answer", path }`, as
 * a small captioned picture. The image loads from the data repo.
 */
export function sketchFigure(sketch) {
  const figure = document.createElement('figure');
  figure.className = 'you-sketch';
  const img = document.createElement('img');
  img.alt = `Your sketch, sent with your ${sketch.in}`;
  img.hidden = true;
  loadThumb({ kind: 'image', path: sketch.path }, img);
  const caption = document.createElement('figcaption');
  caption.textContent = `Sketch · with your ${sketch.in}`;
  figure.append(img, caption);
  return figure;
}

/**
 * A scratch-pad drawing waiting to go with `label` ("your reasoning", "your
 * answer to problem 2"): a small picture with a Remove button. Removing
 * tells the page (`sketch-discard`, detail: the attachment) so the staged
 * image can be dropped. `set` returns the drawing it replaced; `restore`
 * loads one that was kept (a draft) and says whether it could.
 */
export function sketchSlot(label) {
  const el = document.createElement('figure');
  el.className = 'ex-sketch';
  el.hidden = true;
  const img = document.createElement('img');
  img.alt = `Your sketch, sent with ${label}`;
  const bar = document.createElement('figcaption');
  const remove = document.createElement('button');
  remove.type = 'button';
  remove.className = 'btn text';
  remove.textContent = 'Remove sketch';
  const text = document.createElement('span');
  text.textContent = `Sketch goes with ${label}`;
  bar.append(text, remove);
  el.append(img, bar);
  let attachment = null;
  const set = (next, url) => {
    const previous = attachment;
    attachment = next;
    img.src = url;
    el.hidden = false;
    return previous;
  };
  remove.addEventListener('click', () => {
    const gone = attachment;
    attachment = null;
    el.hidden = true;
    img.removeAttribute('src');
    if (gone) el.dispatchEvent(new CustomEvent('sketch-discard', { bubbles: true, detail: gone }));
  });
  return {
    el,
    get: () => attachment,
    set,
    restore: (kept) =>
      call('attachment_data', { path: kept.path })
        .then((url) => {
          if (typeof url !== 'string' || !url) return false;
          set(kept, url);
          return true;
        })
        .catch(() => false),
  };
}
