// Copy-to-clipboard for any `[data-copy]` (copies the text of the element the
// selector names) or `[data-copy-text]` (copies the attribute itself).
// Delegated from the document so it covers rows rendered on any page without
// each template shipping its own handler.
(function () {
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-copy], [data-copy-text]");
    if (!button) return;

    var text = button.hasAttribute("data-copy-text")
      ? button.getAttribute("data-copy-text")
      : (document.querySelector(button.getAttribute("data-copy")) || {}).textContent;
    if (text == null) return;

    var note = button.parentNode.querySelector("[data-copy-note]");
    var say = function (message) {
      if (!note) return;
      note.textContent = message;
      // Cleared again so a stale "Copied." does not sit next to a value the
      // operator has since changed their mind about.
      window.setTimeout(function () { note.textContent = ""; }, 2000);
    };

    // `navigator.clipboard` needs a secure context, which a plain-HTTP LAN
    // instance is not — so fall back to selecting the value for a manual copy
    // rather than silently doing nothing.
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(function () { say(" Copied."); });
      return;
    }

    var source = button.hasAttribute("data-copy")
      ? document.querySelector(button.getAttribute("data-copy"))
      : button.previousElementSibling;
    if (!source) { say(" Copy manually: " + text); return; }
    var range = document.createRange();
    range.selectNodeContents(source);
    var sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    say(" Selected — press Ctrl+C.");
  });
})();

(function () {
  var path = location.pathname;
  var links = document.querySelectorAll(".bar nav a[href]");
  for (var i = 0; i < links.length; i++) {
    var href = links[i].getAttribute("href");
    var here = href === "/" ? path === "/" : path === href || path.indexOf(href + "/") === 0;
    if (here) links[i].setAttribute("aria-current", "page");
  }
})();

// Fades whichever edge of a `.table-scroll` still has content past it, so a
// wide table reads as scrollable before the operator has touched it — the
// scrollbar `overflow-x: auto` relies on otherwise never appears on a phone.
(function () {
  var EPS = 2;
  function paint(el) {
    var canLeft = el.scrollLeft > EPS;
    var canRight = el.scrollLeft < el.scrollWidth - el.clientWidth - EPS;
    if (canLeft && canRight) el.setAttribute("data-edge", "both");
    else if (canRight) el.setAttribute("data-edge", "right");
    else if (canLeft) el.setAttribute("data-edge", "left");
    else el.removeAttribute("data-edge");
  }
  function paintAll() {
    var els = document.querySelectorAll(".table-scroll");
    for (var i = 0; i < els.length; i++) paint(els[i]);
  }
  // `scroll` does not bubble, so the listener has to be capturing to catch it
  // via delegation instead of one handler per table.
  document.addEventListener("scroll", function (event) {
    if (event.target.classList && event.target.classList.contains("table-scroll")) paint(event.target);
  }, true);
  window.addEventListener("resize", paintAll);
  paintAll();
})();

// Submits the form of any `[data-autosubmit]` control as soon as it changes,
// so a lone select needs no button; a `<noscript>` button covers the rest.
document.addEventListener("change", function (event) {
  var control = event.target.closest && event.target.closest("[data-autosubmit]");
  if (control && control.form) control.form.submit();
});

// A `[data-select-all="name"]` checkbox ticks or clears every checkbox named
// `name` in its form, and follows them: ticked when all are, mixed when some.
// `form.elements` includes inputs joined to the form by a `form` attribute.
document.addEventListener("change", function (event) {
  var box = event.target;
  if (box.type !== "checkbox" || !box.form) return;
  var name = box.getAttribute("data-select-all") || box.name;
  var master = null, boxes = [];
  var els = box.form.elements;
  for (var i = 0; i < els.length; i++) {
    if (els[i].type !== "checkbox") continue;
    if (els[i].getAttribute("data-select-all") === name) master = els[i];
    else if (els[i].name === name) boxes.push(els[i]);
  }
  if (!master) return;
  if (box === master) {
    for (var j = 0; j < boxes.length; j++) boxes[j].checked = master.checked;
    return;
  }
  var ticked = boxes.filter(function (b) { return b.checked; }).length;
  master.checked = ticked === boxes.length;
  master.indeterminate = ticked > 0 && ticked < boxes.length;
});

// A button with `data-confirm` asks before its form submits.
document.addEventListener("click", function (event) {
  var button = event.target.closest && event.target.closest("[data-confirm]");
  if (button && !window.confirm(button.getAttribute("data-confirm"))) event.preventDefault();
});

// A `[data-print]` button opens the print dialog for the page it is on.
document.addEventListener("click", function (event) {
  var button = event.target.closest && event.target.closest("[data-print]");
  if (button) window.print();
});
