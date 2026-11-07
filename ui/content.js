// Post-render passes shared by finished and streaming prose: diagrams,
// artifact figures, and highlighting. Each pass is idempotent.

import { appendUnreferenced, artifactFigure, resolveArtifacts } from './artifacts.js';
import { highlightAll } from './highlight.js';
import { renderDiagrams } from './mermaid.js';

/** Passes that are safe on a block that is still being appended to. */
export function enhanceLive(root) {
  renderDiagrams(root);
  highlightAll(root);
}

/**
 * A finished message: resolve `artifact:` slots, then diagrams/highlighting,
 * then append artifacts the reply never referenced.
 */
export function enhanceMessage(entry, prose, artifacts, ctx = {}) {
  const referenced = resolveArtifacts(prose, ctx);
  enhanceLive(prose);
  appendUnreferenced(entry, artifacts, referenced, ctx);
  return referenced;
}

/** Figures created in a turn whose reply never carried a message block. */
export function artifactsEl(ops, ctx = {}) {
  const entry = document.createElement('div');
  entry.className = 'entry artifacts';
  for (const op of ops) entry.append(artifactFigure(op, op.data?.title || 'Figure', ctx));
  return entry;
}
