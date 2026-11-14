// Image sets for the `image-set` plugin's drills (docs/plugins.md §12):
// labelled folders (one subfolder of images per label) copied into the data
// repo's `image-sets/`, so they sync to every device. Settings → Libraries
// lists them, imports a folder (desktop) and removes a set.

import { call, message } from './api.js';
import { confirmDialog } from './dialog.js';

let els = null;
let busy = false;

function setStatus(text, error = false) {
  if (!els?.status) return;
  els.status.textContent = text;
  els.status.classList.toggle('error', error);
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

/** "crow 12 · raven 9" */
export function labelsText(set) {
  return (set.labels ?? []).map((l) => `${l.label} ${l.count}`).join(' · ');
}

function setRow(set) {
  const row = document.createElement('div');
  row.className = 'library image-set';
  const head = document.createElement('div');
  head.className = 'plugin-head';
  const title = document.createElement('span');
  title.className = 'plugin-title';
  title.textContent = set.title;
  const id = document.createElement('span');
  id.className = 'plugin-version';
  id.textContent = set.id;
  head.append(title, id);
  const desc = document.createElement('p');
  desc.className = 'plugin-desc';
  desc.textContent = `${plural(set.size, 'image')}, ${plural(set.labels.length, 'label')}: ${labelsText(set)}`;
  const actions = document.createElement('div');
  actions.className = 'actions';
  const remove = document.createElement('button');
  remove.type = 'button';
  remove.className = 'btn text';
  remove.textContent = 'Remove';
  remove.addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: `Remove ${set.title}?`,
      text: 'Its images leave every device at the next sync. The folder you imported it from is not touched.',
      confirmLabel: 'Remove image set',
      cancelLabel: 'Keep it',
    });
    if (!ok) return;
    try {
      await call('image_set_remove', { id: set.id });
      setStatus(`Removed ${set.title}.`);
      await renderImageSetSettings();
    } catch (e) {
      setStatus(message(e), true);
    }
  });
  actions.append(remove);
  row.append(head, desc, actions);
  return row;
}

/** Re-reads the sets into Settings → Libraries → Image sets. */
export async function renderImageSetSettings() {
  if (!els?.list) return;
  let sets;
  try {
    sets = await call('image_sets');
  } catch (e) {
    setStatus(`Couldn’t load image sets: ${message(e)}`, true);
    return;
  }
  els.list.textContent = '';
  if (!sets.length) {
    const p = document.createElement('p');
    p.className = 'help';
    p.textContent = 'No image sets yet.';
    els.list.append(p);
    return;
  }
  for (const set of sets) els.list.append(setRow(set));
}

async function importSet() {
  if (busy) return;
  let path;
  try {
    path = await call('pick_folder');
  } catch (e) {
    if (!/no folder chosen/i.test(message(e))) setStatus(message(e), true);
    return;
  }
  busy = true;
  els.importBtn.disabled = true;
  setStatus('Copying images…');
  try {
    const r = await call('image_set_import', { path, name: null });
    const skipped = r.skipped ? ` Skipped ${plural(r.skipped, 'file')} (not an image, or over 5 MB).` : '';
    setStatus(`${r.set.title}: ${plural(r.set.size, 'image')} in ${plural(r.set.labels.length, 'label')}.${skipped}`);
    await renderImageSetSettings();
  } catch (e) {
    setStatus(`Couldn’t import the folder: ${message(e)}`, true);
  } finally {
    busy = false;
    els.importBtn.disabled = false;
  }
}

export function initImageSetSettings() {
  els = {
    list: document.getElementById('image-set-list'),
    status: document.getElementById('image-set-status'),
    importBtn: document.getElementById('image-set-import'),
  };
  els.importBtn?.addEventListener('click', importSet);
}
