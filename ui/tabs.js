// Settings tabs (ui-design §7.5): a horizontal tablist above the settings
// panels, in the WAI-ARIA tabs pattern with automatic activation. Arrow
// keys, Home and End move between tabs; the selected tab is the one tab stop
// and is remembered per device (`localStorage["lh.settings.tab"]`).

const KEY = 'lh.settings.tab';

const tabs = () => [...document.querySelectorAll('#settings-tabs [role="tab"]')];

function stored() {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return null;
  }
}

function store(name) {
  try {
    localStorage.setItem(KEY, name);
  } catch {
    // Storage can be unavailable; the tab still switches for this visit.
  }
}

/**
 * Shows the panel of tab `name` (the first tab when unknown). `remember`
 * keeps the choice for the next visit; navigating into a section from
 * elsewhere (a sync prompt, "connect a model") passes false.
 */
export function selectTab(name, { focus = false, remember = true } = {}) {
  const all = tabs();
  const target = all.find((t) => t.dataset.tab === name) ?? all[0];
  if (!target) return;
  for (const tab of all) {
    const on = tab === target;
    tab.setAttribute('aria-selected', String(on));
    tab.tabIndex = on ? 0 : -1;
    const panel = document.getElementById(tab.getAttribute('aria-controls'));
    if (panel) panel.hidden = !on;
  }
  if (remember) store(target.dataset.tab);
  target.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  if (focus) target.focus();
}

/** The tab this device last used (the first one by default). */
export function restoreTab() {
  selectTab(stored(), { remember: false });
}

function move(to) {
  const all = tabs();
  const next = all[(to + all.length) % all.length];
  selectTab(next.dataset.tab, { focus: true });
  document.getElementById('settings').scrollTop = 0;
}

export function initTabs() {
  const list = document.getElementById('settings-tabs');
  if (!list) return;
  list.addEventListener('click', (e) => {
    const tab = e.target.closest('[role="tab"]');
    if (!tab) return;
    selectTab(tab.dataset.tab);
    document.getElementById('settings').scrollTop = 0;
  });
  list.addEventListener('keydown', (e) => {
    const tab = e.target.closest('[role="tab"]');
    if (!tab || e.altKey || e.ctrlKey || e.metaKey) return;
    const at = tabs().indexOf(tab);
    const to = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs().length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    move(to);
  });
  restoreTab();
}
