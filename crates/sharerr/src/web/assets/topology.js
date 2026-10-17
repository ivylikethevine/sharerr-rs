(function () {
  var addresses = document.getElementById("topo-hide-addresses");
  var networking = document.getElementById("topo-networking");
  var svg = document.getElementById("topo-diagram");
  var fullViewBox = svg.getAttribute("viewBox");
  var fullWidth = svg.getAttribute("width");
  var margin = 40;

  // Both toggles survive a reload: an operator who prefers the networking
  // view should not have to re-tick it every visit. Storage can be absent or
  // throw (private windows, blocked site data), and the page must still work.
  function remember(key, value) {
    try { localStorage.setItem(key, value ? "1" : "0"); } catch (e) {}
  }
  function recall(key, fallback) {
    try {
      var v = localStorage.getItem(key);
      return v === null ? fallback : v === "1";
    } catch (e) { return fallback; }
  }

  function applyAddresses() {
    var els = document.querySelectorAll(".topo-address");
    for (var i = 0; i < els.length; i++) {
      var el = els[i];
      var text = addresses.checked ? el.getAttribute("data-masked") : el.getAttribute("data-full");
      // An SVG <text> may carry a <title> child for its tooltip: replace only
      // the text node, not the whole content.
      var last = el.lastChild;
      if (last && last.nodeType === 3) { last.nodeValue = text; } else { el.appendChild(document.createTextNode(text)); }
    }
    remember("topo.hideAddresses", addresses.checked);
  }

  // Networking only: hide the sources lane and every edge into it, then
  // re-frame the viewBox to start at the instance column so the diagram does
  // not open on an empty third. The instance box's own x is the only
  // coordinate needed, so the layout math stays in Rust.
  function applyNetworking() {
    var on = networking.checked;
    var hide = document.querySelectorAll('[data-lane="source"], [data-kind="source"]');
    for (var i = 0; i < hide.length; i++) hide[i].classList.toggle("topo-hidden", on);
    var pathLabel = svg.querySelector('[data-kind="client"] .topo-edge-label');
    if (pathLabel) pathLabel.classList.toggle("topo-hidden", on);

    var instance = svg.querySelector('[data-lane="instance"] rect');
    if (on && instance) {
      var x0 = Math.max(0, parseInt(instance.getAttribute("x"), 10) - margin);
      var parts = fullViewBox.split(" ");
      var w = parseInt(parts[2], 10) - x0;
      svg.setAttribute("viewBox", x0 + " 0 " + w + " " + parts[3]);
      svg.setAttribute("width", String(w));
    } else {
      svg.setAttribute("viewBox", fullViewBox);
      svg.setAttribute("width", fullWidth);
    }
    remember("topo.networkingOnly", on);
  }

  // `?view=networking` in the URL wins over what was remembered, so the
  // networking view can be linked to from a README or a chat message.
  var wantsNetworking = /[?&]view=networking(&|$)/.test(location.search);
  addresses.checked = recall("topo.hideAddresses", addresses.checked);
  networking.checked = wantsNetworking || recall("topo.networkingOnly", false);
  addresses.addEventListener("change", applyAddresses);
  networking.addEventListener("change", applyNetworking);
  applyAddresses();
  applyNetworking();
})();
