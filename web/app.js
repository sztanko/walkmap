/* walkmap UI — MapLibre + PMTiles, no build step. */
"use strict";

const BASEMAP = "https://tiles.openfreemap.org/styles/positron";
const LOCAL = ["localhost", "127.0.0.1"].includes(location.hostname);

const $ = (id) => document.getElementById(id);
const els = {
  city: $("city"),
  ftype: $("ftype"),
  q: $("q"),
  results: $("results"),
  opacity: $("opacity"),
  modeBuildings: $("mode-buildings"),
  modeAreas: $("mode-areas"),
};

const state = {
  manifest: null,
  city: null, // manifest city object
  type: null, // type id
  mode: "buildings", // "buildings" | "areas"
  opacity: 0.8,
  sites: [], // [pid, name, lng, lat] for current city+type
  names: new Map(), // pid -> name
};

// ---------- URL hash: #city/type/mode/zoom/lat/lng ----------
function readHash() {
  const p = location.hash.replace(/^#/, "").split("/");
  return {
    city: p[0] || null,
    type: p[1] || null,
    mode: p[2] === "areas" ? "areas" : "buildings",
    view: p.length >= 6 ? { z: +p[3], lat: +p[4], lng: +p[5] } : null,
  };
}
let hashTimer;
function writeHash() {
  clearTimeout(hashTimer);
  hashTimer = setTimeout(() => {
    const c = map.getCenter();
    const h = `#${state.city.id}/${state.type}/${state.mode}/${map.getZoom().toFixed(2)}/${c.lat.toFixed(5)}/${c.lng.toFixed(5)}`;
    history.replaceState(null, "", h);
  }, 200);
}

function dataBase(cityId) {
  if (LOCAL) return `/data/out/${cityId}/`;
  return state.manifest.dataUrlTemplate.replace("{city}", cityId);
}

// ---------- map ----------
const protocol = new pmtiles.Protocol();
maplibregl.addProtocol("pmtiles", protocol.tile);

const map = new maplibregl.Map({
  container: "map",
  style: BASEMAP,
  center: [-16.93, 32.65],
  zoom: 12,
  attributionControl: { compact: true },
});
map.addControl(new maplibregl.NavigationControl({ visualizePitch: false }), "bottom-right");
map.addControl(new maplibregl.GeolocateControl({ trackUserLocation: true }), "bottom-right");

// insert data layers under the basemap's first symbol (label) layer
function firstSymbolId() {
  for (const l of map.getStyle().layers) if (l.type === "symbol") return l.id;
  return undefined;
}

const SRC = "walk";
const LAYERS = ["areas-fill", "areas-line", "bld-fill", "bld-line", "sites-dot", "sites-label"];

function removeData() {
  for (const id of LAYERS) if (map.getLayer(id)) map.removeLayer(id);
  if (map.getSource(SRC)) map.removeSource(SRC);
  if (map.getSource("sites")) map.removeSource("sites");
}

function addData() {
  const url = `pmtiles://${dataBase(state.city.id)}${state.type}.pmtiles`;
  const before = firstSymbolId();
  map.addSource(SRC, { type: "vector", url });

  map.addLayer(
    {
      id: "areas-fill",
      type: "fill",
      source: SRC,
      "source-layer": "partitions",
      paint: { "fill-color": ["to-color", ["get", "c"], "#999"], "fill-opacity": state.opacity },
    },
    before
  );
  map.addLayer(
    {
      id: "areas-line",
      type: "line",
      source: SRC,
      "source-layer": "partitions",
      paint: { "line-color": "rgba(255,255,255,0.85)", "line-width": ["interpolate", ["linear"], ["zoom"], 8, 0.4, 14, 1.6] },
    },
    before
  );
  map.addLayer(
    {
      id: "bld-fill",
      type: "fill",
      source: SRC,
      "source-layer": "buildings",
      paint: { "fill-color": ["to-color", ["get", "c"], "#999"], "fill-opacity": state.opacity },
    },
    before
  );
  map.addLayer(
    {
      id: "bld-line",
      type: "line",
      source: SRC,
      "source-layer": "buildings",
      minzoom: 15,
      paint: { "line-color": "rgba(20,24,40,0.25)", "line-width": 0.5 },
    },
    before
  );

  // feature sites (from sites.json) — dots + names at high zoom
  map.addSource("sites", { type: "geojson", data: sitesGeojson() });
  map.addLayer({
    id: "sites-dot",
    type: "circle",
    source: "sites",
    minzoom: 12,
    paint: {
      "circle-radius": ["interpolate", ["linear"], ["zoom"], 12, 2.5, 16, 5],
      "circle-color": "#1d2233",
      "circle-stroke-color": "#fff",
      "circle-stroke-width": 1.2,
    },
  });
  map.addLayer({
    id: "sites-label",
    type: "symbol",
    source: "sites",
    minzoom: 14,
    layout: {
      "text-field": ["get", "name"],
      "text-size": 11.5,
      "text-offset": [0, 1],
      "text-anchor": "top",
      "text-font": ["Noto Sans Regular"],
    },
    paint: { "text-color": "#1d2233", "text-halo-color": "rgba(255,255,255,0.9)", "text-halo-width": 1.2 },
  });

  applyMode();
}

function sitesGeojson() {
  return {
    type: "FeatureCollection",
    features: state.sites.map(([pid, name, lng, lat]) => ({
      type: "Feature",
      properties: { pid, name: name || "" },
      geometry: { type: "Point", coordinates: [lng, lat] },
    })),
  };
}

function applyMode() {
  const b = state.mode === "buildings";
  const vis = (on) => (on ? "visible" : "none");
  if (map.getLayer("bld-fill")) {
    map.setLayoutProperty("bld-fill", "visibility", vis(b));
    map.setLayoutProperty("bld-line", "visibility", vis(b));
    map.setLayoutProperty("areas-fill", "visibility", vis(!b));
    map.setLayoutProperty("areas-line", "visibility", "visible"); // outlines help in both modes
    map.setPaintProperty("areas-line", "line-color", b ? "rgba(29,34,51,0.35)" : "rgba(255,255,255,0.85)");
  }
  els.modeBuildings.classList.toggle("on", b);
  els.modeAreas.classList.toggle("on", !b);
  document.querySelector(".legend-buildings").hidden = !b;
  document.querySelector(".legend-areas").hidden = b;
}

function applyOpacity() {
  if (map.getLayer("areas-fill")) {
    map.setPaintProperty("areas-fill", "fill-opacity", state.opacity);
    map.setPaintProperty("bld-fill", "fill-opacity", state.opacity);
  }
}

// ---------- data switching ----------
async function loadSites() {
  try {
    const r = await fetch(`${dataBase(state.city.id)}${state.type}.sites.json`);
    state.sites = (await r.json()).sites;
  } catch {
    state.sites = [];
  }
  state.names = new Map(state.sites.map((s) => [s[0], s[1]]));
}

async function switchData(fit) {
  removeData();
  await loadSites();
  addData();
  const tn = state.manifest.types.find((t) => t.id === state.type)?.name || state.type;
  for (const el of document.querySelectorAll(".tname")) el.textContent = tn.toLowerCase().replace(/s$/, "");
  if (fit) {
    const [w, s, e, n] = state.city.bbox;
    map.fitBounds([[w, s], [e, n]], { padding: 24, duration: 800 });
  }
  writeHash();
}

function fillSelectors() {
  els.city.innerHTML = state.manifest.cities
    .map((c) => `<option value="${c.id}"${c.id === state.city.id ? " selected" : ""}>${c.name}</option>`)
    .join("");
  els.ftype.innerHTML = state.manifest.types
    .filter((t) => state.city.types.includes(t.id))
    .map((t) => `<option value="${t.id}"${t.id === state.type ? " selected" : ""}>${t.name}</option>`)
    .join("");
}

// ---------- search ----------
function search(qs) {
  const q = qs.trim().toLowerCase();
  if (q.length < 2) return [];
  return state.sites.filter((s) => s[1] && s[1].toLowerCase().includes(q)).slice(0, 8);
}
function renderResults(items) {
  if (!items.length) {
    els.results.hidden = true;
    return;
  }
  els.results.innerHTML = items
    .map((s) => `<button data-ll="${s[2]},${s[3]}">${escapeHtml(s[1])}</button>`)
    .join("");
  els.results.hidden = false;
}
function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
let marker;
els.q.addEventListener("input", () => renderResults(search(els.q.value)));
els.results.addEventListener("click", (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  const [lng, lat] = b.dataset.ll.split(",").map(Number);
  map.flyTo({ center: [lng, lat], zoom: 15.5 });
  if (marker) marker.remove();
  marker = new maplibregl.Marker({ color: "#1d2233" }).setLngLat([lng, lat]).addTo(map);
  els.results.hidden = true;
  els.q.blur();
});
document.addEventListener("click", (e) => {
  if (!e.target.closest(".search")) els.results.hidden = true;
});

// ---------- popups ----------
function fmtMin(t) {
  if (t == null) return "walking time unknown";
  const m = Math.round(t / 60);
  return m < 1 ? "under a minute's walk" : `${m} min walk`;
}
map.on("click", (e) => {
  const layers = (state.mode === "buildings" ? ["bld-fill"] : ["areas-fill"]).filter((l) => map.getLayer(l));
  if (!layers.length) return;
  const f = map.queryRenderedFeatures(e.point, { layers })[0];
  if (!f) return;
  const p = f.properties;
  const name = state.names.get(p.pid) || p.name || "—";
  const tn = state.manifest.types.find((t) => t.id === state.type)?.name || "";
  const line =
    state.mode === "buildings"
      ? `${fmtMin(p.t === "null" || p.t == null ? null : +p.t)} to <b>${escapeHtml(String(name))}</b>`
      : `Catchment of <b>${escapeHtml(String(name))}</b>`;
  new maplibregl.Popup({ closeButton: false, maxWidth: "280px" })
    .setLngLat(e.lngLat)
    .setHTML(
      `<div class="popup-name"><span class="popup-swatch" style="background:${p.c}"></span>${escapeHtml(tn)}</div>
       <div class="popup-time">${line}</div>`
    )
    .addTo(map);
});
for (const l of ["bld-fill", "areas-fill"]) {
  map.on("mouseenter", l, () => (map.getCanvas().style.cursor = "pointer"));
  map.on("mouseleave", l, () => (map.getCanvas().style.cursor = ""));
}

// ---------- controls ----------
els.city.addEventListener("change", async () => {
  state.city = state.manifest.cities.find((c) => c.id === els.city.value);
  if (!state.city.types.includes(state.type)) state.type = state.city.types[0];
  fillSelectors();
  els.q.value = "";
  await switchData(true);
});
els.ftype.addEventListener("change", async () => {
  state.type = els.ftype.value;
  els.q.value = "";
  await switchData(false);
});
els.modeBuildings.addEventListener("click", () => {
  state.mode = "buildings";
  applyMode();
  writeHash();
});
els.modeAreas.addEventListener("click", () => {
  state.mode = "areas";
  applyMode();
  writeHash();
});
els.opacity.addEventListener("input", () => {
  state.opacity = els.opacity.value / 100;
  applyOpacity();
});
map.on("moveend", writeHash);

// ---------- boot ----------
(async function boot() {
  const r = await fetch("data/manifest.json");
  state.manifest = await r.json();
  const h = readHash();
  state.city =
    state.manifest.cities.find((c) => c.id === h.city) || state.manifest.cities[0];
  state.type = state.city.types.includes(h.type) ? h.type : state.city.types[0];
  state.mode = h.mode;
  fillSelectors();
  map.on("load", async () => {
    await loadSites();
    addData();
    if (h.view) {
      map.jumpTo({ center: [h.view.lng, h.view.lat], zoom: h.view.z });
    } else {
      const [w, s, e, n] = state.city.bbox;
      map.fitBounds([[w, s], [e, n]], { padding: 24, duration: 0 });
    }
  });
})();
