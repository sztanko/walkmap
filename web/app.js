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
  pathToggle: $("pathhover"),
};

const state = {
  manifest: null,
  city: null, // manifest city object
  type: null, // type id
  mode: "buildings", // "buildings" | "areas"
  opacity: 0.8,
  sites: [], // [pid, name, lng, lat, k]
  names: new Map(), // pid -> {n, k}
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
const LAYERS = [
  "areas-fill",
  "areas-line",
  "bld-fill",
  "bld-line",
  "sites-dot",
  "sites-label",
  "walkpath-casing",
  "walkpath-line",
  "walkpath-arrows",
  "walkpath-end",
];

function removeData() {
  for (const id of LAYERS) if (map.getLayer(id)) map.removeLayer(id);
  for (const s of [SRC, "sites", "walkpath"]) if (map.getSource(s)) map.removeSource(s);
}

const EMPTY = { type: "FeatureCollection", features: [] };

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
      paint: {
        "line-color": "rgba(255,255,255,0.85)",
        "line-width": ["interpolate", ["linear"], ["zoom"], 8, 0.4, 14, 1.6],
      },
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

  // hover walk path (above fills, below labels)
  map.addSource("walkpath", { type: "geojson", data: EMPTY });
  map.addLayer(
    {
      id: "walkpath-casing",
      type: "line",
      source: "walkpath",
      filter: ["==", "$type", "LineString"],
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": "rgba(20,24,40,0.55)", "line-width": 2.8 },
    },
    before
  );
  map.addLayer(
    {
      id: "walkpath-line",
      type: "line",
      source: "walkpath",
      filter: ["==", "$type", "LineString"],
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": "#ffffff", "line-width": 1.2 },
    },
    before
  );
  map.addLayer({
    id: "walkpath-arrows",
    type: "symbol",
    source: "walkpath",
    filter: ["==", "$type", "LineString"],
    layout: {
      "symbol-placement": "line",
      "symbol-spacing": 55,
      "text-field": ">",
      "text-size": 11,
      "text-font": ["Noto Sans Bold"],
      "text-keep-upright": false,
      "text-rotation-alignment": "map",
      "text-allow-overlap": true,
      "text-ignore-placement": true,
    },
    paint: {
      "text-color": "#1d2233",
      "text-halo-color": "rgba(255,255,255,0.9)",
      "text-halo-width": 1,
    },
  });
  map.addLayer({
    id: "walkpath-end",
    type: "circle",
    source: "walkpath",
    filter: ["==", "$type", "Point"],
    paint: {
      "circle-radius": 4,
      "circle-color": "#1d2233",
      "circle-stroke-color": "#fff",
      "circle-stroke-width": 1.5,
    },
  });

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
    paint: {
      "text-color": "#1d2233",
      "text-halo-color": "rgba(255,255,255,0.9)",
      "text-halo-width": 1.2,
    },
  });

  applyMode();
}

function siteLabel(name, k) {
  return k > 1 ? `${name} (×${k})` : name;
}

function sitesGeojson() {
  return {
    type: "FeatureCollection",
    features: state.sites.map(([pid, name, lng, lat, k]) => ({
      type: "Feature",
      properties: { pid, name: name ? siteLabel(name, k) : "" },
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
  document.querySelector(".legend-ramp-note").hidden = !b;
}

function applyOpacity() {
  if (map.getLayer("areas-fill")) {
    map.setPaintProperty("areas-fill", "fill-opacity", state.opacity);
    map.setPaintProperty("bld-fill", "fill-opacity", state.opacity);
  }
}

// ---------- walk-path raster ----------
const pathCache = { key: null, dirs: null, loading: null };

async function ensureDirs() {
  const key = `${state.city.id}/${state.type}`;
  if (pathCache.key === key && pathCache.dirs) return pathCache.dirs;
  if (pathCache.loading?.key === key) return pathCache.loading.promise;
  const promise = (async () => {
    const resp = await fetch(`${dataBase(state.city.id)}${state.type}.dirs.gz`);
    if (!resp.ok) throw new Error(`dirs ${resp.status}`);
    let buf = await resp.arrayBuffer();
    const head = new Uint8Array(buf, 0, 2);
    if (head[0] === 0x1f && head[1] === 0x8b) {
      // gzip magic — the server did not transparently decode it
      const ds = new Blob([buf]).stream().pipeThrough(new DecompressionStream("gzip"));
      buf = await new Response(ds).arrayBuffer();
    }
    const dirs = new Uint8Array(buf);
    pathCache.key = key;
    pathCache.dirs = dirs;
    pathCache.loading = null;
    return dirs;
  })();
  pathCache.loading = { key, promise };
  return promise;
}

// 1–8 = N,NE,E,SE,S,SW,W,NW as (dx, dy) with y pointing south (row index)
const DIRS = [null, [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1]];

function tracePath(lngLat, dirs) {
  const pg = state.city.pathGrid;
  if (!pg || !dirs || dirs.length !== pg.w * pg.h) return null;
  let x = Math.floor((lngLat.lng - pg.west) / pg.dlng);
  let y = Math.floor((pg.north - lngLat.lat) / pg.dlat);
  if (x < 0 || y < 0 || x >= pg.w || y >= pg.h) return null;
  const pts = [];
  const center = (x, y) => [pg.west + (x + 0.5) * pg.dlng, pg.north - (y + 0.5) * pg.dlat];
  const visited = new Set();
  for (let i = 0; i < 60000; i++) {
    const cell = y * pg.w + x;
    if (visited.has(cell)) break; // safety against degenerate cycles
    visited.add(cell);
    pts.push(center(x, y));
    const d = dirs[cell];
    if (!d) break;
    x += DIRS[d][0];
    y += DIRS[d][1];
    if (x < 0 || y < 0 || x >= pg.w || y >= pg.h) break;
  }
  if (pts.length < 2) return null;
  // one round of Chaikin to soften the 8-direction staircase
  const sm = [pts[0]];
  for (let i = 0; i < pts.length - 1; i++) {
    const [p, q] = [pts[i], pts[i + 1]];
    if (i > 0) sm.push([0.75 * p[0] + 0.25 * q[0], 0.75 * p[1] + 0.25 * q[1]]);
    if (i < pts.length - 2) sm.push([0.25 * p[0] + 0.75 * q[0], 0.25 * p[1] + 0.75 * q[1]]);
  }
  sm.push(pts[pts.length - 1]);
  return sm;
}

let pathBusy = false;
async function showPath(lngLat) {
  if (!els.pathToggle.checked || !map.getSource("walkpath")) return;
  if (pathBusy) return;
  pathBusy = true;
  try {
    const dirs = await ensureDirs();
    const line = tracePath(lngLat, dirs);
    const src = map.getSource("walkpath");
    if (!src) return;
    src.setData(
      line
        ? {
            type: "FeatureCollection",
            features: [
              { type: "Feature", properties: {}, geometry: { type: "LineString", coordinates: line } },
              { type: "Feature", properties: {}, geometry: { type: "Point", coordinates: line[line.length - 1] } },
            ],
          }
        : EMPTY
    );
  } catch {
    /* raster missing — silently no path */
  } finally {
    pathBusy = false;
  }
}
function clearPath() {
  map.getSource("walkpath")?.setData(EMPTY);
}

let hoverTimer = null;
map.on("mousemove", (e) => {
  if (!els.pathToggle.checked) return;
  clearTimeout(hoverTimer);
  hoverTimer = setTimeout(() => showPath(e.lngLat), 60);
});
map.getCanvas()?.addEventListener?.("mouseleave", clearPath);
els.pathToggle?.addEventListener("change", () => {
  if (!els.pathToggle.checked) clearPath();
});

// ---------- data switching ----------
async function loadSites() {
  try {
    const r = await fetch(`${dataBase(state.city.id)}${state.type}.sites.json`);
    state.sites = (await r.json()).sites;
  } catch {
    state.sites = [];
  }
  state.names = new Map(state.sites.map((s) => [s[0], { n: s[1], k: s[4] || 1 }]));
}

async function switchData(fit) {
  removeData();
  clearTimeout(hoverTimer);
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
    .map((s) => `<button data-ll="${s[2]},${s[3]}">${escapeHtml(siteLabel(s[1], s[4] || 1))}</button>`)
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
  if (!f) {
    return;
  }
  showPath(e.lngLat); // taps draw the path too (mobile has no hover)
  const p = f.properties;
  const site = state.names.get(p.pid) || { n: p.name || "—", k: 1 };
  const label = siteLabel(site.n || "—", site.k);
  const tn = state.manifest.types.find((t) => t.id === state.type)?.name || "";
  const line =
    state.mode === "buildings"
      ? `${fmtMin(p.t === "null" || p.t == null ? null : +p.t)} to <b>${escapeHtml(label)}</b>`
      : `Catchment of <b>${escapeHtml(label)}</b>`;
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
  state.city = state.manifest.cities.find((c) => c.id === h.city) || state.manifest.cities[0];
  state.type = state.city.types.includes(h.type) ? h.type : state.city.types[0];
  state.mode = h.mode;
  // hover is pointless on coarse pointers; keep the path for taps only
  if (!window.matchMedia("(pointer: fine)").matches) els.pathToggle.checked = true;
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
