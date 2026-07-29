// Applies persisted appearance preferences before first paint to avoid a
// flash of the wrong theme. Runs as a classic blocking script before the
// module graph loads; failures fall back to CSS defaults (auto theme,
// serif reading font).
(function () {
  'use strict';
  try {
    var theme = localStorage.getItem('papered.theme');
    if (theme === 'light' || theme === 'dark') {
      document.documentElement.setAttribute('data-theme', theme);
    }
    if (localStorage.getItem('papered.reading-font') === 'sans') {
      document.documentElement.setAttribute('data-font', 'sans');
    }
  } catch (e) { /* localStorage unavailable — defaults apply */ }
})();
