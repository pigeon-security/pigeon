/* The current console's shell at /app, kept until every view is ported to the new shell at
 * /app/next (shell.js); see docs/plans/console-redesign.md for its removal.
 *
 * Phase 2 API — window.pigeonApp (available to all deferred app scripts).
 * register(tab, render) or register(tab, {subtabs: {key: 'Label', ...}, render}).
 * Keys: sessions, cost, architecture, linked-home, linked-settings.
 * registerSettingsPanel(key, render) contributes to the single platform Settings view.
 * The latter two use connections.js and URL tabs 'home' and 'settings'. The
 * first declared subtab is the default when a view has subtabs.
 * render({tab, subtab, instance, signal, op}) returns a Node or Promise<Node>.
 * The shell owns the heading, loading and retry UI. Return detached nodes; the
 * shell discards results on navigation. Use scoped op for fetch cancellation
 * and signal to clean up any other work. Aborting cannot undo a write. Confirm
 * writes in the view, and never put mutations in render (which can run again).
 *
 * op(name, args={}) resolves to the tool's data or rejects with Error (status
 * on HTTP errors); a 401 returns to passkey sign-in. Use /approvals and pass
 * args.approval for the exact call if needed. See the allowlist in ops.rs.
 * Calls are to this platform, not forwarded to the selected account. instance
 * (also available as selectedInstance) is the instances_list record or null.
 *
 * url({tab, subtab, instance}) builds /app?instance=…&tab=…&subtab=…;
 * nav(options) pushes that URL and renders. Omitted instance keeps the current
 * one. Unknown tabs/instances fall back to the account's first tab/Home.
 * el(tag, {class, text, ...attrs}, ...children) inserts text, never HTML.
 * Use addEventListener for events and same-origin or HTTPS paths for href/src.
 * card(...children) -> section; pill(text, tone) -> span (running/failed/done).
 */
(() => {
  "use strict";
  const root = document.getElementById("pigeon-app");
  const passkey = window.pigeonPasskey;
  const views = new Map();
  const workspaceTabs = new Map();
  const settingsPanels = new Map();
  const settingsTabs = { access: "Access", models: "Models", connections: "Connections",
    people: "People", capabilities: "Capabilities" };
  views.set("settings", { subtabs: settingsTabs, render: (ctx) => settingsPanels.get(ctx.subtab)(ctx) });
  const coreTabs = { sessions: "Sessions", cost: "Cost", architecture: "Architecture" };
  const linkedTabs = { home: "Home" };
  const tags = new Set("a article aside b button caption code dd details div dl dt em fieldset footer form h1 h2 h3 h4 header hr img input label legend li main nav ol option p pre section select small span strong summary table tbody td textarea th thead time tr ul".split(" "));
  const attrsAllowed = /^(?:aria-[\w-]+|data-[\w-]+|id|class|role|tabindex|title|href|src|alt|width|height|type|name|value|placeholder|disabled|open|hidden|spellcheck|selected|checked|for|autocomplete|rows|scope|datetime|min|max|step)$/;
  function el(tag, attrs = {}, ...children) {
    if (!tags.has(tag)) throw new TypeError(`Unsupported tag: ${tag}`);
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (value == null || value === false) continue;
      if (key === "text") node.textContent = value;
      else if (!attrsAllowed.test(key)) throw new TypeError(`Unsupported attribute: ${key}`);
      else if (key === "href" || key === "src") {
        const target = new URL(value, location.href);
        if (target.origin !== location.origin && target.protocol !== "https:") throw new TypeError("Unsupported URL");
        node.setAttribute(key, target.href);
      } else node.setAttribute(key, value === true ? "" : String(value));
    }
    for (const child of children.flat(Infinity)) if (child != null && child !== false) node.append(child);
    return node;
  }
  const card = (...children) => el("section", { class: "app-card" }, ...children);
  const pill = (text, tone) => el("span", {
    class: ["running", "failed", "done"].includes(tone) ? `app-pill app-${tone}` : "app-pill", text,
  });
  const button = (text, action, css = "app-button") => {
    const node = el("button", { class: css, type: "button", text });
    node.addEventListener("click", action);
    return node;
  };

  let instances = [];
  let selected = null;
  let person = root.dataset.person;
  let viewLoad, instanceLoad, main, sidebar;

  async function op(name, args = {}, signal) {
    const res = await fetch("/app/op", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, args }), signal,
    });
    if (res.status === 401 && !signal?.aborted && person !== undefined) signIn();
    const body = await res.json().catch(() => null);
    if (!res.ok || !body || !Object.hasOwn(body, "data")) {
      const err = new Error(body?.error || `Operation failed (${res.status}).`);
      err.status = res.status;
      throw err;
    }
    return body.data;
  }
  function url({ tab, subtab, instance = selected?.id } = {}) {
    const query = new URLSearchParams();
    if (instance) query.set("instance", instance);
    if (tab) query.set("tab", tab);
    if (subtab) query.set("subtab", subtab);
    return `/app${query.size ? `?${query}` : ""}`;
  }
  function nav(options) {
    const to = url(options);
    if (to !== location.pathname + location.search) history.pushState(null, "", to);
    render(true);
  }
  function register(tab, view) {
    if (views.has(tab)) throw new Error(`Duplicate app view: ${tab}`);
    views.set(tab, typeof view === "function" ? { render: view } : view);
  }
  function registerWorkspaceTab(key, title, render) {
    register(key, render);
    workspaceTabs.set(key, title);
  }
  function registerSettingsPanel(key, render) {
    if (!Object.hasOwn(settingsTabs, key) || settingsPanels.has(key)) throw new Error(`Invalid settings panel: ${key}`);
    settingsPanels.set(key, render);
  }
  window.pigeonApp = Object.freeze({ register, registerWorkspaceTab, registerSettingsPanel, op, url, nav, el, card, pill,
    refresh: () => render(),
    get selectedInstance() { return selected; },
  });

  function routeLink(title, options, current) {
    const link = el("a", { href: url(options), text: title, "aria-current": current ? "page" : null });
    link.addEventListener("click", (event) => {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      nav(options);
    });
    return link;
  }
  const site = root.dataset.site;
  const brand = () => el("div", { class: "app-brand" },
    el("img", { src: "/console/icon.png", width: 28, height: 28, alt: "" }), site);
  const label = (text) => el("p", { class: "app-label", text });
  const notice = (text, role = "status") => el("p", { class: "app-muted", role, text });
  const isLinked = (instance) => instance?.account?.kind === "linked";
  function instanceRow(instance) {
    const title = instance?.title || site;
    const host = instance?.zones?.[0] || (isLinked(instance) ? "No host" : location.host);
    return el("span", { class: "app-instance-row" },
      el("span", { class: "app-initial", "aria-hidden": "true", text: ([...title][0] || "?").toUpperCase() }),
      el("span", { class: "app-instance-text" }, el("strong", { text: title }),
        el("small", { text: host }),
        el("small", { text: isLinked(instance) ? "Linked account" : "Home account" })));
  }

  function signIn() {
    person = undefined;
    selected = null;
    instances = [];
    viewLoad?.abort();
    instanceLoad?.abort();
    const status = notice("");
    const login = button("Sign in with passkey", async () => {
      login.disabled = true;
      passkey.say("Waiting for your passkey…", false, status);
      try {
        const begin = await passkey.post("/console/sign-in/begin", {});
        const assertion = await passkey.approve(begin.options);
        await passkey.post("/console/sign-in/finish", { challenge_id: begin.challenge_id, ...assertion });
        location.reload();
      } catch (err) {
        passkey.say(err.message, true, status);
        login.disabled = false;
      }
    }, "app-button app-primary");
    root.replaceChildren(el("main", { class: "app-sign-in" }, card(
      brand(), el("h1", { text: "Sign in" }), notice("Use your passkey to open the app.", null), login, status)));
  }

  function chrome() {
    sidebar = el("aside", { class: "app-sidebar" });
    main = el("main", { class: "app-main", id: "app-main", tabindex: "-1" });
    root.replaceChildren(el("a", { class: "app-skip", href: "#app-main", text: "Skip to content" }),
      el("div", { class: "app-layout" }, sidebar, main));
  }

  function side(tabs, tab) {
    const switcher = el("details", { class: "app-switcher" },
      el("summary", { "aria-label": `Switch instance (current: ${selected?.title || site})` },
        instanceRow(selected), el("span", { "aria-hidden": "true", text: "▾" })));
    if (instances.length) {
      const list = el("ul", { "aria-label": "Instances" });
      // The platform exists even before its home record has been saved.
      const choices = instances.some((i) => i.account?.kind === "home") ? instances : [null, ...instances];
      for (const instance of choices) {
        const link = routeLink("", { instance: instance?.id ?? null }, selected?.id === instance?.id);
        link.append(instanceRow(instance));
        list.append(el("li", {}, link));
      }
      switcher.append(list);
    } else switcher.append(notice("No instances to select."));
    const links = el("nav", { "aria-label": "Instance navigation" }, ...Object.entries(tabs)
      .map(([key, title]) => routeLink(title, { tab: key }, key === tab)));
    const status = notice("");
    const logout = button("Sign out", async () => {
      logout.disabled = true;
      try {
        await passkey.post("/console/sign-out", {});
        signIn();
      } catch (err) {
        status.textContent = err.message;
        logout.disabled = false;
      }
    });
    sidebar.replaceChildren(brand(), switcher, label("Workspace"), links,
      el("footer", { class: "app-side-footer" },
        routeLink("Approvals", { tab: "approvals" }, tab === "approvals"),
        routeLink("Instances", { tab: "instances" }, tab === "instances"),
        el("p", { class: "app-muted", text: person }), logout, status));
  }

  async function render(focus = false) {
    if (person === undefined || instanceLoad) return;
    viewLoad?.abort();
    const { signal } = viewLoad = new AbortController();
    const query = new URLSearchParams(location.search);
    selected = instances.find((i) => i.id === query.get("instance")) ||
      instances.find((i) => i.account?.kind === "home") || null;
    const linked = isLinked(selected);
    const tabs = { ...(linked ? linkedTabs : coreTabs), ...Object.fromEntries(workspaceTabs), settings: "Settings" };
    const management = { approvals: "Approvals", instances: "Instances", management: "Access" };
    const titles = { ...tabs, ...management };
    const asked = query.get("tab");
    const tab = Object.hasOwn(titles, asked) ? asked : Object.keys(tabs)[0];
    const view = views.get(linked && !Object.hasOwn(management, tab) && !workspaceTabs.has(tab) ? `linked-${tab}` : tab);
    const subtabs = view?.subtabs || {};
    let sub = query.get("subtab");
    if (!sub && tab === "management" && location.hash) {
      sub = location.hash === "#people" ? "people" :
        location.hash === "#delegation" ? "delegation" : "credentials";
    }
    if (tab === "cost") {
      try {
        if (!sub) sub = localStorage.getItem("pigeon.cost.period");
        else if (Object.hasOwn(subtabs, sub)) localStorage.setItem("pigeon.cost.period", sub);
      } catch { /* The URL still works when storage is unavailable. */ }
    }
    const subtab = Object.hasOwn(subtabs, sub) ? sub : Object.keys(subtabs)[0] || null;
    side(tabs, tab);
    const heading = el("h1", { tabindex: "-1", text: titles[tab] });
    const outlet = el("div", { class: "app-view" });
    main.replaceChildren(label(selected?.title || site), heading);
    if (subtab) main.append(el("nav", { class: "app-subtabs", "aria-label": `${titles[tab]} sections` },
      ...Object.entries(subtabs).map(([key, title]) => routeLink(title, { tab, subtab: key }, key === subtab))));
    main.append(outlet);
    if (focus) heading.focus();
    if (!view) {
      outlet.append(card(notice("This view is not available yet.", null)));
      return;
    }
    outlet.append(notice("Loading…"));
    try {
      const node = await view.render({ tab, subtab, instance: selected, signal,
        op: (name, args) => op(name, args, signal) });
      if (!signal.aborted) outlet.replaceChildren(node);
    } catch (err) {
      if (!signal.aborted) outlet.replaceChildren(card(notice(err.message || String(err), "alert"),
        button("Retry", () => render(true))));
    }
  }

  async function start() {
    if (person === undefined) return signIn();
    chrome();
    instanceLoad?.abort();
    const { signal } = instanceLoad = new AbortController();
    side({}, null);
    main.replaceChildren(el("h1", { text: site }), notice("Loading instances…"));
    try {
      const data = await op("instances_list", {}, signal);
      if (signal.aborted) return;
      if (!Array.isArray(data)) throw new Error("The instance list could not be read.");
      instances = data;
      instanceLoad = null;
      render();
    } catch (err) {
      if (signal.aborted) return;
      main.replaceChildren(el("h1", { text: site }), card(notice(err.message, "alert"), button("Retry", start)));
    }
  }

  window.addEventListener("popstate", () => render(true));
  document.addEventListener("DOMContentLoaded", start, { once: true });
})();
