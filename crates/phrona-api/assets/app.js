"use strict";

const $ = (id) => document.getElementById(id);
const qs = (sel, root) => (root || document).querySelector(sel);
const qsa = (sel, root) => [...(root || document).querySelectorAll(sel)];

function esc(s) {
  const div = document.createElement("div");
  div.textContent = s ?? "";
  return div.innerHTML;
}

/* Only http(s) URLs may be navigated to or embedded. javascript:, data:,
   vbscript:, and malformed URLs become a dead link ("about:blank") so an
   attacker can never run code through href/src/window.open bindings. */
function sanitizeUrl(rawUrl) {
  if (typeof rawUrl !== "string" || !rawUrl.trim()) return "about:blank";
  let parsed;
  try {
    parsed = new URL(rawUrl);
  } catch {
    return "about:blank";
  }
  const proto = parsed.protocol.toLowerCase();
  if (proto !== "http:" && proto !== "https:") return "about:blank";
  return parsed.href;
}

/* ===================== theming ===================== */

const setTheme = (t) => {
  document.documentElement.dataset.theme = t;
  localStorage.setItem("phrona-theme", t);
  $("theme-btn").textContent = t === "dark" ? "\u263e" : "\u263d";
};
setTheme(localStorage.getItem("phrona-theme") || "light");
$("theme-btn").addEventListener("click", () =>
  setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark"));

/* ===================== views & tool tabs ===================== */

qsa(".topbar .tab").forEach((tab) =>
  tab.addEventListener("click", () => {
    qsa(".topbar .tab").forEach((t) => t.classList.toggle("active", t === tab));
    qsa("#view-search, #view-tools").forEach((v) => (v.hidden = v.id !== "view-" + tab.dataset.view));
  }));

qsa("#tool-tabs .tab").forEach((tab) =>
  tab.addEventListener("click", () => {
    qsa("#tool-tabs .tab").forEach((t) => t.classList.toggle("active", t === tab));
    qsa(".tool-panel").forEach((p) => (p.hidden = p.id !== "tool-" + tab.dataset.tool));
  }));

/* ===================== search state ===================== */

const state = { category: "web", engines: new Set(), allEngines: {}, busy: false };

/* API key: stored locally, sent only as the x-api-key header. The API
   rejects api_key in the query string (credential leakage into logs and
   referrers), so it must never appear in URLs or the saved-location hash. */
const apiKey = () => $("api-key").value.trim();
$("api-key").addEventListener("input", () =>
  localStorage.setItem("phrona-key", apiKey()));
if (localStorage.getItem("phrona-key")) $("api-key").value = localStorage.getItem("phrona-key");

const authHeaders = () => (apiKey() ? { "x-api-key": apiKey() } : {});

const buildParams = () => {
  const p = new URLSearchParams({
    q: $("q").value.trim(),
    category: state.category,
    max_results: $("max-results").value || "20",
    page: $("page").value || "1",
  });
  if (state.engines.size) p.set("engines", [...state.engines].join(","));
  for (const id of ["region", "language", "filters"]) {
    const v = $(id).value.trim();
    if (v) p.set(id, v);
  }
  const tr = $("time-range").value;
  if (tr) p.set("time_range", tr);
  const ss = $("safesearch").value;
  if (ss) p.set("safesearch", ss);
  const mode = $("source-policy-mode").value;
  if (mode && mode !== "any") p.set("source_policy_mode", mode);
  for (const [id, key] of [["allowed-domains", "allowed_domains"], ["excluded-domains", "excluded_domains"]]) {
    const v = $(id).value.trim();
    if (v) p.set(key, v);
  }
  return p;
};

const saveLocation = (p) => {
  const url = new URL(window.location.href);
  url.hash = p.toString();
  history.replaceState(null, "", url);
};

const restoreLocation = () => {
  const p = new URLSearchParams(location.hash.replace(/^#/, ""));
  if (!p.get("q")) return false;
  $("q").value = p.get("q");
  $("clear-btn").hidden = false;
  const cat = p.get("category");
  if (["web", "images", "news", "videos", "books"].includes(cat)) {
    state.category = cat;
    qsa("#category-row .chip").forEach((c) => c.classList.toggle("active", c.dataset.cat === cat));
  }
  for (const id of ["region", "language", "filters", "max-results", "page"]) {
    if (p.get(id)) $(id).value = p.get(id);
  }
  const tr = p.get("time_range");
  if (tr) $("time-range").value = tr;
  const ss = p.get("safesearch");
  if (ss) $("safesearch").value = ss;
  if (p.get("source_policy_mode")) $("source-policy-mode").value = p.get("source_policy_mode");
  for (const [id, key] of [["allowed-domains", "allowed_domains"], ["excluded-domains", "excluded_domains"]]) {
    if (p.get(key)) $(id).value = p.get(key);
  }
  if (p.get("engines")) state.engines = new Set(p.get("engines").split(",").filter(Boolean));
  renderEngines();
  return true;
};

/* ===================== engines ===================== */

async function loadEngines() {
  try {
    const r = await fetch("/v1/engines");
    state.allEngines = await r.json();
    renderEngines();
  } catch (e) {
    console.warn("engines unavailable:", e);
  }
}

function renderEngines() {
  const row = $("engines-row");
  row.innerHTML = "";
  for (const name of state.allEngines[state.category] || []) {
    const chip = document.createElement("button");
    chip.className = "chip" + (state.engines.has(name) ? " active" : "");
    chip.textContent = name;
    chip.addEventListener("click", () => {
      state.engines.has(name) ? state.engines.delete(name) : state.engines.add(name);
      chip.classList.toggle("active");
    });
    row.appendChild(chip);
  }
}

qsa("#category-row .chip").forEach((chip) =>
  chip.addEventListener("click", () => {
    qsa("#category-row .chip").forEach((c) => c.classList.remove("active"));
    chip.classList.add("active");
    state.category = chip.dataset.cat;
    state.engines.clear();
    renderEngines();
  }));

/* ===================== suggestions ===================== */

let suggestTimer = null;
$("q").addEventListener("input", () => {
  $("clear-btn").hidden = $("q").value.length === 0;
  clearTimeout(suggestTimer);
  if ($("q").value.trim().length < 2 || !$("suggest-toggle").checked) {
    $("suggestions").hidden = true;
    return;
  }
  suggestTimer = setTimeout(fetchSuggestions, 180);
});

async function fetchSuggestions() {
  try {
    const r = await fetch(`/v1/suggest?q=${encodeURIComponent($("q").value.trim())}`, { headers: authHeaders() });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const d = await r.json();
    const box = $("suggestions");
    box.textContent = "";
    const seen = new Set();
    let n = 0;
    for (const [, list] of Object.entries(d.suggestions || {})) {
      for (const s of list) {
        if (seen.has(s) || n >= 8) continue;
        seen.add(s);
        n++;
        const chip = document.createElement("button");
        chip.className = "chip";
        chip.textContent = s;
        chip.addEventListener("mousedown", (e) => e.preventDefault());
        chip.addEventListener("click", () => {
          $("q").value = s;
          box.hidden = true;
          doSearch();
        });
        box.appendChild(chip);
      }
    }
box.hidden = n === 0;
  } catch { /* ignore suggestion errors */ }
}

/* ===================== search ===================== */

$("search-btn").addEventListener("click", doSearch);
$("q").addEventListener("keydown", (e) => e.key === "Enter" && doSearch());
$("clear-btn").addEventListener("click", () => {
  $("q").value = "";
  $("clear-btn").hidden = true;
  $("q").focus();
});
$("prev-btn").addEventListener("click", () => {
  $("page").value = String(Math.max(1, (parseInt($("page").value, 10) || 1) - 1));
  doSearch();
});
$("next-btn").addEventListener("click", () => {
  $("page").value = String((parseInt($("page").value, 10) || 1) + 1);
  doSearch();
});
$("report-toggle").addEventListener("click", () => {
  const body = $("report-body");
  body.hidden = !body.hidden;
  $("report-caret").textContent = body.hidden ? "▸" : "▾";
});

async function doSearch() {
  if (!$("q").value.trim() || state.busy) return;
  state.busy = true;
  const resultsEl = $("results");
  resultsEl.innerHTML = '<div class="spinner"></div>';
  for (const id of ["meta", "answer", "empty", "pager", "engine-report"]) $(id).hidden = true;

  const params = buildParams();
  saveLocation(params);

  try {
    const r = await fetch(`/v1/search?${params}`, { headers: authHeaders() });
    const d = await r.json();
    if (!r.ok) throw new Error(d.error || `HTTP ${r.status}`);
    const ok = (d.engines || []).filter((e) => e.status === "ok" && e.results > 0);
    const err = (d.engines || []).filter((e) => e.status === "error");
    $("meta").textContent =
      `${d.total} results in ${d.elapsed_ms} ms · engines: ${ok.map((e) => e.name).join(", ") || "—"}` +
      (err.length ? ` · failed: ${err.map((e) => e.name).join(", ")}` : "");
    $("meta").hidden = false;
    if (d.answer) {
      $("answer").textContent = d.answer;
      $("answer").hidden = false;
    }
    renderResults(d);
    if (!d.results.length) $("empty").hidden = false;
    $("pager-info").textContent = `page ${d.page}`;
    $("pager").hidden = false;
    renderReport(d.engines || []);
  } catch (e) {
    resultsEl.innerHTML = `<div class="error-banner">Search failed: ${esc(e.message)}</div>`;
  } finally {
    state.busy = false;
  }
}

function renderResults(d) {
  const el = $("results");
  if ($("json-toggle").checked) {
    el.className = "results";
    el.innerHTML = `<pre class="json-view">${esc(JSON.stringify(d, null, 2))}</pre>`;
    return;
  }
  el.className = "results" + (state.category === "images" ? " images" : "");
  el.textContent = "";
  for (const r of d.results) el.appendChild(card(r));
  qsa(".vid-thumb img", el).forEach((img) => { img.onerror = () => img.parentElement.remove(); });
}

function card(r) {
  const sources = (r.engines || []).map((e) => `<span class="source-chip">${esc(e)}</span>`).join("");
  if (state.category === "images") {
    const wrap = document.createElement("article");
    wrap.className = "image-card";
    wrap.innerHTML = `<img loading="lazy" src="${esc(sanitizeUrl(r.image_url || r.thumbnail_url))}" alt="${esc(r.title)}" onerror="this.style.visibility='hidden'">
      <div class="cap"><span class="t">${esc(r.title)}</span><span class="d">${r.width ? `${esc(r.width)}×${esc(r.height)}` : ""}</span></div>`;
    wrap.addEventListener("click", () => {
      const u = sanitizeUrl(r.url);
      if (u !== "about:blank") window.open(u, "_blank", "noopener,noreferrer");
    });
    return wrap;
  }
  const sub = [
    r.published && `<span>${esc(r.published)}</span>`,
    r.source && `<span>${esc(r.source)}</span>`,
    r.author && `<span>${esc(r.author)}</span>`,
    r.publisher && `<span>${esc(r.publisher)}</span>`,
    r.duration && `<span>${esc(r.duration)}</span>`,
    r.views && `<span>${Number(r.views).toLocaleString()} views</span>`,
    r.uploader && `<span>${esc(r.uploader)}</span>`,
    r.source_tier && `<span>source: ${esc(r.source_tier)}${r.requested_match ? " · requested" : ""}</span>`,
  ].filter(Boolean).join("");
  const thumb = state.category === "videos" && r.thumbnail_url
    ? `<div class="vid-thumb"><img loading="lazy" src="${esc(sanitizeUrl(r.thumbnail_url))}" alt=""><span class="vid-dur">${esc(r.duration || "")}</span></div>`
    : "";
  const wrap = document.createElement("article");
  wrap.className = "card";
  wrap.innerHTML = `
    <h3><a href="${esc(sanitizeUrl(r.url))}" target="_blank" rel="noopener noreferrer">${esc(r.title)}</a></h3>
    <div class="url">${esc(r.url)}</div>
    ${thumb}
    ${r.description ? `<p class="desc">${esc(r.description)}</p>` : ""}
    ${sub ? `<div class="sub">${sub}</div>` : ""}
    <div class="sources">${sources}</div>`;
  return wrap;
}

function renderReport(engines) {
  $("engine-report").hidden = false;
  const body = $("report-body");
  body.innerHTML = `<table class="report-table"><thead><tr><th>engine</th><th>status</th><th>results</th><th>error</th></tr></thead><tbody>
    ${engines.map((e) => `<tr><td>${esc(e.name)}</td><td class="st-${esc(e.status)}">${esc(e.status)}</td><td>${esc(String(e.results))}</td><td class="err-cell">${esc(e.error || "")}</td></tr>`).join("")}
    </tbody></table>`;
}

/* ===================== tools (CLI parity) ===================== */

const TOOLS = {
  suggest: {
    endpoint: "/v1/suggest",
    params: (f) => {
      const p = new URLSearchParams({ q: f("q") });
      for (const k of ["source", "region"]) if (f(k)) p.set(k, f(k));
      return p;
    },
    render: (el, d) => {
      const map = d.suggestions || {};
      const rows = typeof map === "object" && !Array.isArray(map)
        ? Object.entries(map)
        : [[d.source || "", map]];
      el.innerHTML = rows.map(([k, list]) =>
        `<div class="row"><span class="k">${esc(k)}</span><span class="v">${esc((list || []).join("  ·  "))}</span></div>`).join("");
    },
  },
  extract: {
    endpoint: "/v1/extract",
    params: (f) => {
      const p = new URLSearchParams({ url: f("url"), max_chars: f("max_chars") || "5000" });
      if (f("query")) p.set("query", f("query"));
      addSourcePolicy(p, f);
      return p;
    },
    render: (el, d) => {
      el.innerHTML = `<h3>${esc(d.title)}</h3><div class="url">${esc(d.url)}</div>
        ${d.description ? `<p class="desc">${esc(d.description)}</p>` : ""}
        <pre class="extract-text">${esc(d.text)}</pre>
        ${d.images.length ? `<div class="sub">images: ${d.images.map(esc).join(" · ")}</div>` : ""}`;
    },
  },
  ground: {
    endpoint: "/v1/grounding",
    params: (f) => {
      const p = new URLSearchParams({ query: f("query"), max_results: f("max_results") || "8" });
      for (const k of ["category", "time_range"]) if (f(k)) p.set(k, f(k));
      addSourcePolicy(p, f);
      return p;
    },
    render: (el, d) => {
      el.innerHTML = `<div class="answer">${esc(d.answer)}</div>` +
        (d.sources || []).map((s) =>
          `<article class="card"><h3><a href="${esc(sanitizeUrl(s.url))}" target="_blank" rel="noopener noreferrer">${esc(s.title)}</a></h3>
           <div class="url">${esc(s.url)}</div><p class="desc">${esc(s.content)}</p>
           <div class="sub"><span>score ${esc(String(s.score))}</span><span>source: ${esc(s.source_tier || "unknown")}${s.requested_match ? " · requested" : ""}</span></div></article>`).join("");
    },
  },
  engines: {
    endpoint: "/v1/engines",
    params: (f) => {
      const p = new URLSearchParams();
      if (f("category")) p.set("category", f("category"));
      return p;
    },
    render: (el, d) => {
      el.innerHTML = Object.entries(d).map(([cat, list]) =>
        `<div class="row"><span class="k">${esc(cat)}</span><span class="v">${esc((list || []).join(", "))}</span></div>`).join("");
    },
  },
  test: {
    endpoint: "/v1/test",
    params: (f) => {
      const p = new URLSearchParams({ query: f("query") || "rust programming", max_results: f("max_results") || "5" });
      if (f("category")) p.set("category", f("category"));
      return p;
    },
    render: (el, d) => {
      el.innerHTML = (d || []).map((c) =>
        `<h3 class="tool-h3">${esc(c.category)} — ${esc(String(c.total))} results in ${esc(String(c.elapsed_ms))} ms</h3>
         <table class="report-table"><thead><tr><th>engine</th><th>status</th><th>results</th><th>error</th></tr></thead><tbody>
         ${(c.engines || []).map((e) => `<tr><td>${esc(e.name)}</td><td class="st-${esc(e.status)}">${esc(e.status)}</td><td>${esc(String(e.results))}</td><td class="err-cell">${esc(e.error || "")}</td></tr>`).join("")}
         </tbody></table>`).join("");
    },
  },
};

function addSourcePolicy(p, field) {
  if (field("source_policy_mode") && field("source_policy_mode") !== "any") {
    p.set("source_policy_mode", field("source_policy_mode"));
  }
  for (const key of ["allowed_domains", "excluded_domains"]) {
    if (field(key)) p.set(key, field(key));
  }
}

qsa("form.tool-form").forEach((form) => {
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const name = form.dataset.toolForm;
    const tool = TOOLS[name];
    const f = (k) => qs(`[data-f="${k}"]`, form)?.value;
    const out = qs(`[data-out="${name}"]`);
    const useJson = qs(".json-check", form).checked;
    out.innerHTML = '<div class="spinner small"></div>';
    try {
      const r = await fetch(`${tool.endpoint}?${tool.params(f)}`, { headers: authHeaders() });
      const d = await r.json();
      if (!r.ok) throw new Error(d.error || `HTTP ${r.status}`);
      if (useJson) {
        out.innerHTML = `<pre class="json-view">${esc(JSON.stringify(d, null, 2))}</pre>`;
      } else {
        out.textContent = "";
        tool.render(out, d);
      }
    } catch (err) {
      out.innerHTML = `<div class="error-banner">Failed: ${esc(err.message)}</div>`;
    }
  });
});

/* ===================== init ===================== */

loadEngines();
if (restoreLocation()) {
  // a shared link restores its full parameter set and runs immediately
  doSearch();
} else {
  $("empty").hidden = false;
}
$("q").focus();
