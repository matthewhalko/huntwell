// Apply the theme before first paint so a dark-mode visitor never sees a white
// flash. Mirrors ThemeProvider (src/theme.tsx). Loaded from <head> as a file:
// the CSP (script-src 'self') blocks inline scripts.
(function () {
  try {
    var t = localStorage.getItem('huntwell.theme') || 'system';
    var dark = t === 'dark' || (t === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
    document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  } catch (e) {}
})();
