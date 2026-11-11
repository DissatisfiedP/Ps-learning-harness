// The shared composer (build prompt W3, D11): one component on Home and in
// sessions. Attachments are staged as soon as they arrive (images in a
// device-local staging folder, files and folders referenced by path) but are
// only sent with the next learner message; Send moves staged images into
// the data repo, and removing a chip deletes its staged copy. Practice has
// no file tools, so it takes images only (no '+', no '@'). The `@` list is
// an ARIA combobox over the backend's mention search. On Home (a new Learn
// session) '+' opens a menu: files, or a folder imported as a reference
// library (library.js), or a library imported before; a library becomes a
// `library` chip that links it to the new session. In a session '+' picks
// files as before. Libraries never go through the workspace folder.

import { call, message } from './api.js';
import { attachmentChip } from './attachment.js';
import { confirmDialog } from './dialog.js';
import { importLibrary, listLibraries, progressText, reportText } from './library.js';

const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

let els = null;
let sessionId = null;
let mode = 'learn';
let vision = { tutor: null, practice: null };
let hooks = {};
let items = []; // { key, attachment, origin, thumb }
let seq = 0;
let mention = { open: false, tokenStart: 0, query: '', items: [], active: -1 };
let mentionTimer = 0;
let mentionSeq = 0;
let nativeDrop = false;

const key = (a) => `${a.kind}\u0000${a.path}`;

function visionForMode() {
  return mode === 'practice' ? vision.practice : vision.tutor;
}

function say(text, error = false) {
  if (!els) return;
  els.error.textContent = text;
  els.error.classList.toggle('error', Boolean(text) && error);
  // A note (import progress, a result) reads as status, not as an error.
  els.error.classList.toggle('note', Boolean(text) && !error);
  els.error.hidden = !text;
}

function setBusy(busy) {
  els.add.disabled = busy;
}

function renderChips() {
  els.chips.textContent = '';
  for (const it of items) {
    const li = attachmentChip(it.attachment, {
      thumb: it.thumb,
      onRemove: (a) => removeItem(it.key),
      onUseWorkspace:
        it.attachment.kind === 'folder' && it.origin === 'mention' && sessionId && hooks.onUseWorkspace
          ? (a) => hooks.onUseWorkspace({ path: a.path, name: a.name ?? a.path.split(/[\\/]/).pop() })
          : null,
    });
    if (it.attachment.kind === 'image' && visionForMode() === false) {
      const kind = li.querySelector('.chip-kind');
      if (kind) kind.textContent = 'This model can’t see images';
    }
    els.chips.append(li);
  }
  const any = items.length > 0;
  els.attach.hidden = !any;
}

function addItem(attachment, { origin = 'pick', thumb = null } = {}) {
  if (!attachment?.path) return;
  if (items.some((it) => key(it.attachment) === key(attachment))) return;
  items.push({ key: `${++seq}`, attachment, origin, thumb });
  renderChips();
}

function removeItem(id) {
  const hadFocus = els.chips.contains(document.activeElement);
  const gone = items.filter((it) => it.key === id);
  items = items.filter((it) => it.key !== id);
  unstage(gone);
  renderChips();
  if (hadFocus) els.prompt.focus({ preventScroll: true });
}

/** Deletes the staged copies of image chips that will not be sent. */
function unstage(list) {
  for (const it of list) {
    if (it.attachment.kind !== 'image') continue;
    call('unstage_image', { path: it.attachment.path }).catch(() => {
      // Best effort: staging is cleared at the next start anyway.
    });
  }
}

function fileDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error('could not read that image'));
    reader.readAsDataURL(file);
  });
}

async function addImageFile(file) {
  if (file.size > MAX_IMAGE_BYTES) {
    say('Image over 5 MB. Not added.', true);
    return;
  }
  try {
    const data = await fileDataUrl(file);
    const attachment = await call('stage_image', {
      data,
      name: file.name || 'Pasted image',
    });
    addItem(attachment, { origin: 'paste', thumb: data });
    say('');
  } catch (e) {
    say(`Couldn’t add the image: ${message(e)}`, true);
  }
}

/**
 * A picked, dropped or mentioned path. The backend stages an image file like
 * a pasted image; anything else becomes a file or folder reference.
 */
async function addPath(path, { origin = 'pick', name = null } = {}) {
  if (!path) return;
  try {
    const attachment = await call('stage_file', { sessionId, path });
    if (mode === 'practice' && attachment.kind !== 'image') {
      say('Practice can take images only.', true);
      return;
    }
    if (name && !attachment.name) attachment.name = name;
    addItem(attachment, { origin });
    say('');
  } catch (e) {
    say(`Couldn’t add ${name || path}: ${message(e)}`, true);
  }
}

async function pickFiles() {
  setBusy(true);
  say('');
  try {
    const paths = await call('pick_files');
    for (const path of paths ?? []) await addPath(path);
  } catch (e) {
    // "no files chosen" is a normal cancel.
    if (!/no files chosen/i.test(message(e))) say(`Couldn’t add files: ${message(e)}`, true);
  } finally {
    setBusy(false);
  }
}

// ---------- Home '+': files or a reference library ----------

/** Home, Learn, no session yet: the only place a library is linked. */
const onHome = () => !sessionId && mode === 'learn';

function updateAddButton() {
  if (!els) return;
  const home = onHome();
  els.add.setAttribute('aria-label', home ? 'Add files or a reference library' : 'Add files');
  if (home) {
    els.add.setAttribute('aria-haspopup', 'menu');
    els.add.setAttribute('aria-expanded', String(!els.addMenu.hidden));
  } else {
    closeAddMenu();
    els.add.removeAttribute('aria-haspopup');
    els.add.removeAttribute('aria-expanded');
  }
}

function closeAddMenu() {
  if (!els?.addMenu || els.addMenu.hidden) return;
  els.addMenu.hidden = true;
  els.add.setAttribute('aria-expanded', 'false');
}

function menuItem(label, note, onClick) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'menu-item';
  b.setAttribute('role', 'menuitem');
  b.textContent = label;
  if (note) {
    const n = document.createElement('span');
    n.className = 'menu-note';
    n.textContent = note;
    b.append(n);
  }
  b.addEventListener('click', () => {
    closeAddMenu();
    els.add.focus({ preventScroll: true });
    onClick();
  });
  return b;
}

async function openAddMenu() {
  const menu = els.addMenu;
  menu.textContent = '';
  menu.append(
    menuItem('Files…', null, pickFiles),
    menuItem('Folder as a reference library…', 'Copied in; only its text syncs', addLibraryFolder),
  );
  menu.hidden = false;
  els.add.setAttribute('aria-expanded', 'true');
  menu.querySelector('.menu-item')?.focus();
  let libs = [];
  try {
    libs = await listLibraries();
  } catch {
    // The two actions still work without the list.
  }
  if (menu.hidden || !libs.length) return;
  const heading = document.createElement('div');
  heading.className = 'menu-heading';
  heading.textContent = 'Link a library';
  menu.append(heading);
  for (const lib of libs) {
    const linked = items.some((it) => it.attachment.kind === 'library' && it.attachment.path === lib.slug);
    const item = menuItem(lib.name, `${lib.files} files${linked ? ' · linked' : ''}`, () => linkLibrary(lib));
    item.disabled = linked;
    menu.append(item);
  }
}

function onAdd() {
  if (!onHome()) {
    pickFiles();
    return;
  }
  if (els.addMenu.hidden) openAddMenu();
  else closeAddMenu();
}

function onAddMenuKey(e) {
  const list = [...els.addMenu.querySelectorAll('.menu-item:not(:disabled)')];
  const at = list.indexOf(document.activeElement);
  if (e.key === 'Escape') {
    e.preventDefault();
    closeAddMenu();
    els.add.focus({ preventScroll: true });
  } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const step = e.key === 'ArrowDown' ? 1 : -1;
    list[(at + step + list.length) % list.length]?.focus();
  }
}

/** A library chip: sent with the first message, it links the library. */
function linkLibrary(lib) {
  addItem({ kind: 'library', path: lib.slug, name: lib.name }, { origin: 'library' });
}

/** Pick a folder, import it (or refresh it if imported before) and link it. */
async function addLibraryFolder() {
  let path;
  try {
    path = await call('pick_folder');
  } catch (e) {
    if (!/no folder chosen/i.test(message(e))) say(message(e), true);
    return;
  }
  setBusy(true);
  say('Reading the folder…');
  try {
    const report = await importLibrary(path, (p) => say(progressText(p)));
    linkLibrary(report.library);
    say(reportText(report));
  } catch (e) {
    say(`Couldn’t import the folder: ${message(e)}`, true);
  } finally {
    setBusy(false);
  }
}

// ---------- @ mentions ----------

function closeMention() {
  if (!mention.open) return;
  mention = { open: false, tokenStart: 0, query: '', items: [], active: -1 };
  els.prompt.setAttribute('aria-expanded', 'false');
  els.prompt.removeAttribute('aria-activedescendant');
  els.pop.hidden = true;
  els.list.textContent = '';
}

function setActive(index) {
  if (!mention.open) return;
  if (index < 0) index = mention.items.length - 1;
  if (index >= mention.items.length) index = 0;
  mention.active = index;
  const options = [...els.list.querySelectorAll('[role="option"]')];
  options.forEach((opt, i) => opt.setAttribute('aria-selected', String(i === index)));
  const active = options[index];
  if (active?.id) {
    els.prompt.setAttribute('aria-activedescendant', active.id);
    active.scrollIntoView({ block: 'nearest', behavior: 'instant' });
  }
}

function renderMentionList(results, error = '') {
  els.list.textContent = '';
  mention.items = results;
  mention.active = results.length ? 0 : -1;
  for (const [i, item] of results.entries()) {
    const opt = document.createElement('li');
    opt.id = `mention-option-${i}`;
    opt.setAttribute('role', 'option');
    opt.setAttribute('aria-selected', String(i === 0));
    opt.className = 'mention-option';
    const name = document.createElement('span');
    name.className = 'mention-name';
    name.textContent = item.name;
    const kind = document.createElement('span');
    kind.className = 'mention-kind';
    kind.textContent = item.kind === 'folder' ? 'Folder' : 'File';
    const path = document.createElement('span');
    path.className = 'mention-path';
    path.textContent = item.path;
    opt.append(name, kind, path);
    // mousedown keeps focus in the textarea; click picks.
    opt.addEventListener('mousedown', (e) => e.preventDefault());
    opt.addEventListener('click', () => acceptMention(i));
    els.list.append(opt);
  }
  if (!results.length) {
    const empty = document.createElement('li');
    empty.className = 'mention-empty';
    empty.setAttribute('role', 'option');
    empty.setAttribute('aria-disabled', 'true');
    empty.textContent = error || 'No files match.';
    els.list.append(empty);
  }
  els.pop.hidden = false;
  els.prompt.setAttribute('aria-expanded', 'true');
  if (mention.active >= 0) {
    els.prompt.setAttribute('aria-activedescendant', `mention-option-${mention.active}`);
  } else {
    els.prompt.removeAttribute('aria-activedescendant');
  }
}

function queryMentions(query) {
  const seqNo = ++mentionSeq;
  clearTimeout(mentionTimer);
  mentionTimer = setTimeout(async () => {
    let results = [];
    let error = '';
    try {
      results = (await call('mentions', { sessionId, query })) ?? [];
    } catch (e) {
      error = `Couldn’t search files: ${message(e)}`;
    }
    if (seqNo !== mentionSeq || !mention.open) return;
    renderMentionList(Array.isArray(results) ? results : [], error);
  }, 120);
}

function updateMention() {
  if (!els) return;
  if (mode === 'practice') return closeMention();
  const value = els.prompt.value;
  const pos = els.prompt.selectionStart;
  if (pos !== els.prompt.selectionEnd) return closeMention();
  const before = value.slice(0, pos);
  const m = /(^|[\s])@([^\s@]*)$/.exec(before);
  if (!m) return closeMention();
  const tokenStart = m.index + m[1].length;
  const query = m[2];
  if (mention.open && mention.tokenStart === tokenStart && mention.query === query) return;
  mention = { open: true, tokenStart, query, items: mention.items, active: -1 };
  els.prompt.setAttribute('aria-expanded', 'true');
  queryMentions(query);
}

function acceptMention(index) {
  const item = mention.items[index];
  if (!item) return;
  const value = els.prompt.value;
  const caret = els.prompt.selectionStart;
  const before = value.slice(0, mention.tokenStart);
  const after = value.slice(caret);
  const insert = `@${item.path} `;
  els.prompt.value = before + insert + after;
  const pos = (before + insert).length;
  els.prompt.setSelectionRange(pos, pos);
  closeMention();
  els.prompt.dispatchEvent(new Event('input', { bubbles: true }));
  addPath(item.path, { origin: 'mention', name: item.name });
}

function onMentionKey(e) {
  // An IME composing text owns Enter and the arrows.
  if (!mention.open || e.isComposing || e.keyCode === 229) return;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    e.stopPropagation();
    if (mention.items.length) setActive(mention.active + (e.key === 'ArrowDown' ? 1 : -1));
  } else if (e.key === 'Enter' || e.key === 'Tab') {
    if (mention.items.length) {
      e.preventDefault();
      e.stopPropagation();
      acceptMention(mention.active >= 0 ? mention.active : 0);
    } else {
      closeMention();
    }
  } else if (e.key === 'Escape') {
    e.preventDefault();
    e.stopPropagation();
    closeMention();
  }
}

// ---------- drag and drop ----------

/** Whether a native drag position (physical pixels) is over the composer. */
function overComposer(pos) {
  if (!pos) return true;
  const rect = els.composer.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const x = pos.x / dpr;
  const y = pos.y / dpr;
  return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
}

function wireNativeDrop() {
  const tauri = window.__TAURI__;
  const webview = tauri?.webviewWindow?.getCurrentWebview?.() ?? tauri?.webview?.getCurrentWebview?.();
  if (!webview?.onDragDropEvent) return;
  nativeDrop = true;
  try {
    // With the native handler on, HTML5 file drags never reach the page, so
    // the drop outline follows these events too.
    webview.onDragDropEvent((event) => {
      const p = event?.payload ?? event;
      const over = p?.type !== 'leave' && overComposer(p?.position);
      els.composer.classList.toggle('dropping', over && (p?.type === 'enter' || p?.type === 'over'));
      if (p?.type !== 'drop' || !over) return;
      for (const path of p.paths ?? []) addPath(path, { origin: 'drop' });
    });
  } catch {
    nativeDrop = false;
  }
}

// ---------- public ----------

export function initComposer(h) {
  hooks = h ?? {};
  els = {
    composer: document.getElementById('composer'),
    prompt: document.getElementById('prompt'),
    add: document.getElementById('add-files'),
    attach: document.getElementById('composer-attach'),
    chips: document.getElementById('attach-chips'),
    error: document.getElementById('attach-error'),
    pop: document.getElementById('mention-pop'),
    list: document.getElementById('mention-list'),
    addMenu: document.getElementById('add-menu'),
  };
  if (!els.composer) return null;

  els.prompt.setAttribute('aria-autocomplete', 'list');
  els.prompt.setAttribute('aria-controls', 'mention-list');
  els.prompt.setAttribute('aria-expanded', 'false');

  els.add.addEventListener('click', onAdd);
  els.addMenu?.addEventListener('keydown', onAddMenuKey);
  document.addEventListener('pointerdown', (e) => {
    const menu = els.addMenu;
    if (menu && !menu.hidden && !menu.contains(e.target) && !els.add.contains(e.target)) closeAddMenu();
  });
  els.prompt.addEventListener('input', updateMention);
  els.prompt.addEventListener('click', updateMention);
  els.prompt.addEventListener('keyup', (e) => {
    if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) updateMention();
  });
  els.prompt.addEventListener('keydown', onMentionKey, true);
  els.prompt.addEventListener('blur', () => setTimeout(closeMention, 100));

  els.composer.addEventListener('paste', (e) => {
    // Text paste is never intercepted: a clipboard with text (a copy from a
    // browser or office app often carries an image rendering too) pastes
    // the text as usual.
    const types = [...(e.clipboardData?.types ?? [])];
    if (types.includes('text/plain')) return;
    const files = [...(e.clipboardData?.items ?? [])]
      .filter((item) => item.type.startsWith('image/'))
      .map((item) => item.getAsFile())
      .filter(Boolean);
    if (!files.length) return;
    e.preventDefault();
    for (const file of files) addImageFile(file);
  });

  els.composer.addEventListener('dragover', (e) => {
    if (![...(e.dataTransfer?.types ?? [])].includes('Files')) return;
    e.preventDefault();
    els.composer.classList.add('dropping');
  });
  els.composer.addEventListener('dragleave', (e) => {
    if (e.target === els.composer) els.composer.classList.remove('dropping');
  });
  els.composer.addEventListener('drop', (e) => {
    els.composer.classList.remove('dropping');
    if (![...(e.dataTransfer?.types ?? [])].includes('Files')) return;
    e.preventDefault();
    if (nativeDrop) return; // the native handler has real paths
    const files = [...(e.dataTransfer?.files ?? [])];
    for (const file of files) {
      if (file.path) addPath(file.path, { origin: 'drop' });
      else if (file.type.startsWith('image/')) addImageFile(file);
      else say(mode === 'practice' ? 'Practice can take images only.' : 'Can’t drop that file. Use “Add files”.', true);
    }
  });

  wireNativeDrop();
  renderChips();
  updateAddButton();
  return api;
}

const api = {
  setSession(id) {
    if (sessionId === id) return;
    sessionId = id ?? null;
    closeMention();
    renderChips();
    updateAddButton();
  },

  setMode(next) {
    mode = next === 'practice' ? 'practice' : 'learn';
    // Practice has no file tools: images only, no '+' and no '@'.
    els.add.hidden = mode === 'practice';
    if (mode === 'practice') {
      closeMention();
      if (items.some((it) => it.attachment.kind !== 'image')) {
        items = items.filter((it) => it.attachment.kind === 'image');
        say('Practice can take images only. File and folder chips were removed.');
      }
    }
    renderChips();
    updateAddButton();
  },

  setVision(next) {
    vision = {
      tutor: next?.tutor ?? null,
      practice: next?.practice ?? null,
    };
    renderChips();
  },

  /**
   * The attachments for one send, or null if the learner kept editing. With
   * a model that can't see images, asks once whether to send without them.
   */
  async takeForSend() {
    let list = [...items];
    if (visionForMode() === false && list.some((it) => it.attachment.kind === 'image')) {
      const ok = await confirmDialog({
        title: 'This model can’t see images.',
        text: 'Send without them?',
        confirmLabel: 'Send without images',
        cancelLabel: 'Keep editing',
      });
      if (!ok) return null;
      unstage(list.filter((it) => it.attachment.kind === 'image'));
      list = list.filter((it) => it.attachment.kind !== 'image');
    }
    items = [];
    renderChips();
    return list.map((it) => it.attachment);
  },

  /** Puts attachments back after a rejected send. */
  restore(list) {
    for (const a of list ?? []) addItem(a);
  },
};
