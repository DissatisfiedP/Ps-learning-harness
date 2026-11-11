// Reference libraries (architecture §4.27, ui-design §7.7.8): folders of
// references imported on a desktop. Only their extracted text syncs; the
// copied originals stay on the importing device. The primary way in is the
// Home composer's '+' (composer.js); Settings → Libraries lists, refreshes,
// edits and removes them.

import { call, channel, message } from './api.js';
import { confirmDialog } from './dialog.js';

const KIND_LABELS = {
  'question-guide': 'question-writing guide',
  'question-bank': 'question bank',
  guide: 'guide',
  notes: 'notes',
  reference: 'reference',
};

/** Every library (synced index plus this device's link to its folder). */
export function listLibraries() {
  return call('libraries');
}

/** Import a folder as a library; `onProgress` gets one event per file. */
export function importLibrary(path, onProgress = () => {}, options = null) {
  return call('library_import', { path, options, onProgress: channel(onProgress) });
}

export function refreshLibrary(slug, onProgress = () => {}, maxFileMb = null) {
  return call('library_refresh', { slug, maxFileMb, onProgress: channel(onProgress) });
}

/** "Reading 12 of 340: notes/digest.md" while an import runs. */
export function progressText(p) {
  if (p.stage === 'scan') return 'Reading the folder…';
  if (p.stage === 'done') return 'Finishing…';
  const where = p.path ? `: ${p.path}` : '';
  return `Extracting ${p.done + 1} of ${p.total}${where}`;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

/** One line on what an import or refresh did. */
export function reportText(r) {
  const parts = [];
  if (r.added) parts.push(`${r.added} added`);
  if (r.changed) parts.push(`${r.changed} updated`);
  if (r.removed) parts.push(`${r.removed} removed`);
  if (!parts.length) parts.push('nothing changed');
  return `${r.library.name}: ${plural(r.library.files, 'file')} (${parts.join(', ')}).`;
}

/** "3 question banks · 12 guides · 40 notes …" */
export function kindsText(lib) {
  return Object.entries(lib.kinds ?? {})
    .map(([kind, n]) => `${n} ${KIND_LABELS[kind] ?? kind}`)
    .join(' · ');
}

function skippedText(lib) {
  const skipped = Object.entries(lib.skipped ?? {});
  if (!skipped.length) return '';
  const total = skipped.reduce((sum, [, n]) => sum + n, 0);
  const reasons = skipped.map(([reason, n]) => `${n} ${reason.replace(/-/g, ' ')}`).join(', ');
  return `Skipped at import: ${total} (${reasons}).`;
}

// ---------- Settings → Libraries ----------

let els = null;
let busy = false;

function setStatus(text, error = false) {
  if (!els?.status) return;
  els.status.textContent = text;
  els.status.classList.toggle('error', error);
}

function button(label, onClick, cls = 'btn text') {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = cls;
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

function field(id, label, value, multiline = false) {
  const wrap = document.createElement('div');
  wrap.className = 'field';
  const l = document.createElement('label');
  l.htmlFor = id;
  l.textContent = label;
  const input = document.createElement(multiline ? 'textarea' : 'input');
  input.className = 'input';
  input.id = id;
  input.value = value ?? '';
  if (multiline) input.rows = 2;
  wrap.append(l, input);
  return { wrap, input };
}

function libraryRow(lib) {
  const row = document.createElement('div');
  row.className = 'library';

  const head = document.createElement('div');
  head.className = 'plugin-head';
  const title = document.createElement('span');
  title.className = 'plugin-title';
  title.textContent = lib.name;
  const id = document.createElement('span');
  id.className = 'plugin-version';
  id.textContent = lib.slug;
  head.append(title, id);
  row.append(head);

  if (lib.topic || lib.description) {
    const d = document.createElement('p');
    d.className = 'plugin-desc';
    d.textContent = [lib.topic, lib.description].filter(Boolean).join(' — ');
    row.append(d);
  }

  const meta = document.createElement('p');
  meta.className = 'meta';
  const noText = lib.noText ? ` · ${lib.noText} without text (scans)` : '';
  meta.textContent = `${plural(lib.files, 'file')}: ${kindsText(lib) || 'none'}${noText}`;
  row.append(meta);

  const where = document.createElement('p');
  where.className = 'meta';
  where.textContent = lib.source
    ? `From ${lib.source} on this device.`
    : 'Imported on another device: its text is here; refresh it where it was imported.';
  row.append(where);

  const skipped = skippedText(lib);
  if (skipped) {
    const s = document.createElement('p');
    s.className = 'meta';
    s.textContent = skipped;
    row.append(s);
  }

  const actions = document.createElement('div');
  actions.className = 'actions';
  const edit = document.createElement('form');
  edit.className = 'library-edit';
  edit.hidden = true;
  const name = field(`library-name-${lib.slug}`, 'Name', lib.name);
  const topic = field(`library-topic-${lib.slug}`, 'Topic', lib.topic);
  const description = field(`library-description-${lib.slug}`, 'Description (agents read it)', lib.description, true);
  const save = document.createElement('div');
  save.className = 'actions';
  const saveBtn = document.createElement('button');
  saveBtn.type = 'submit';
  saveBtn.className = 'btn secondary';
  saveBtn.textContent = 'Save';
  save.append(saveBtn, button('Cancel', () => { edit.hidden = true; }));
  edit.append(name.wrap, topic.wrap, description.wrap, save);
  edit.addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      await call('library_update', {
        slug: lib.slug,
        name: name.input.value,
        topic: topic.input.value,
        description: description.input.value,
      });
      setStatus('Saved.');
      await renderLibrarySettings();
    } catch (err) {
      setStatus(message(err), true);
    }
  });

  actions.append(button('Edit', () => {
    edit.hidden = !edit.hidden;
    if (!edit.hidden) name.input.focus();
  }));
  if (lib.source) {
    actions.append(button('Refresh', async (e) => {
      if (busy) return;
      busy = true;
      const btn = e.currentTarget;
      btn.disabled = true;
      try {
        const report = await refreshLibrary(lib.slug, (p) => setStatus(progressText(p)));
        setStatus(reportText(report));
        await renderLibrarySettings();
      } catch (err) {
        setStatus(`Couldn’t refresh ${lib.name}: ${message(err)}`, true);
      } finally {
        busy = false;
        btn.disabled = false;
      }
    }));
  }
  actions.append(button('Remove', async () => {
    const ok = await confirmDialog({
      title: `Remove ${lib.name}?`,
      text: 'Its text leaves every device at the next sync, and this device’s copies of the originals are deleted. Your folder is not touched.',
      confirmLabel: 'Remove library',
      cancelLabel: 'Keep it',
    });
    if (!ok) return;
    try {
      await call('library_remove', { slug: lib.slug });
      setStatus(`Removed ${lib.name}.`);
      await renderLibrarySettings();
    } catch (err) {
      setStatus(message(err), true);
    }
  }));
  row.append(actions, edit);
  return row;
}

/** Re-reads the list into Settings → Libraries. */
export async function renderLibrarySettings() {
  if (!els?.list) return;
  let libs;
  try {
    libs = await listLibraries();
  } catch (e) {
    setStatus(`Couldn’t load libraries: ${message(e)}`, true);
    return;
  }
  els.list.textContent = '';
  if (!libs.length) {
    const p = document.createElement('p');
    p.className = 'help';
    p.textContent = 'No libraries yet. Add a folder of references with + on Home, or import one here.';
    els.list.append(p);
    return;
  }
  for (const lib of libs) els.list.append(libraryRow(lib));
}

async function importFromSettings() {
  if (busy) return;
  let path;
  try {
    path = await call('pick_folder');
  } catch (e) {
    if (!/no folder chosen/i.test(message(e))) setStatus(message(e), true);
    return;
  }
  const mb = Number.parseInt(els.maxMb?.value ?? '', 10);
  busy = true;
  els.importBtn.disabled = true;
  try {
    const report = await importLibrary(path, (p) => setStatus(progressText(p)), {
      maxFileMb: Number.isFinite(mb) && mb > 0 ? mb : null,
    });
    setStatus(reportText(report));
    await renderLibrarySettings();
  } catch (e) {
    setStatus(`Couldn’t import the folder: ${message(e)}`, true);
  } finally {
    busy = false;
    els.importBtn.disabled = false;
  }
}

export function initLibrarySettings() {
  els = {
    list: document.getElementById('library-list'),
    status: document.getElementById('library-status'),
    importBtn: document.getElementById('library-import'),
    maxMb: document.getElementById('library-max-mb'),
  };
  els.importBtn?.addEventListener('click', importFromSettings);
}
