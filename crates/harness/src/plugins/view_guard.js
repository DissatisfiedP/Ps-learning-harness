// The core puts this first in every plugin view document (`with_view_guard`).
// A view that throws while it loads or handles `init` has drawn nothing the
// learner can use: the host is told, and shows the fallback figure instead.
(() => {
  let starting = true;
  addEventListener('message', (event) => {
    const data = event.data;
    if (event.source !== parent || !data || data.protocol !== 1 || data.type !== 'init') return;
    // The view's own `init` listeners run after this one, before the timer.
    setTimeout(() => { starting = false; }, 0);
  });
  addEventListener('error', (event) => {
    if (!starting) return;
    starting = false;
    parent.postMessage({ protocol: 1, type: 'error', message: String(event.message || 'error').slice(0, 300) }, '*');
  });
})();
