// Applies the saved theme override (Settings → Appearance) before first
// paint. Classic script, loaded blocking from <head>; settings.js owns writes.
try {
  const theme = localStorage.getItem('lh.theme');
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
} catch {
  // Storage unavailable: follow the system setting.
}
