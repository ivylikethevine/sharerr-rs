// The light/dark toggle. Loaded in <head> without `defer`, on every page,
// so the remembered theme is on <html> before the first paint: the system
// preference is the default, and a pinned choice is `data-theme="light"` or
// `"dark"`, which style.css honours over `prefers-color-scheme`. Remembered
// per browser in localStorage, like Topology's own toggles; storage can be
// absent or throw, and the page must still work.
(function () {
  var KEY = "theme";
  var ORDER = ["system", "light", "dark"];
  var LABEL = { system: "System theme", light: "Light theme", dark: "Dark theme" };

  function recall() {
    try {
      var v = localStorage.getItem(KEY);
      return v === "light" || v === "dark" ? v : "system";
    } catch (e) { return "system"; }
  }
  function remember(theme) {
    try {
      if (theme === "system") localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, theme);
    } catch (e) {}
  }
  function apply(theme) {
    var root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    var button = document.getElementById("theme-toggle");
    if (!button) return;
    button.setAttribute("data-theme-choice", theme);
    button.setAttribute("aria-label", LABEL[theme] + " — click to change");
    button.setAttribute("title", LABEL[theme]);
  }

  apply(recall());

  document.addEventListener("DOMContentLoaded", function () {
    apply(recall());
    var button = document.getElementById("theme-toggle");
    if (!button) return;
    button.addEventListener("click", function () {
      var next = ORDER[(ORDER.indexOf(recall()) + 1) % ORDER.length];
      remember(next);
      apply(next);
    });
  });
})();
