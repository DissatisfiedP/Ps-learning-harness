// The app is not a web page to zoom: a pinch, ctrl/⌘+wheel or Safari gesture
// never magnifies the whole window (the Tauri zoom hotkeys are off for the
// same reason). Surfaces that zoom, the Map and the plan diagram, handle the
// gesture themselves before it gets here; the scratch pad and the rest of
// the page simply ignore it. Pair with `touch-action: pan-x pan-y` on the
// page (styles.css), which stops a touch-screen pinch where the engine
// honours it.

export function blockPageZoom() {
  const stop = (e) => e.preventDefault();
  document.addEventListener(
    'wheel',
    (e) => {
      if (e.ctrlKey || e.metaKey) e.preventDefault();
    },
    { passive: false, capture: true },
  );
  for (const type of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(type, stop, { passive: false });
  }
  // Two or more fingers on the screen: only a pinch, never a scroll.
  document.addEventListener(
    'touchmove',
    (e) => {
      if (e.touches.length > 1) e.preventDefault();
    },
    { passive: false },
  );
}
