// Installed plugins, as the rest of the UI needs them (artifact figures,
// plan cards, Settings). One cache; every mutation refreshes it.

import { call } from './api.js';

let list = [];
let loaded = false;
let pending = null;
const listeners = new Set();

function notify() {
  for (const fn of listeners) {
    try {
      fn(list);
    } catch {
      // A listener must not break the cache.
    }
  }
}

export function onPluginsChanged(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Loads `plugins()` once; `force` re-reads after an install or removal. */
export function loadPlugins(force = false) {
  if (loaded && !force) return Promise.resolve(list);
  pending ??= call('plugins')
    .then((next) => {
      list = Array.isArray(next) ? next : [];
      loaded = true;
      notify();
      return list;
    })
    .catch(() => {
      // No backend (or a failed read): the UI still renders fallbacks.
      return list;
    })
    .finally(() => {
      pending = null;
    });
  return pending;
}

export function plugins() {
  return list;
}

export function pluginIds() {
  return new Set(list.map((p) => p.id));
}

export function getPlugin(id) {
  return list.find((p) => p.id === id) ?? null;
}
