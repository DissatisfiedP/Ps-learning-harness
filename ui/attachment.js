// Attachment chips and image thumbnails (build prompt W3).
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
