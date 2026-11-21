// The console's shell: the sign-in panel, the header with its tabs and account card, and each
// tab's sections. Every section loads on its own: a skeleton while it waits, one quiet retry
// when the network drops, then a message with Retry; one slow or failed section never holds up
// the rest. The other console scripts register their renderers through
// window.pigeonConsole before the first render.
(() => {
  const PP = window.pigeonPasskey;
  const root = document.getElementById("console");
  const site = root.dataset.site || "pigeon";
  const IDLE_MS = 30 * 60 * 1000;
  const sections = {}, tabFns = {};
  const TIMEOUT_MS = 12000, POLL_MS = 30000;
  let tabs = [], timer = null, poller = null, menuEl = null, meBtn = null, gen = 0;

  const el = (tag, attrs, ...kids) => {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === false || v == null) continue;
      if (k === "text") n.textContent = v;
      else if (k.startsWith("on") && typeof v === "function") n.addEventListener(k.slice(2).toLowerCase(), v);
      else n.setAttribute(k, v === true ? "" : v);
    }
    for (const c of kids.flat()) if (c != null && c !== false) n.append(c instanceof Node ? c : String(c));
    return n;
  };
  // Cents, or four places for a cost under one cent that is not nothing.
  const money = (n) => {
    const v = Number(n || 0);
    return "$" + (v > 0 && v < 0.01 ? v.toFixed(4) : v.toFixed(2));
  };

  // A credential's expiry as a pill: its status, and the date or why it is not known. A row
  // carries only names, dates and statuses, never a value.
  const expiryPill = (r) => {
    const e = r.expiry || {};
    const left = r.days_left;
    const text = {
      ok: e.kind === "never" ? "no expiry" : `expires ${r.expires} · ${left} days`,
      soon: `expires ${r.expires} · ${left < 1 ? "today" : `${left} day${left === 1 ? "" : "s"}`}`,
      expired: e.kind === "refused" ? "refused by its issuer" : `expired ${r.expires}`,
      unknown: "expiry unknown",
      missing: "not set",
    }[r.status] || r.status;
    const why = e.why || (r.checked_ms ? `checked ${new Date(r.checked_ms).toISOString().slice(0, 16).replace("T", " ")} UTC` : "");
    return el("span", { class: `pill ${r.status}`, title: why || null, text });
  };

  // Each answered request moves the idle limit on, up to the session's absolute end.
  const seen = () => {
    if (api.me) api.me.expires_at = Math.min(Date.now() + IDLE_MS, api.me.ends_at);
  };
  const fail = async (r) => {
    if (r.status === 401) { signIn(); const e = new Error("Not signed in"); e.auth = true; throw e; }
    let msg = r.statusText || "Request failed";
    try { const t = await r.text(); try { const j = JSON.parse(t); msg = j.error || j.message || t; } catch { msg = t || msg; } } catch {}
    throw new Error(msg);
  };
  // A GET that gives up after TIMEOUT_MS. A dropped connection or a timeout is marked
  // `retry`, as is the server's own 504; meta, when given, gets the snapshot's time and staleness.
  const get = async (path, meta) => {
    const abort = new AbortController();
    const t = setTimeout(() => abort.abort(), TIMEOUT_MS);
    let r;
    try {
      r = await fetch(path, { credentials: "same-origin", signal: abort.signal });
    } catch (e) {
      const err = new Error(abort.signal.aborted
        ? `No answer within ${TIMEOUT_MS / 1000} seconds.`
        : "The connection dropped before an answer came.");
      err.retry = true;
      throw err;
    } finally {
      clearTimeout(t);
    }
    if (!r.ok) {
      try { await fail(r); } catch (e) { if (r.status === 504 || r.status === 503) e.retry = true; throw e; }
    }
    seen();
    if (meta) {
      meta.stale = r.headers.get("X-Snapshot-Stale") === "1";
      meta.at = Number(r.headers.get("X-Snapshot-At")) || null;
    }
    return r.json();
  };
  const post = async (path, body) => {
    const r = await fetch(path, { method: "POST", credentials: "same-origin",
      headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
    if (!r.ok) await fail(r);
    seen();
    const t = await r.text();
    try { return t ? JSON.parse(t) : {}; } catch { return {}; }
  };

  const api = { el, money, expiryPill, get, post, me: null, navigate: (href) => navigate(href),
    section: (id, fn) => { sections[id] = fn; },
    tab: (name, fn) => { tabFns[name] = fn; },
    render: () => render() };
  window.pigeonConsole = api;

  function signIn() {
    api.me = null; gen++;
    clearInterval(timer); timer = null; clearInterval(poller); poller = null; menuEl = meBtn = null;
    const status = el("p", { "data-status": true, role: "status" });
    const btn = el("button", { class: "button", type: "button", text: "Sign in with passkey", onclick: async () => {
      btn.disabled = true;
      try {
        PP.say("Waiting for your passkey…", false, status);
        const b = await PP.post("/console/sign-in/begin", {});
        const ans = await PP.approve(b.options);
        await PP.post("/console/sign-in/finish", { challenge_id: b.challenge_id, ...ans });
        await start();
      } catch (e) { PP.say(e.message || String(e), true, status); }
      finally { btn.disabled = false; }
    } });
    root.replaceChildren(el("main", { class: "c-signin" },
      el("section", { class: "card", "aria-label": "Sign in" },
        el("img", { class: "c-icon", src: "/console/icon.png", alt: site, width: 44, height: 44 }),
        el("p", { text: "Sign in with your passkey to open the console." }), btn, status)));
  }

  async function start() {
    try {
      [api.me, tabs] = await Promise.all([get("/console/api/me"), get("/console/api/tabs")]);
      render();
      clearInterval(poller);
      poller = setInterval(poll, POLL_MS);
    } catch (e) {
      if (!e.auth) root.replaceChildren(el("main", { class: "c-signin" },
        el("section", { class: "card" }, el("p", { role: "alert", text: e.message || String(e) }))));
    }
  }

  // While the page is open and visible, the tab list is asked again, so a landing that starts
  // waiting for the person shows up without a reload, and one approved elsewhere goes.
  async function poll() {
    if (!api.me || document.visibilityState !== "visible") return;
    try {
      const next = await get("/console/api/tabs");
      const was = waiting(tabs), now = waiting(next);
      tabs = next;
      if (was !== now) render();
    } catch {}
  }
  const waiting = (list) => list.reduce((n, t) => n + (t.waiting || 0), 0);
  // The Approvals tab is listed with how many wait; at none it is not shown.
  const visible = () => tabs.filter((t) => t.waiting !== 0);

  const avatar = (me) => me.avatar
    ? el("img", { class: "c-avatar", src: me.avatar, alt: "", width: 44, height: 44 })
    : el("span", { class: "c-avatar c-initials", "aria-hidden": "true", text: me.initials });

  const navigate = (href) => { history.pushState(null, "", href); scrolledTo = null; render(); };
  const linkClick = (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault(); navigate(e.currentTarget.getAttribute("href"));
  };

  function setMenu(open) {
    if (!menuEl) return;
    menuEl.hidden = !open;
    meBtn.setAttribute("aria-expanded", String(open));
  }

  // A picture is centre-cropped to 128 px square here, so the server keeps at most 64 KiB.
  function changePicture(input, status) {
    const f = input.files && input.files[0];
    input.value = "";
    if (!f) return;
    const url = URL.createObjectURL(f), img = new Image();
    img.onload = async () => {
      URL.revokeObjectURL(url);
      const c = el("canvas", { width: 128, height: 128 }), s = Math.min(img.width, img.height);
      c.getContext("2d").drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, 128, 128);
      const data = c.toDataURL("image/webp", 0.85);
      if (data.length > 65536) return PP.say("That picture is too large, even at 128×128. Try a simpler one.", true, status);
      try {
        PP.say("Saving…", false, status);
        await post("/console/api/avatar", { avatar: data });
        api.me = await get("/console/api/me");
        render();
      } catch (e) { PP.say(e.message || String(e), true, status); }
    };
    img.onerror = () => { URL.revokeObjectURL(url); PP.say("Could not read that image.", true, status); };
    img.src = url;
  }

  function header(cur) {
    const me = api.me;
    const time = el("span", { class: "c-time" });
    const tick = () => {
      if (!api.me) return;
      const m = Math.max(0, Math.ceil((api.me.expires_at - Date.now()) / 60000));
      if (m <= 0) return signIn();
      time.replaceChildren(el("span", { class: "dot", "aria-hidden": "true" }), `${m} min left`);
    };
    clearInterval(timer); timer = setInterval(tick, 30000);

    const status = el("p", { "data-status": true, role: "status" });
    const file = el("input", { type: "file", accept: "image/png,image/jpeg,image/webp", hidden: true,
      onchange: () => changePicture(file, status) });
    const until = new Date(me.ends_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    menuEl = el("div", { class: "card c-menu", id: "c-menu", role: "dialog", "aria-label": "Your account", hidden: true },
      el("div", { class: "c-menu-who" }, avatar(me),
        el("div", {}, el("strong", { text: me.name }), el("span", { class: "pill accent", text: "person" }))),
      el("dl", { class: "c-kv" },
        el("dt", { text: "Passkeys" }), el("dd", { text: (me.passkeys || []).join(", ") || "None" }),
        el("dt", { text: "Apps" }), el("dd", { text: String(me.apps ?? 0) }),
        el("dt", { text: "Session" }), el("dd", { text: `ends after 30 min idle, at ${until} at the latest` })),
      el("div", { class: "c-menu-actions" },
        el("button", { class: "button", type: "button", text: "Sign out", onclick: async () => {
          try { await post("/console/sign-out", {}); } catch (e) { if (e.auth) return; }
          signIn();
        } }),
        el("button", { class: "button c-quiet", type: "button", text: "Change picture", onclick: () => file.click() }),
        file,
        el("a", { href: "/access#people", text: "Manage passkeys", onclick: (e) => { setMenu(false); linkClick(e); } })),
      status);
    meBtn = el("button", { class: "c-me", type: "button", "aria-expanded": "false", "aria-controls": "c-menu",
      "aria-label": `${me.name}: account`, onclick: () => setMenu(menuEl.hidden) }, avatar(me));
    tick();

    return el("header", { class: "c-header" },
      el("a", { class: "c-brand", href: "/", onclick: linkClick },
        el("img", { class: "c-icon", src: "/console/icon.png", alt: site, width: 44, height: 44 })),
      el("nav", { class: "c-tabs", "aria-label": "Console" },
        visible().map((t) => el("a", { class: "c-tab", href: t.path, "aria-current": t === cur ? "page" : null, onclick: linkClick },
          t.title, t.waiting ? el("span", { class: "c-waiting", role: "img", "aria-label": `${t.waiting} waiting` }) : null))),
      el("div", { class: "c-right" }, time, el("div", { class: "c-me-wrap" }, meBtn, menuEl)));
  }

  // "updated 4 min ago" for a snapshot's time.
  const ago = (at) => {
    const m = Math.floor((Date.now() - at) / 60000);
    return m < 1 ? "updated just now" : m < 60 ? `updated ${m} min ago` : `updated ${Math.floor(m / 60)} h ago`;
  };

  // Loads section s into body and its freshness line into fresh. A section answered from a
  // stale snapshot is shown, then loaded fresh behind it; if that fails, the snapshot stays. A
  // reload after a write always asks fresh. A dropped connection is retried once, quietly.
  async function loadSection(s, body, fresh, days, g, opts = {}) {
    const { refresh, behind, retried } = opts;
    if (!behind) body.replaceChildren(el("div", { class: "card c-skel", "aria-busy": "true", "aria-label": `Loading ${s.title}` }, el("i"), el("i"), el("i")));
    const again = (o) => loadSection(s, body, fresh, days, g, o);
    try {
      const meta = {};
      const data = await get(`/console/api/section/${encodeURIComponent(s.id)}?days=${days}${refresh ? "&fresh=1" : ""}`, meta);
      if (g !== gen) return;
      body.replaceChildren();
      const fn = sections[s.id];
      const ctx = { days, reload: (d) => loadSection(s, body, fresh, d ?? days, gen, { refresh: true }) };
      if (fn) await fn(body, data, ctx);
      else if (data && typeof data.html === "string") body.innerHTML = data.html; // the server's own architecture drawing
      else body.append(el("p", { class: "muted", text: "Nothing to show yet." }));
      toHash();
      fresh.replaceChildren();
      if (meta.at) fresh.append(meta.stale ? "refreshing…" : ago(meta.at),
        el("button", { class: "c-link", type: "button", text: "refresh", onclick: () => ctx.reload() }));
      if (meta.stale && !refresh) again({ refresh: true, behind: true });
    } catch (e) {
      if (g !== gen || e.auth) return;
      if (e.retry && !retried) return setTimeout(() => again({ ...opts, retried: true }), 800);
      // A refresh behind a shown snapshot that fails leaves the snapshot in place.
      if (behind) { fresh.replaceChildren("refresh failed", el("button", { class: "c-link", type: "button", text: "retry", onclick: () => again({ refresh: true }) })); return; }
      body.replaceChildren(el("div", { class: "c-fail", role: "alert" },
        el("p", { text: `${s.title} did not load: ${e.message || e}. The rest of the console is unaffected.` }),
        el("button", { class: "button c-quiet", type: "button", text: "Retry", onclick: () => again({ refresh }) })));
    }
  }

  async function render() {
    if (!api.me) return;
    const g = ++gen;
    const shown = visible();
    const cur = shown.find((t) => t.path === location.pathname) || shown[0];
    const main = el("main", { id: "c-main" }, el("h1", { class: "sr-only", text: cur ? cur.title : site }));
    root.replaceChildren(header(cur), main);
    if (!cur) return;
    const waits = waiting(tabs);
    if (waits && cur.tab !== "approvals") {
      const at = tabs.find((t) => t.tab === "approvals");
      main.append(el("div", { class: "c-banner", role: "status" },
        el("span", { class: "pill warn", text: "needs you" }),
        el("p", { text: `${waits === 1 ? "A landing waits" : `${waits} landings wait`} for your passkey.` }),
        el("a", { class: "button c-quiet", href: at ? at.path : "/approvals", text: "Review", onclick: linkClick })));
    }
    try {
      if (tabFns[cur.tab]) await tabFns[cur.tab](main, { tabs });
      else {
        const list = [...(cur.sections || [])].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
        for (const s of list) {
          const body = el("div", { class: "c-body" });
          const fresh = el("span", { class: "c-fresh" });
          const head = el("div", { class: "c-section-h" }, el("h2", { id: `h-${s.id}`, text: s.title }), fresh,
            s.description && el("p", { text: s.description }));
          main.append(el("section", { class: "c-section", id: s.id, "aria-labelledby": `h-${s.id}` }, head, body));
          loadSection(s, body, fresh, 7, g); // each on its own; none waits for another
        }
      }
    } catch (e) {
      if (!e.auth && g === gen) main.append(el("p", { class: "c-error", role: "alert", text: e.message || String(e) }));
    }
    if (g === gen) toHash();
  }

  // Scrolls to the element the fragment names, once it exists: a section at once, a secret's
  // row when its section has loaded. Only the first time it appears, so a refresh doesn't jump.
  let scrolledTo = null;
  function toHash() {
    if (!location.hash || scrolledTo === location.href) return;
    let target = null;
    try { target = document.getElementById(decodeURIComponent(location.hash.slice(1))); } catch {}
    if (target) { scrolledTo = location.href; target.scrollIntoView({ block: "start" }); }
  }

  addEventListener("popstate", () => render());
  document.addEventListener("click", (e) => {
    if (menuEl && !menuEl.hidden && !e.target.closest(".c-me-wrap")) setMenu(false);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && menuEl && !menuEl.hidden) { setMenu(false); meBtn.focus(); }
  });

  // Deferred scripts all run before DOMContentLoaded, so every renderer is registered by then.
  if (document.readyState === "complete") start();
  else document.addEventListener("DOMContentLoaded", start);
})();
