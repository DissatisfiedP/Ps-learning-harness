// Artifact figures (build prompt W6, D7/D8).
//
// Markdown keeps `![alt](artifact:<id>)` out of DOMPurify's URI filter: the
// renderer turns it into a `<span data-artifact="<id>" data-alt="…">` slot.
// Here a slot becomes a `<figure>`: the interactive view when its plugin is
// installed, otherwise the stored fallback SVG (sanitized with DOMPurify's
// SVG profile). An id with no artifact op renders as bracketed alt text.

import DOMPurify from './vendor/purify.es.mjs';
import { getPlugin } from './pluginstore.js';
import { createViewHost } from './viewhost.js';

const SVG_PROFILE = {
  USE_PROFILES: { svg: true, svgFilters: true },
  FORBID_TAGS: ['script', 'style', 'foreignObject', 'iframe', 'object', 'embed', 'audio', 'video',
    'animate', 'animateTransform', 'animateMotion', 'set', 'discard'],
  FORBID_ATTR: ['style'],
};

// Same rules as the core's `sanitize_svg`: links stay in-document or embed a
// raster image; CSS may only reference `url(#id)`; no fetching CSS function or
// backslash (CSS escape) anywhere in a value.
const SAFE_HREF = /^(#|data:image\/(png|jpeg|gif|webp);base64,)/i;
const FETCHING = /\\|image-set\(|image\(|src\(|cross-fade\(|element\(|javascript:/i;
const EXTERNAL_URL = /url\(\s*[^#\s]/i;

/**
 * DOMPurify's SVG profile, then the core's reference rules again, so a stored
 * figure can't fetch anything.
 */
export function cleanSvg(svg) {
  if (!svg) return '';
  const root = DOMPurify.sanitize(String(svg), { ...SVG_PROFILE, RETURN_DOM_FRAGMENT: true });
  for (const el of root.querySelectorAll('*')) {
    for (const { name, value } of [...el.attributes]) {
      const lower = name.toLowerCase();
      const link = lower === 'href' || lower === 'xlink:href';
      if (lower.startsWith('on') || lower === 'style' || (link && !SAFE_HREF.test(value))
        || FETCHING.test(value) || EXTERNAL_URL.test(value)) el.removeAttribute(name);
    }
  }
  const box = document.createElement('div');
  box.append(root);
  return box.innerHTML;
}

function textFigure(alt) {
  const figure = document.createElement('figure');
  figure.className = 'artifact-figure missing';
  const text = alt ? `[${alt}]` : '[Figure]';
  figure.textContent = text;
  return figure;
}

/**
 * Builds the figure for one artifact op and, when possible, upgrades it to
 * the plugin's interactive view. The fallback is visible immediately.
 */
export function artifactFigure(op, alt, ctx = {}) {
  if (!op) return textFigure(alt);
  const label = alt || op.data?.title || 'Figure';
  const figure = document.createElement('figure');
  figure.className = 'artifact-figure';
  figure.dataset.artifactId = op.artifactId;

  // The alt text names the image (fallback) or the frame (view, its title).
  const clean = cleanSvg(op.fallbackSvg);
  if (clean && /<svg[\s>]/i.test(clean)) {
    figure.innerHTML = clean;
    const svg = figure.querySelector('svg');
    if (svg) {
      svg.setAttribute('role', 'img');
      svg.setAttribute('aria-label', label);
      svg.removeAttribute('width');
      svg.removeAttribute('height');
      svg.style.maxWidth = '100%';
      svg.style.height = 'auto';
    }
  } else {
    figure.textContent = alt ? `[${alt}]` : '[Figure]';
  }

  const plugin = getPlugin(op.pluginId);
  // A plugin that doesn't list this device's platform (a desktop-only one on
  // a phone) keeps the fallback, so its answer views stay unanswerable here.
  const canView = plugin && plugin.enabled !== false && plugin.hasView && plugin.onPlatform !== false;
  if (!canView) return figure;

  createViewHost({
    pluginId: op.pluginId,
    artifact: op,
    alt: label,
    answer: Boolean(ctx.answer),
    sessionId: ctx.sessionId ?? null,
    onSubmit: ctx.onSubmit ?? null,
  })
    .then((host) => {
      if (!figure.isConnected) {
        host.destroy();
        return;
      }
      figure.classList.add('interactive');
      figure.replaceChildren(host.el);
    })
    .catch(() => {
      // Missing plugin, older package or a platform without iframes: the
      // fallback already shown is the answer.
    });
  return figure;
}

function slotFigure(slot, ctx) {
  const id = slot.dataset.artifact;
  const alt = slot.dataset.alt ?? '';
  const op = ctx.artifacts?.get(id);
  return { id, figure: artifactFigure(op, alt, ctx) };
}

/**
 * Replaces every artifact slot under `root`; returns the ids that were
 * referenced (so the caller can append the rest after the message).
 */
export function resolveArtifacts(root, ctx = {}) {
  const referenced = new Set();
  if (!root?.querySelectorAll) return referenced;
  for (const slot of [...root.querySelectorAll('[data-artifact]')]) {
    const { id, figure } = slotFigure(slot, ctx);
    referenced.add(id);
    slot.replaceWith(figure);
  }
  return referenced;
}

/**
 * Appends artifacts created in a turn that the reply never referenced, in
 * creation order, after the message.
 */
export function appendUnreferenced(entry, ops, referenced, ctx = {}) {
  if (!Array.isArray(ops) || !ops.length) return;
  for (const op of ops) {
    if (!op || referenced.has(op.artifactId)) continue;
    entry.append(artifactFigure(op, op.data?.title || 'Figure', ctx));
  }
}
