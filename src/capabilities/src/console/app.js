// The console's shell: the sign-in panel, the header with its tabs and account card, and each
// tab's sections. The other console scripts register their renderers through
// window.pigeonConsole before the first render.
(() => {
  const PP = window.pigeonPasskey;
  const root = document.getElementById("console");
  const site = root.dataset.site || "pigeon";
  const IDLE_MS = 30 * 60 * 1000;
  const sections = {}, tabFns = {};
  let tabs = [], timer = null, menuEl = null, meBtn = null, gen = 0;

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
  const money = (n) => "$" + Number(n || 0).toFixed(2);

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
  const get = async (path) => {
    const r = await fetch(path, { credentials: "same-origin" });
    if (!r.ok) await fail(r);
    seen();
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
    clearInterval(timer); timer = null; menuEl = meBtn = null;
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
      api.me = await get("/console/api/me");
      tabs = await get("/console/api/tabs");
      render();
    } catch (e) {
      if (!e.auth) root.replaceChildren(el("main", { class: "c-signin" },
        el("section", { class: "card" }, el("p", { role: "alert", text: e.message || String(e) }))));
    }
  }

  const avatar = (me) => me.avatar
    ? el("img", { class: "c-avatar", src: me.avatar, alt: "", width: 44, height: 44 })
    : el("span", { class: "c-avatar c-initials", "aria-hidden": "true", text: me.initials });

  const navigate = (href) => { history.pushState(null, "", href); render(); };
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
        el("a", { href: "/settings#users", text: "Manage passkeys", onclick: (e) => { setMenu(false); linkClick(e); } })),
      status);
    meBtn = el("button", { class: "c-me", type: "button", "aria-expanded": "false", "aria-controls": "c-menu",
      "aria-label": `${me.name}: account`, onclick: () => setMenu(menuEl.hidden) }, avatar(me));
    tick();

    return el("header", { class: "c-header" },
      el("a", { class: "c-brand", href: "/", onclick: linkClick },
        el("img", { class: "c-icon", src: "/console/icon.png", alt: site, width: 44, height: 44 })),
      el("nav", { class: "c-tabs", "aria-label": "Console" },
        tabs.map((t) => el("a", { class: "c-tab", href: t.path, "aria-current": t === cur ? "page" : null, text: t.title, onclick: linkClick }))),
      el("div", { class: "c-right" }, time, el("div", { class: "c-me-wrap" }, meBtn, menuEl)));
  }

  async function loadSection(s, body, days, g) {
    try {
      const data = await get(`/console/api/section/${encodeURIComponent(s.id)}?days=${days}`);
      if (g !== gen) return;
      body.replaceChildren();
      const fn = sections[s.id];
      if (fn) await fn(body, data, { days, reload: (d) => loadSection(s, body, d ?? days, gen) });
      else if (data && typeof data.html === "string") body.innerHTML = data.html; // the server's own architecture drawing
      else body.append(el("p", { class: "muted", text: "Nothing to show yet." }));
    } catch (e) {
      if (g !== gen || e.auth) return;
      body.replaceChildren(el("p", { class: "c-error", role: "alert", text: e.message || String(e) }));
    }
  }

  async function render() {
    if (!api.me) return;
    const g = ++gen;
    const cur = tabs.find((t) => t.path === location.pathname)
      || tabs.find((t) => t.tab === "overview") || tabs[0];
    const main = el("main", { id: "c-main" }, el("h1", { class: "sr-only", text: cur ? cur.title : site }));
    root.replaceChildren(header(cur), main);
    if (!cur) return;
    try {
      if (tabFns[cur.tab]) await tabFns[cur.tab](main, { tabs });
      else {
        const list = [...(cur.sections || [])].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
        await Promise.all(list.map((s) => {
          const body = el("div", { class: "c-body" });
          // The tab already names a section titled like it, so its heading is for screen readers only.
          const head = s.title === cur.title
            ? el("h2", { id: `h-${s.id}`, class: "sr-only", text: s.title })
            : el("div", { class: "c-section-h" }, el("h2", { id: `h-${s.id}`, text: s.title }),
              s.description && el("p", { text: s.description }));
          main.append(el("section", { class: "c-section", id: s.id, "aria-labelledby": `h-${s.id}` }, head, body));
          return loadSection(s, body, 7, g);
        }));
      }
    } catch (e) {
      if (!e.auth && g === gen) main.append(el("p", { class: "c-error", role: "alert", text: e.message || String(e) }));
    }
    if (g !== gen || !location.hash) return;
    let target = null;
    try { target = document.getElementById(decodeURIComponent(location.hash.slice(1))); } catch {}
    if (target) target.scrollIntoView({ block: "start" });
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
