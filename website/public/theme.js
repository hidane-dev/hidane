// Applies the stored theme before first paint, then wires the toggle button.
(function () {
  var KEY = "hd-theme";
  var root = document.documentElement;
  try {
    var stored = localStorage.getItem(KEY);
    if (stored === "dark" || stored === "light") root.setAttribute("data-theme", stored);
  } catch (e) {}
  function current() {
    var attr = root.getAttribute("data-theme");
    if (attr === "dark" || attr === "light") return attr;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  document.addEventListener("DOMContentLoaded", function () {
    var buttons = document.querySelectorAll("[data-theme-toggle]");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].addEventListener("click", function () {
        var next = current() === "dark" ? "light" : "dark";
        root.setAttribute("data-theme", next);
        try { localStorage.setItem(KEY, next); } catch (e) {}
      });
    }
  });
})();
