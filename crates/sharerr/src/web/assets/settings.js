// The section strip sticks just under the top bar, whose height depends on
// whether its nav wrapped — measured rather than guessed, and re-measured
// on resize. The strip's own height feeds the anchor offset the same way,
// since it wraps too. Both have static fallbacks in the stylesheet for the
// moment before this runs.
//
// Above the rail breakpoint the strip is a column beside the content rather
// than a bar above it, so it no longer covers an anchor and must contribute
// zero to the offset — or every section lands that much too low. The
// breakpoint is the one in the stylesheet; the two have to agree.
var RAIL = "(min-width: 64rem)";

(function () {
  var bar = document.querySelector(".bar");
  var strip = document.querySelector(".quicknav--sticky");
  if (!bar || !strip) return;
  var set = function () {
    var root = document.documentElement.style;
    root.setProperty("--bar-h", bar.offsetHeight + "px");
    root.setProperty(
      "--quicknav-h",
      window.matchMedia(RAIL).matches ? "0px" : strip.offsetHeight + "px"
    );
  };
  set();
  window.addEventListener("resize", set);
})();

// Marks the rail link for the section being read. Only useful once the strip
// is a rail — as a wrapped bar of pills there is no room for the mark to say
// anything — so it stays idle below the breakpoint.
//
// Position-based rather than IntersectionObserver: a section taller than the
// viewport has no heading on screen at all, and an observer then reports
// nothing intersecting and unmarks every pill. Several sections here are that
// tall, so the rail would blank out in the middle of the page it is meant to
// orient you in. "The last heading scrolled past" has an answer everywhere.
(function () {
  var strip = document.querySelector(".quicknav--sticky");
  var bar = document.querySelector(".bar");
  if (!strip) return;

  var links = [];
  var anchors = strip.querySelectorAll("a[href^='#']");
  for (var i = 0; i < anchors.length; i++) {
    var section = document.getElementById(anchors[i].getAttribute("href").slice(1));
    if (section) links.push({ link: anchors[i], section: section });
  }
  if (!links.length) return;

  var mark = function () {
    if (!window.matchMedia(RAIL).matches) return;
    // A heading tucked under the sticky bar has been passed, not reached.
    var line = (bar ? bar.offsetHeight : 0) + 1;
    var active = 0;
    for (var j = 0; j < links.length; j++) {
      if (links[j].section.getBoundingClientRect().top <= line) active = j;
    }
    for (var k = 0; k < links.length; k++) {
      if (k === active) links[k].link.setAttribute("aria-current", "true");
      else links[k].link.removeAttribute("aria-current");
    }
  };

  // Coalesced onto a frame: scroll fires far more often than the mark can
  // possibly change, and this runs a rect read per section each time.
  var queued = false;
  var onScroll = function () {
    if (queued) return;
    queued = true;
    window.requestAnimationFrame(function () {
      queued = false;
      mark();
    });
  };

  mark();
  window.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("resize", onScroll);
  // A fold opening or closing moves every section below it.
  document.addEventListener("toggle", onScroll, true);
})();

// A folded section that is the anchor target opens itself, so a quick-nav
// link to it lands on the form rather than on a closed summary.
(function () {
  var reveal = function () {
    if (!location.hash) return;
    var target = document.getElementById(location.hash.slice(1));
    if (!target) return;
    var fold = target.closest("details");
    if (fold && !fold.open) {
      fold.open = true;
      target.scrollIntoView();
    }
  };
  reveal();
  window.addEventListener("hashchange", reveal);
})();

// Warns before leaving with typed changes no save has taken. Each section is
// its own form, so saving one reloads the page and would drop the others: the
// form being submitted is the only one exempt. Compared by value, so typing
// something and undoing it is not a change; `current-password` is left out
// because a browser autofills it without anyone typing.
(function () {
  var forms = document.querySelectorAll("main form[method='post']");
  if (!forms.length) return;

  var values = function (form) {
    var out = [];
    new FormData(form).forEach(function (value, name) {
      var field = form.elements[name];
      if (typeof value !== "string") return;
      if (field && field.autocomplete === "current-password") return;
      out.push(name + "=" + value);
    });
    return out.join("&");
  };

  var loaded = [];
  for (var i = 0; i < forms.length; i++) loaded.push(values(forms[i]));

  var submitting = null;
  document.addEventListener("submit", function (event) {
    submitting = event.target;
  });
  // Back to a page kept in memory: the save that left it is no longer pending.
  window.addEventListener("pageshow", function () {
    submitting = null;
  });
  window.addEventListener("beforeunload", function (event) {
    for (var j = 0; j < forms.length; j++) {
      if (forms[j] !== submitting && values(forms[j]) !== loaded[j]) {
        event.preventDefault();
        return;
      }
    }
  });
})();

// A file input with `data-fills` reads the chosen file into the textarea that
// attribute names, so a restore can be picked rather than pasted.
document.addEventListener("change", function (event) {
  var input = event.target;
  var id = input.getAttribute && input.getAttribute("data-fills");
  if (!id || !input.files || !input.files[0]) return;
  var reader = new FileReader();
  reader.onload = function () { document.getElementById(id).value = reader.result; };
  reader.readAsText(input.files[0]);
});
