// The app is not a web page to zoom: a pinch, ctrl/⌘+wheel or Safari gesture
// never magnifies the whole window (the Tauri zoom hotkeys are off for the
// same reason). Surfaces that zoom, the Map, the plan diagram and the scratch
// pad, handle the gesture themselves before it gets here; the rest of the
// page simply ignores it. Pair with `touch-action: pan-x pan-y` on the page
// (styles.css), which stops a touch-screen pinch where the engine honours it.

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

/**
 * Takes the fingers on `el` away from the engine, for a surface that follows
 * them itself with pointer events. `touch-action: none` is not enough on
 * WebKitGTK: it does not read it, and turns every touch the page has not
 * cancelled into its own gestures as well, which the page then also hears. A
 * one-finger drag becomes `wheel` events, lifting the finger one more whose
 * delta is the fling's speed in px/s, a tap a mouse click, and two fingers
 * magnify the window. Cancelling a touch event is the one thing that stops
 * them, for every finger until all have lifted. Pointer events still arrive;
 * a cancelled `touchstart` leaves no `click` behind, so `wants(e)` says which
 * events to take when `el` holds things to tap.
 */
export function ownTouches(el, wants = () => true) {
  const take = (e) => {
    if (e.cancelable && wants(e)) e.preventDefault();
  };
  el.addEventListener('touchstart', take, { passive: false });
  el.addEventListener('touchmove', take, { passive: false });
}
