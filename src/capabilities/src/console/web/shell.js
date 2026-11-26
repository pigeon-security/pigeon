/* The console shell at /app (docs/plans/console-redesign.md): the frame every view lives in, and
 * the one component set views build from (P13).
 *
 * What the shell owns:
 * - The sidebar: the instance switcher at the top, the only way to pick an instance (P1); the
 *   nav, built only from console_manifest(instance) (M3), Inbox first with its count (P5), then
 *   the selected instance's areas in the manifest's order, with no group headings: each area
 *   is there because a capability enabled on the instance fills it
 *   (docs/plans/capability-scoping.md). Each area's `group` is not read.
 * - Routing: <base>/<instance>/<area>/<view>, from the manifest's stable ids (P12). The
 *   Worker redirects the first console's bookmarks here in one hop (C7). Switching to an instance
 *   without the current area opens its Overview and says why in a toast (M5). A view newer
 *   than the instance's selected capability version opens on an upgrade notice (M4).
 * - The read path (op → POST /app/op) and the write path (write → /console/write/<action>/
 *   {begin,finish}, only through ConfirmDialog). Both refuse names outside READS and WRITES.
 * - The keyboard map: o instance switcher, g then a letter for an area, / filter, ? help.
 * - Fixture mode (P14): only on a page that carries data-fixtures (the harness, fixtures.html,
 *   which the Worker never serves) and only with ?fixtures=1. Every read then comes from the
 *   recorded JSON in fixtures/, writes and passkeys are simulated, and no request reaches the
 *   platform. ?state=loading|empty|error sets what views' reads answer.
 *
 * Views register a renderer with pigeonShell.register(area, view|null, render); null covers
 * every view of the area. render(ctx) returns a Node or a Promise of one; ctx is
 * { instance, record, area, view, manifest, signal, op, nav, refresh }. Return detached nodes
 * built with el(); never parse HTML or set inline styles. tests/smoke/console.rs (P15) reads
 * READS, WRITES and every register call. */
(() => {
  "use strict";
  const root = document.getElementById("pigeon-shell");
  if (!root) return;

  // ---- Contract (P15): every op the shell and its views read, and every write they make.
  const READS = Object.freeze(["console_manifest", "instances_list", "app_inbox", "app_linked_details",
    "storage_list_objects", "source_resolve_revision", "source_read_file", "ci_jobs", "ci_status",
    "observability_worker_metrics", "app_ingest_runs", "ingest_report", "digitalocean_status", "digitalocean_verify",
    "digitalocean_spaces_status", "digitalocean_spaces_verify", "digitalocean_droplet_status",
    "ai_agents_status", "ai_agents_compare", "cost_daily", "land_list",
    "site_releases", "app_site_hosts", "app_site_visitors", "capabilities_versions", "capabilities_upgrade_plan",
    // Access and Platform.
    "secrets_list", "credentials_expiry", "app_proxy_tokens", "app_people", "architecture_describe", "architecture_draw",
    "ai_agents_models", "ai_agents_config"]);
  const WRITES = Object.freeze(["approval-approve", "approval-deny", "land-approve", "run-stop", "run-note", "run-extend",
    "instance-put", "linked-unlink", "site-deploy", "site-rollback", "visitor-invite", "visitor-revoke",
    // Access and Platform.
    "secret-set", "apple-adopt", "apple-identity", "token-issue", "token-revoke", "invite", "passkey-remove", "person-admin", "member-set", "user-remove",
    "app-disconnect", "model-defaults", "agents-config", "digitalocean-token-set", "digitalocean-spaces-key-set", "digitalocean-droplet-token-set", "digitalocean-droplet-start", "digitalocean-droplet-cancel"]);

  // ---- Fixture mode (P14) and routing base.
  const query = new URLSearchParams(location.search);
  const FIXTURES = root.dataset.fixtures && query.get("fixtures") === "1" ? root.dataset.fixtures : null;
  const fixtureState = FIXTURES ? query.get("state") || "loaded" : null;
  const base = (root.dataset.base ?? "/app").replace(/\/+$/, "");
  const site = root.dataset.site || "pigeon";
  let person = FIXTURES ? "Fixture Person" : root.dataset.person;

  // ---- Elements: text only, never HTML.
  const TAGS = new Set("a aside b button code dd details div dl dt em footer form h1 h2 h3 header hr img input kbd label li main nav ol option p picture pre section select small source span strong summary table tbody td textarea th thead time tr ul".split(" "));
  const ATTRS = /^(?:aria-[\w-]+|data-[\w-]+|id|class|role|tabindex|title|href|src|alt|type|name|value|placeholder|disabled|hidden|spellcheck|selected|checked|for|autocomplete|rows|colspan|scope|datetime|min|max|maxlength|step|media|srcset)$/;
  function el(tag, attrs = {}, ...children) {
    if (!TAGS.has(tag)) throw new TypeError(`Unsupported tag: ${tag}`);
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (value == null || value === false) continue;
      if (key === "text") node.textContent = value;
      else if (key === "on") for (const [type, fn] of Object.entries(value)) node.addEventListener(type, fn);
      else if (!ATTRS.test(key)) throw new TypeError(`Unsupported attribute: ${key}`);
      else if (key === "src" || key === "srcset") {
        // Only an image the platform drew, as a data URL: an SVG in an img runs no script.
        if (tag !== (key === "src" ? "img" : "source") || !/^data:image\/svg\+xml;base64,[A-Za-z0-9+/=]+$/.test(value)) throw new TypeError("Unsupported image");
        node.setAttribute(key, value);
      } else if (key === "href") {
        const target = new URL(value, location.href);
        if (target.origin !== location.origin && target.protocol !== "https:") throw new TypeError("Unsupported URL");
        node.setAttribute(key, value);
      } else node.setAttribute(key, value === true ? "" : String(value));
    }
    for (const child of children.flat(Infinity)) if (child != null && child !== false) node.append(child);
    return node;
  }
  const ICONS = {
    inbox: "M2 9h3l1 2h4l1-2h3M3.5 3h9L14 9v4H2V9z",
    overview: "M2 2h5v5H2zM9 2h5v5H9zM2 9h5v5H2zM9 9h5v5H9z",
    site: "M2 8h12M8 2c2.2 2 2.2 10 0 12M8 2c-2.2 2-2.2 10 0 12M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0z",
    storage: "M2.5 4v8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2V4M2.5 8c0 1.1 2.5 2 5.5 2s5.5-.9 5.5-2M13.5 4c0 1.1-2.5 2-5.5 2s-5.5-.9-5.5-2 2.5-2 5.5-2 5.5.9 5.5 2z",
    source: "M4.5 5v6M11.5 7c0 2.5-3 3-6.5 4M6 3.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0zM6 12.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0zM13 5.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0z",
    ci: "M5.5 8.2l1.8 1.8 3.2-4M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0z",
    metrics: "M2 12l4-5 3 3 5-6",
    ingest: "M8 2v8M5 7l3 3 3-3M3 13h10",
    capabilities: "M6 2h4v3h3v4h-3v5H6V9H3V5h3z",
    sessions: "M2 4h12M2 8h8M2 12h10",
    cost: "M2 13V9M6 13V5M10 13V7M14 13V3",
    instances: "M2 2.5h12v4H2zM2 9.5h12v4H2z",
    access: "M8.5 8H14M12 8v2.5M8.5 8a3 3 0 1 1-6 0 3 3 0 0 1 6 0z",
    platform: "M8 2l6 3-6 3-6-3zM2 8l6 3 6-3M2 11l6 3 6-3",
    key: "M7.5 8H14M11.5 8v2M13.5 8v1.5M7.5 8a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z",
    chev: "M6 3l5 5-5 5",
    copy: "M5 5h9v9H5zM11 5V2H2v9h3",
    dot: "M8 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4z",
    updown: "M5 6l3-3 3 3M5 10l3 3 3-3",
    system: "M2 3h12v8H2zM6 14h4M8 11v3",
    sun: "M8 5.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5zM8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1",
    moon: "M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5z",
    keyboard: "M1.5 4h13v8h-13zM4 7h1M7 7h2M11 7h1M5 9.5h6",
    signout: "M6 2.5H3v11h3M10.5 5L13.5 8l-3 3M13.5 8H6",
  };
  const SVG = "http://www.w3.org/2000/svg";
  function icon(name) {
    const svg = document.createElementNS(SVG, "svg");
    for (const [k, v] of Object.entries({ viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", "stroke-width": "1.5",
      "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true", class: "icon" })) svg.setAttribute(k, v);
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", ICONS[name] || ICONS.dot);
    svg.append(path);
    return svg;
  }

  // ---- Components (P13).
  const TONES = new Set(["ok", "warn", "bad", "info", "accent", "plain"]);
  const pill = (text, tone) => el("span", { class: TONES.has(tone) ? `pill ${tone}` : "pill", text });
  const tag = (text) => el("span", { class: "tag", text });
  const code = (text) => el("code", { text: String(text ?? "") });
  const VARIANTS = { primary: "btn-primary", danger: "btn-danger", ghost: "btn-ghost", "danger-primary": "btn-danger btn-primary" };
  function button(label, onClick, { variant, size, icon: name, ...attrs } = {}) {
    const cls = ["btn", VARIANTS[variant], size === "sm" && "btn-sm"].filter(Boolean).join(" ");
    const node = el("button", { type: "button", class: cls, ...attrs }, name && icon(name), label);
    if (onClick) node.addEventListener("click", onClick);
    return node;
  }
  function facts(pairs) {
    return el("dl", { class: "facts" }, pairs.filter(Boolean).flatMap(([k, v]) =>
      [el("dt", { text: k }), el("dd", {}, v instanceof Node || Array.isArray(v) ? v : String(v ?? "—"))]));
  }
  function segmented({ label, options, value, onChange }) {
    const group = el("span", { class: "seg", role: "group", "aria-label": label });
    const set = (id) => { for (const b of group.children) b.setAttribute("aria-pressed", String(b.dataset.id === id)); };
    for (const o of options) {
      group.append(el("button", { type: "button", "data-id": o.id, "aria-pressed": String(o.id === value), text: o.title,
        on: { click: () => { set(o.id); onChange?.(o.id); } } }));
    }
    return group;
  }
  const input = (attrs = {}) => el("input", { class: attrs.mono ? "input mono" : "input", ...attrs, mono: null });
  function field({ label, hint, control }) {
    return el("label", { class: "field" }, el("span", { text: label }), control, hint && el("small", { text: hint }));
  }
  const panel = (title, ...children) => el("section", { class: "panel" }, title && el("h2", { text: title }), ...children);
  const strip = (tone, ...children) => el("div", { class: tone ? `strip ${tone}` : "strip" }, ...children);
  // Bar: an inline bar of stacked parts, each { value: a fraction of the track, tone }, drawn as
  // SVG rects sized by attribute (no inline style). Meter: a bar and its figure, such as spend
  // against a budget; at 80% or more it turns hot.
  const BAR_TONES = new Set(["ok", "warn", "bad", "info", "muted", "hot"]);
  function bar(parts, { label } = {}) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", "bar");
    svg.setAttribute("preserveAspectRatio", "none");
    if (label) { svg.setAttribute("role", "img"); svg.setAttribute("aria-label", label); } else svg.setAttribute("aria-hidden", "true");
    let at = 0;
    for (const { value, tone } of parts) {
      const w = Math.min(Math.max(Number(value) || 0, 0), 1 - at);
      if (w <= 0) continue;
      const rect = document.createElementNS(SVG, "rect");
      for (const [k, v] of Object.entries({ x: `${at * 100}%`, y: "0", width: `${w * 100}%`, height: "100%",
        class: BAR_TONES.has(tone) ? `fill ${tone}` : "fill" })) rect.setAttribute(k, v);
      svg.append(rect);
      at += w;
    }
    return svg;
  }
  function meter(value, max, text, { tone } = {}) {
    const f = max > 0 && Number.isFinite(value) ? value / max : null;
    return el("span", { class: "meter" }, f === null ? el("span", { class: "bar-none" }) : bar([{ value: f, tone: tone || (f >= 0.8 ? "hot" : null) }]),
      el("span", { class: "num" }, text));
  }
  function diffBlock(lines) {
    return el("div", { class: "diff changes" }, lines.map((l) =>
      el("span", { class: l.op === "add" ? "add" : l.op === "del" ? "del" : "note", text: `${l.op === "add" ? "+ " : l.op === "del" ? "- " : ""}${l.text}` })));
  }

  // Toast: one at a time, polite.
  let toastNode, toastTimer;
  function toast(message, tone = "ok") {
    clearTimeout(toastTimer);
    if (!toastNode) {
      toastNode = el("div", { class: "toast", role: "status", "aria-live": "polite" });
      document.body.append(toastNode);
    }
    toastNode.className = tone === "ok" ? "toast" : `toast ${tone}`;
    toastNode.textContent = message;
    toastNode.hidden = false;
    toastTimer = setTimeout(() => { toastNode.hidden = true; }, 4500);
  }

  // Dialog: the one modal. Focus moves in and comes back; Tab stays inside; Esc closes.
  let openDialog = null;
  function dialog({ title, body, actions = [], onClose }) {
    openDialog?.close();
    const last = document.activeElement;
    const status = el("span", { class: "status", role: "status" });
    const closeBtn = button("Close", () => close(), { variant: "ghost", size: "sm" });
    const content = el("div", { class: "modal-body" }, body);
    const footer = el("footer", {}, status, ...actions);
    const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;
    const modal = el("div", { class: "modal", role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
      el("header", {}, el("h2", { id: titleId, text: title }), closeBtn), content, footer);
    const scrim = el("div", { class: "scrim" }, modal);
    let busy = false;
    scrim.addEventListener("mousedown", (e) => { if (e.target === scrim && !busy) close(); });
    modal.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !busy) { e.stopPropagation(); close(); }
      if (e.key !== "Tab") return;
      const items = [...modal.querySelectorAll("button:not(:disabled), input, textarea, select, a[href], [tabindex='0']")];
      if (!items.length) return;
      const [first, end] = [items[0], items[items.length - 1]];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); end.focus(); }
      else if (!e.shiftKey && document.activeElement === end) { e.preventDefault(); first.focus(); }
    });
    function close() {
      if (!scrim.isConnected) return;
      scrim.remove();
      openDialog = null;
      onClose?.();
      if (last?.isConnected) last.focus();
    }
    const handle = {
      close, modal, content, footer,
      say(text, bad = false) { status.textContent = text; status.className = bad ? "status error" : "status"; },
      set busy(v) { busy = v; closeBtn.disabled = v; },
      get busy() { return busy; },
    };
    openDialog = handle;
    document.body.append(scrim);
    (content.querySelector("input, textarea, select") || actions[actions.length - 1] || closeBtn).focus();
    return handle;
  }

  // ShowOnce (P3): one component for proxy tokens, person invites and visitor invites.
  function copyField(value) {
    const shown = el("code", { text: value });
    const copy = button("Copy", async () => {
      try {
        await navigator.clipboard.writeText(value);
        copy.lastChild.textContent = "Copied";
        setTimeout(() => { copy.lastChild.textContent = "Copy"; }, 1500);
      } catch {
        const range = document.createRange();
        range.selectNodeContents(shown);
        getSelection().removeAllRanges();
        getSelection().addRange(range);
        toast("Selected. Press Ctrl+C or ⌘C to copy.", "info");
      }
    }, { icon: "copy" });
    return el("div", { class: "copyfield" }, shown, copy);
  }
  const SHOW_ONCE = {
    token: "Shown once. After Done it can’t be shown again; issue a new token if it’s lost.",
    invite: "Shown once. The link works once; make a new invite if it’s lost.",
  };
  function showOnceBody({ note, value, kind = "token" }) {
    return el("div", { class: "section" }, note && el("p", { text: note }), copyField(value),
      el("p", { class: "muted", text: SHOW_ONCE[kind] || SHOW_ONCE.token }));
  }
  function showOnce({ title, note, value, kind }) {
    const done = button("Done", () => d.close(), { variant: "primary" });
    const d = dialog({ title, body: showOnceBody({ note, value, kind }), actions: [done] });
    return d;
  }

  // ConfirmDialog (P2, P4): names the target and the exact change, then asks for the passkey in
  // place through the one write path. `danger` makes the confirm button the danger variant.
  // A write that answers { reveal: {value, note, kind} } turns the dialog into ShowOnce.
  // `form` adds fields to the dialog; `prepare()` reads them when the person confirms and
  // answers { id, change, begin } for this write (`begin`: the change sent to begin, such as a
  // secret's digest, when it differs), or throws an Error whose message the dialog shows.
  function confirm({ title, target = [], diff, effect, form, prepare, action, id, change, danger = false, confirmLabel, onDone }) {
    if (!WRITES.includes(action)) throw new Error(`Unknown write: ${action}`);
    const summary = el("div", { class: "section", hidden: true });
    const body = [
      target.length ? el("div", { class: "changes" }, facts(target)) : null,
      diff?.length ? el("div", { class: "section" }, el("div", { class: "label", text: "Change" }), diffBlock(diff)) : null,
      form || null,
      effect ? el("p", { class: "ink2", text: effect }) : null,
      summary,
    ];
    const cancel = button("Cancel", () => d.close());
    const ok = button(confirmLabel || "Confirm with passkey", run, { variant: danger ? "danger-primary" : "primary", icon: "key" });
    const d = dialog({ title, body, actions: [cancel, ok] });
    async function run() {
      let ready = { id, change };
      if (prepare) {
        try {
          ready = { id, change, ...(await prepare()) };
        } catch (err) {
          return d.say(err.message || String(err), true);
        }
      }
      ok.disabled = cancel.disabled = true;
      d.busy = true;
      d.say("Preparing…");
      try {
        const done = await write(action, { target: ready.id, change: ready.change, begin: ready.begin }, {
          onSummary(s) {
            const node = summaryNode(s);
            if (node) summary.replaceChildren(el("div", { class: "label", text: "The platform will make this change" }), node);
            summary.hidden = !node;
            d.say("Waiting for your passkey…");
          },
        });
        d.busy = false;
        onDone?.(done);
        if (done?.reveal?.value) {
          d.content.replaceChildren(showOnceBody(done.reveal));
          cancel.remove();
          ok.replaceWith(button("Done", () => d.close(), { variant: "primary" }));
          d.footer.lastChild.focus();
          d.say(done.message || "Done.");
        } else {
          d.close();
          toast(done?.message || "Done.");
        }
      } catch (err) {
        d.busy = false;
        ok.disabled = cancel.disabled = false;
        d.say(err.message || String(err), true);
      }
    }
    return d;
  }
  // The begin step's change summary, however the write states it.
  function summaryNode(s) {
    if (s == null) return null;
    if (typeof s === "string") return el("p", { text: s });
    // The write path answers { action, title, target, change: [lines], destructive }.
    const changes = Array.isArray(s) ? s : Array.isArray(s.change) ? s.change : Array.isArray(s.changes) ? s.changes : null;
    if (changes) {
      const show = (v) => (v === undefined || v === null ? "none" : JSON.stringify(v));
      return diffBlock(changes.flatMap((c) => typeof c === "string" ? [{ op: "note", text: c }] : [
        c.from !== undefined && c.from !== null && { op: "del", text: `${c.path}: ${show(c.from)}` },
        c.to !== undefined && c.to !== null && { op: "add", text: `${c.path}: ${show(c.to)}` },
      ].filter(Boolean)));
    }
    return el("pre", { class: "code", text: JSON.stringify(s, null, 2) });
  }

  // DataTable: every list. Loading skeleton, empty, error with Retry, group rows ({group}),
  // child rows (row.children), expandable rows (expand), and an optional filter.
  function dataTable({ columns, load, label, noun = "this list", empty = "Nothing here.", expand, filter, signal }) {
    const cols = expand ? [{ label: "", cls: "chev", cell: () => icon("chev") }, ...columns] : columns;
    const tbody = el("tbody");
    const table = el("table", { class: "tbl" },
      el("thead", {}, el("tr", {}, cols.map((c) => el("th", { scope: "col", class: c.cls || null }, c.label || "")))), tbody);
    const wrap = el("div", { class: "tbl-wrap", role: label ? "region" : null, tabindex: label ? "0" : null, "aria-label": label || null }, table);
    const count = el("span", { class: "muted" });
    const search = filter ? el("input", { class: "input", type: "search", "data-filter": "", placeholder: filter, "aria-label": filter }) : null;
    const node = el("div", { class: "section" }, filter ? el("div", { class: "toolbar" }, search, el("span", { class: "grow" }), count) : null, wrap);
    const n = cols.length;
    const full = (cls, ...kids) => el("tr", { class: cls }, el("td", { colspan: n }, ...kids));
    const cell = (c, row) => {
      const v = c.cell ? c.cell(row) : row[c.key];
      return el("td", { class: c.cls || null }, v instanceof Node || Array.isArray(v) ? v : String(v ?? "—"));
    };
    function rowsFor(row, child) {
      if (row.group) return [full("group", row.group)];
      const tr = el("tr", { class: [child && "child", expand && "expandable"].filter(Boolean).join(" ") || null }, cols.map((c) => cell(c, row)));
      const out = [tr];
      if (expand) {
        tr.setAttribute("tabindex", "0");
        tr.setAttribute("aria-expanded", "false");
        const detail = el("tr", { class: "detail", hidden: true }, el("td", { colspan: n }));
        const toggle = () => {
          const open = tr.getAttribute("aria-expanded") !== "true";
          tr.setAttribute("aria-expanded", String(open));
          if (open && !detail.firstChild.firstChild) detail.firstChild.append(expand(row));
          detail.hidden = !open;
        };
        tr.addEventListener("click", (e) => { if (!e.target.closest("button, a, input")) toggle(); });
        tr.addEventListener("keydown", (e) => {
          if ((e.key === "Enter" || e.key === " ") && e.target === tr) { e.preventDefault(); toggle(); }
        });
        out.push(detail);
      }
      for (const kid of row.children || []) out.push(...rowsFor(kid, true));
      return out;
    }
    function show(rows) {
      const data = rows.filter((r) => !r.group);
      count.textContent = `${data.length} ${data.length === 1 ? "item" : "items"}`;
      tbody.replaceChildren(...(data.length ? rows.flatMap((r) => rowsFor(r, false)) : [full("empty", empty)]));
      applyFilter();
    }
    function applyFilter() {
      const q = search?.value.trim().toLowerCase();
      for (const tr of tbody.children) {
        if (tr.classList.contains("detail") || tr.classList.contains("group") || tr.classList.contains("empty")) continue;
        tr.hidden = Boolean(q) && !tr.textContent.toLowerCase().includes(q);
        const next = tr.nextElementSibling;
        if (next?.classList.contains("detail") && tr.hidden) next.hidden = true;
      }
    }
    search?.addEventListener("input", applyFilter);
    async function reload() {
      count.textContent = "";
      tbody.replaceChildren(...[1, 2, 3].map((k) => el("tr", {}, cols.map((_, i) =>
        el("td", {}, el("span", { class: `skel w${1 + ((k + i) % 4)}` }))))));
      tbody.setAttribute("aria-busy", "true");
      try {
        const rows = await (typeof load === "function" ? load() : load);
        if (signal?.aborted) return;
        show(Array.isArray(rows) ? rows : []);
      } catch (err) {
        if (signal?.aborted) return;
        tbody.replaceChildren(full("err", `Couldn’t load ${noun}: ${err.message || err}`,
          button("Retry", reload, { size: "sm" })));
      } finally {
        tbody.removeAttribute("aria-busy");
      }
    }
    node.reload = reload;
    reload();
    return node;
  }

  // base64url SHA-256, as the write path binds a secret's value at begin.
  async function sha256(text) {
    const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  // ---- The read path (P11): POST /app/op, or the recorded fixtures.
  async function op(name, args = {}, signal, forView = false) {
    if (!READS.includes(name)) throw new Error(`The console does not read ${name}.`);
    if (FIXTURES) return fixture(name, args, signal, forView);
    const res = await fetch("/app/op", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name, args }), signal,
    });
    if (res.status === 401 && !signal?.aborted && person !== undefined) signIn();
    const body = await res.json().catch(() => null);
    if (!res.ok || !body || !Object.hasOwn(body, "data")) {
      const err = new Error(body?.error || `The platform answered ${res.status}.`);
      err.status = res.status;
      throw err;
    }
    return body.data;
  }
  // A recorded answer: fixtures/<name>.<arg>.json for a string argument (an instance, a range),
  // else fixtures/<name>.json. Times in a recording that names _recorded_ms move with the
  // clock, so "waiting 6 min" stays true.
  async function fixture(name, args, signal, forView) {
    const wait = (ms) => new Promise((resolve, reject) => {
      const t = setTimeout(resolve, ms);
      signal?.addEventListener("abort", () => { clearTimeout(t); reject(new DOMException("Aborted", "AbortError")); });
    });
    if (forView && fixtureState === "loading") { await wait(1e9); }
    await wait(60);
    if (forView && fixtureState === "error") throw new Error("the platform didn’t answer within 10 seconds (recorded error)");
    const read = async (file) => {
      const res = await fetch(new URL(`${FIXTURES}${file}.json`, location.href), { signal });
      if (!res.ok) throw new Error(`No fixture ${file}.`);
      return res.json();
    };
    // The most specific recording first: one named by the instance, then by another string
    // argument (a range).
    let data = null;
    const named = [args?.instance, ...Object.entries(args || {}).filter(([k]) => k !== "instance").map(([, v]) => v)];
    for (const v of named) {
      if (data == null && typeof v === "string" && /^[\w-]+$/.test(v)) data = await read(`${name}.${v}`).catch(() => null);
    }
    data ??= await read(name);
    if (data && typeof data === "object" && typeof data._recorded_ms === "number") data = shiftTimes(data, Date.now() - data._recorded_ms);
    if (forView && fixtureState === "empty") data = emptied(data);
    return data;
  }
  function shiftTimes(v, by) {
    if (Array.isArray(v)) return v.map((x) => shiftTimes(x, by));
    if (!v || typeof v !== "object") return v;
    return Object.fromEntries(Object.entries(v).map(([k, x]) =>
      [k, typeof x === "number" && x > 1e12 && /(_ms|_at|_since)$/.test(k) && k !== "_recorded_ms" ? x + by : shiftTimes(x, by)]));
  }
  const emptied = (v) => Array.isArray(v) ? [] : v && typeof v === "object"
    ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, Array.isArray(x) ? [] : x])) : v;

  // ---- The write path (P10): /console/write/<action>/{begin,finish} with {target, change}.
  // begin answers the change summary and the WebAuthn challenge; finish carries the assertion
  // and makes the change once. Called only by ConfirmDialog.
  const REVEALS = new Set(["token-issue", "invite", "visitor-invite"]);
  // `begin`, when given, is the change sent to begin instead (a secret's digest for its value).
  async function write(action, { target, change, begin: beginChange }, { onSummary } = {}) {
    if (!WRITES.includes(action)) throw new Error(`Unknown write: ${action}`);
    const body = { target, change: change ?? null };
    if (FIXTURES) {
      const on = typeof target === "string" ? target : Object.values(target || {}).join(" / ");
      await new Promise((r) => setTimeout(r, 250));
      onSummary?.(`Recorded: ${action} on ${on}.`);
      await new Promise((r) => setTimeout(r, 450));
      // A write that reveals something once (P3: a token, a person or visitor invite) answers
      // its made-up recording, fixtures/write.<action>.json.
      const res = REVEALS.has(action) ? await fetch(new URL(`${FIXTURES}write.${action}.json`, location.href)).catch(() => null) : null;
      const recorded = res?.ok ? await res.json().catch(() => ({})) : {};
      return { message: `${action} on ${on}: done (fixture; nothing was sent).`, ...recorded };
    }
    const post = async (step, payload) => {
      const res = await fetch(`/console/write/${action}/${step}`, {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload),
      });
      const json = await res.json().catch(() => ({}));
      if (res.status === 401) signIn();
      if (!res.ok || json.error) throw new Error(json.error || `The platform answered ${res.status}.`);
      return json;
    };
    const begin = await post("begin", beginChange === undefined ? body : { target, change: beginChange });
    if (begin.errors) throw new Error([].concat(begin.errors).map((e) => e.message || e).join(" "));
    onSummary?.(begin.summary ?? null);
    const passkey = window.pigeonPasskey;
    if (!passkey) throw new Error("Passkeys are not available on this page.");
    const assertion = await passkey.approve(begin.options);
    return post("finish", { ...body, challenge_id: begin.challenge_id, ...assertion });
  }

  // ---- Routes. In fixture mode the route lives in the hash, so a static server can serve it.
  const STABLE = ["inbox", "overview", "site", "storage", "source", "ci", "metrics", "ingest", "capabilities",
    "sessions", "cost", "access", "instances", "platform"];
  function parseRoute() {
    const raw = FIXTURES ? location.hash.replace(/^#\/?/, "") : location.pathname.slice(base.length);
    const [instance, area, view] = raw.split("/").filter(Boolean).map((s) => { try { return decodeURIComponent(s); } catch { return ""; } });
    return { instance: instance || null, area: area || null, view: view || null };
  }
  function cleanSearch() {
    const q = new URLSearchParams(location.search);
    q.delete("instance");
    return q.size ? `?${q}` : "";
  }
  // Where the browser is, as href() spells it: the hash is part of a route only in fixture mode.
  const here = () => location.pathname + location.search + (FIXTURES ? location.hash : "");
  function href({ instance, area, view }) {
    const path = [instance, area, view].filter(Boolean).map(encodeURIComponent).join("/");
    return FIXTURES ? `${location.pathname}${cleanSearch()}#/${path}` : `${base}/${path}`;
  }

  // ---- Renderers.
  const renderers = new Map();
  function register(area, view, render) {
    const key = `${area}/${view ?? "*"}`;
    if (renderers.has(key)) throw new Error(`Duplicate renderer: ${key}`);
    renderers.set(key, render);
  }

  // ---- State.
  let instances = null;
  let instancesLoad = null;
  const manifests = new Map();
  let current = null;
  let viewLoad = null;
  let switcherOpen = false;
  let accountOpen = false;
  let app, side, main;

  const isHome = (record) => record?.account?.kind === "home";
  async function loadInstances() {
    if (instances) return instances;
    instancesLoad ??= op("instances_list", {}).then((list) => {
      instances = Array.isArray(list) ? list : [];
      return instances;
    }).catch((err) => { instancesLoad = null; throw err; });
    return instancesLoad;
  }
  async function manifestFor(id) {
    const key = id || "";
    if (!manifests.has(key)) {
      const load = op("console_manifest", id ? { instance: id } : {});
      manifests.set(key, load);
      load.catch(() => manifests.delete(key));
    }
    return manifests.get(key);
  }
  const ordered = (views) => [...(views || [])].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));

  // ---- Navigation.
  function nav(to, { replace = false } = {}) {
    const url = href(to);
    if (url !== here()) history[replace ? "replaceState" : "pushState"](null, "", url);
    render({ focus: true });
  }

  async function render({ focus = false } = {}) {
    if (person === undefined) return signIn();
    viewLoad?.abort();
    const { signal } = (viewLoad = new AbortController());
    frame();
    if (!current) main.replaceChildren(el("p", { class: "muted", role: "status", text: "Loading the console…" }));
    let list = [];
    try { list = await loadInstances(); } catch { /* The switcher shows only the current instance. */ }
    if (signal.aborted) return;
    const asked = parseRoute();
    const instanceId = asked.instance || list.find(isHome)?.id || null;
    let manifest;
    try {
      manifest = await manifestFor(instanceId);
    } catch (err) {
      if (signal.aborted) return;
      sidebar(null, null);
      return main.replaceChildren(el("div", { class: "page-head" }, el("h1", { text: site })),
        strip("bad", el("span", { class: "grow", text: `Couldn’t load the console for ${instanceId || "this platform"}: ${err.message}` }),
          button("Retry", () => { manifests.delete(instanceId || ""); render(); }, { size: "sm" })));
    }
    if (signal.aborted) return;
    const areas = manifest.areas || [];
    const fallback = areas.find((a) => a.id === (manifest.instance?.home ? "inbox" : "overview")) || areas[0];
    let area = areas.find((a) => a.id === asked.area);
    if (!area) {
      // M5: the instance has no such area, so its Overview opens and the toast says why.
      if (asked.area) {
        const overview = areas.find((a) => a.id === "overview") || fallback;
        const was = current?.manifest.areas.find((a) => a.id === asked.area)?.title || asked.area;
        toast(`${manifest.instance?.title || "This instance"} has no ${was} section, so its ${overview.title} opened instead.`, "info");
        area = overview;
      } else area = fallback;
    }
    const views = ordered(area.views);
    const view = views.find((v) => v.id === asked.view) || views[0] || null;
    const to = { instance: manifest.instance?.id || instanceId, area: area.id, view: view?.id || null };
    const canonical = href(to);
    if (canonical !== here()) history.replaceState(null, "", canonical + (FIXTURES ? "" : location.hash));
    const record = list.find((r) => r.id === to.instance) || null;
    current = { manifest, area, view, record, to };
    document.title = `${area.title} · ${manifest.instance?.title || site}`;
    sidebar(manifest, list);
    renderMain(signal, focus);
  }

  function frame() {
    if (app?.isConnected) return;
    side = el("aside", { class: "side" });
    main = el("main", { class: "main", id: "main", tabindex: "-1" });
    app = el("div", { class: "app" }, side, main);
    root.replaceChildren(el("a", { class: "skip", href: "#main", text: "Skip to content" }), app);
  }

  // ---- Sidebar.
  const initial = (title) => ([...String(title || "?")][0] || "?").toUpperCase();
  const ini = (title, home) => el("span", { class: home ? "ini home" : "ini", "aria-hidden": "true", text: initial(title) });
  function instanceText(title, host, strongTitle) {
    return el("span", { class: "sw-text" }, el(strongTitle ? "strong" : "span", { text: title }), el("small", { text: host || "—" }));
  }
  function sidebar(manifest, list) {
    const inst = manifest?.instance;
    const switcher = el("div", { class: "switcher" });
    const swButton = el("button", {
      type: "button", class: "sw-btn", "aria-haspopup": "menu", "aria-expanded": String(switcherOpen),
      "aria-label": `Instance: ${inst?.title || site}. Switch instance (o)`,
    }, ini(inst?.title || site, inst?.home), instanceText(inst?.title || site, inst?.host, true), icon("chev"));
    swButton.addEventListener("click", () => toggleSwitcher());
    switcher.append(swButton);
    if (switcherOpen) {
      const pop = el("div", { class: "sw-pop", role: "menu", "aria-label": "Instances" }, el("div", { class: "label", text: "Switch instance" }));
      const choices = list?.length ? list : inst ? [{ id: inst.id, title: inst.title, zones: [inst.host], account: { kind: inst.home ? "home" : "linked" } }] : [];
      for (const x of choices) {
        const link = el("a", { role: "menuitem", href: href({ instance: x.id, area: current?.area.id, view: current?.view?.id }),
          "aria-current": String(x.id === inst?.id) }, ini(x.title || x.id, isHome(x)), instanceText(x.title || x.id, x.zones?.[0]),
          isHome(x) ? pill("Home", "accent") : null);
        link.addEventListener("click", (e) => {
          if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
          e.preventDefault();
          switcherOpen = false;
          nav({ instance: x.id, area: current?.area.id, view: current?.view?.id });
        });
        pop.append(link);
      }
      const home = choices.find(isHome);
      if (home) {
        pop.append(el("hr"), routeLink({ instance: home.id, area: "instances", view: "list" }, el("span", { class: "sw-text" }, el("span", { text: "All instances" })), { role: "menuitem" }));
      }
      switcher.append(pop);
    }
    const navNode = el("nav", { class: "nav", "aria-label": "Console" });
    const areas = manifest?.areas || [];
    const link = (a) => {
      const count = a.id === "inbox" ? Number(manifest.inbox?.count) || 0 : 0;
      return routeLink({ instance: inst.id, area: a.id, view: ordered(a.views)[0]?.id || null },
        [icon(a.id), a.title, count ? el("span", { class: "count", "aria-label": `${count} waiting`, text: String(count) }) : null,
          a.key ? el("kbd", { text: `g ${a.key}` }) : null],
        { "aria-current": current?.area.id === a.id ? "page" : null });
    };
    navNode.append(...areas.map(link));
    side.replaceChildren(switcher, navNode, accountFoot());
  }
  const initials = (name) => String(name || "?").split(/\s+/).filter(Boolean).slice(0, 2).map((w) => initial(w)).join("");
  function routeLink(to, children, attrs = {}) {
    const a = el("a", { href: href(to), ...attrs }, children);
    a.addEventListener("click", (e) => {
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      e.preventDefault();
      switcherOpen = false;
      accountOpen = false;
      nav(to);
    });
    return a;
  }
  function toggleSwitcher(open = !switcherOpen) {
    switcherOpen = open;
    if (open) accountOpen = false;
    if (!current) return;
    sidebar(current.manifest, instances);
    const target = open ? side.querySelector(".sw-pop a[aria-current='true']") || side.querySelector(".sw-pop a") : side.querySelector(".sw-btn");
    target?.focus();
  }

  // Theme: system, light or dark; a per-viewer convenience kept in this browser only.
  const THEMES = ["system", "light", "dark"];
  function theme() { try { return localStorage.getItem("pigeon.theme") || "system"; } catch { return "system"; } }
  function applyTheme(t) {
    if (t === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = t;
  }

  // The account menu, the sidebar's footer: the theme, the shortcuts and sign out. It follows
  // the switcher's menu pattern: ↑ ↓ move between the theme, "Keyboard shortcuts" and
  // "Sign out"; ← → move within the theme's radios; Escape or a click outside closes it.
  const THEME_LABELS = { system: ["System", "system"], light: ["Light", "sun"], dark: ["Dark", "moon"] };
  function accountFoot() {
    const status = el("span", { class: "sr-only", role: "status" });
    const picked = theme();
    const radios = THEMES.map((t) => {
      const [label, glyph] = THEME_LABELS[t];
      const r = el("button", { type: "button", role: "radio", "aria-checked": String(t === picked), "aria-label": label, title: label,
        tabindex: t === picked ? "0" : "-1" }, icon(glyph));
      r.addEventListener("click", () => {
        try { localStorage.setItem("pigeon.theme", t); } catch { /* Kept for this page only. */ }
        applyTheme(t);
        for (const x of radios) {
          x.setAttribute("aria-checked", String(x === r));
          x.tabIndex = x === r ? 0 : -1;
        }
        r.focus();
      });
      return r;
    });
    const keys = el("button", { type: "button", class: "mi", role: "menuitem", tabindex: "-1", "aria-keyshortcuts": "?" },
      icon("keyboard"), "Keyboard shortcuts", el("kbd", { text: "?" }));
    keys.addEventListener("click", () => { toggleAccount(false); help(); });
    const out = el("button", { type: "button", class: "mi", role: "menuitem", tabindex: "-1" }, icon("signout"), "Sign out");
    out.addEventListener("click", () => signOut(out, status));
    const pop = el("div", { class: "acct-pop", role: "menu", "aria-label": "Account", id: "acct-menu", hidden: !accountOpen },
      el("div", { class: "who-head" }, el("strong", { text: person || "" })),
      el("hr", { class: "who-sep" }),
      el("div", { class: "theme", role: "none" }, el("span", { text: "Theme" }),
        el("div", { class: "seg", role: "radiogroup", "aria-label": "Theme" }, radios)),
      keys, el("hr"), out);
    pop.addEventListener("keydown", (e) => {
      const at = document.activeElement;
      const onRadio = radios.includes(at);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const stops = [radios.find((r) => r.tabIndex === 0) || radios[0], keys, out];
        const i = onRadio ? 0 : stops.indexOf(at);
        stops[(i + (e.key === "ArrowDown" ? 1 : -1) + stops.length) % stops.length].focus();
      } else if (onRadio && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        const next = radios[(radios.indexOf(at) + (e.key === "ArrowRight" ? 1 : -1) + radios.length) % radios.length];
        for (const x of radios) x.tabIndex = x === next ? 0 : -1;
        next.focus();
      } else if (e.key === "Escape" || e.key === "Tab") {
        toggleAccount(false);
      } else return;
      e.preventDefault();
      e.stopPropagation();
    });
    const btn = el("button", { type: "button", class: "acct-btn", "aria-haspopup": "menu", "aria-expanded": String(accountOpen),
      "aria-controls": "acct-menu", "aria-label": `Account: ${person || ""}` },
      el("span", { class: "avatar", "aria-hidden": "true", text: initials(person) }), el("span", { class: "name", text: person || "" }), icon("updown"));
    btn.addEventListener("click", () => toggleAccount());
    return el("div", { class: "side-foot acct" }, pop, btn, status);
  }
  // Opens or closes the account menu; opening it closes the switcher. Focus moves to the
  // checked theme when it opens, and back to the account button when it closes, unless `focus`
  // is false.
  function toggleAccount(open = !accountOpen, { focus = true } = {}) {
    accountOpen = open;
    if (open && switcherOpen) {
      switcherOpen = false;
      if (current) sidebar(current.manifest, instances);
    }
    const pop = side?.querySelector(".acct-pop");
    const btn = side?.querySelector(".acct-btn");
    if (!pop || !btn) return;
    pop.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
    if (!focus) return;
    (open ? pop.querySelector("[role='radio'][tabindex='0']") : btn)?.focus();
  }

  // ---- Main column.
  async function renderMain(signal, focus) {
    const { manifest, area, view } = current;
    const inst = manifest.instance || {};
    const views = ordered(area.views);
    const heading = el("h1", { tabindex: "-1", text: area.title });
    const head = el("div", { class: "page-head" },
      el("div", { class: "grow" }, el("div", { class: "crumbs", text: `${inst.title} /` }), heading));
    const outlet = el("div", { class: "view", "aria-busy": "true" });
    main.replaceChildren(head);
    if (views.length > 1) {
      main.append(el("nav", { class: "subtabs", "aria-label": `${area.title} views` }, views.map((v) =>
        routeLink({ instance: inst.id, area: area.id, view: v.id }, v.title, { "aria-current": v.id === view?.id ? "page" : null }))));
    }
    main.append(outlet);
    if (focus) heading.focus({ preventScroll: true });
    if (view?.upgrade) {
      outlet.removeAttribute("aria-busy");
      return outlet.append(upgradeNotice(inst, area, view));
    }
    const renderer = renderers.get(`${area.id}/${view?.id ?? "*"}`) || renderers.get(`${area.id}/*`);
    if (!renderer) {
      outlet.removeAttribute("aria-busy");
      return outlet.append(strip("neutral", el("span", { text: `This console has no page for ${area.title}${view ? ` › ${view.title}` : ""} yet.` })));
    }
    const ctx = {
      instance: inst, record: current.record, area: area.id, areaTitle: area.title, view, viewCount: views.length, manifest, signal,
      op: (name, args) => op(name, args, signal, true),
      nav, refresh: () => refresh(),
    };
    try {
      const node = await renderer(ctx);
      if (!signal.aborted) outlet.replaceChildren(node);
    } catch (err) {
      if (!signal.aborted) {
        outlet.replaceChildren(strip("bad", el("span", { class: "grow", text: err.message || String(err) }),
          button("Retry", () => render({ focus: true }), { size: "sm" })));
      }
    } finally {
      outlet.removeAttribute("aria-busy");
    }
  }
  // After a write: the manifest (the Inbox count, the areas) and the view read again.
  function refresh() {
    manifests.delete(current?.to.instance || "");
    manifests.delete("");
    instances = null;
    instancesLoad = null;
    render();
  }

  // UpgradeNotice (M4): the view stays in the nav; its page asks for the capability upgrade,
  // which is made in Capabilities like every capability change.
  function upgradeNotice(inst, area, view) {
    const { selected, since } = view.upgrade || {};
    const capsArea = current.manifest.areas.find((a) => a.id === "capabilities");
    return el("div", { class: "notice" },
      el("strong", { text: `${view.title} needs ${view.capability} ${since} or later.` }),
      el("p", { class: "ink2", text: `${inst.title} runs ${view.capability} ${selected}. Upgrading changes the pin on the instance record, like every capability change, and you approve it with your passkey.` }),
      capsArea ? el("div", { class: "toolbar" }, routeLink({ instance: inst.id, area: "capabilities", view: ordered(capsArea.views)[0]?.id || null },
        `Upgrade ${view.capability} in Capabilities`, { class: "btn btn-primary" })) : null);
  }

  // ---- Help overlay and keyboard map.
  function help() {
    const areas = current?.manifest.areas || [];
    const row = (keys, text) => [el("span", {}, keys.map((k, i) => [i ? " " : "", el("kbd", { text: k })])), el("span", { text })];
    const got = button("Got it", () => d.close(), { variant: "primary" });
    const d = dialog({
      title: "Keyboard shortcuts", actions: [got],
      body: [el("p", { class: "muted", text: "Area shortcuts follow the current instance’s navigation." }),
        el("div", { class: "help-grid" },
          row(["o"], "Switch instance (↑ ↓ to move, Enter to open)"),
          areas.filter((a) => a.key).map((a) => row(["g", a.key], `Go to ${a.title}`)),
          row(["/"], "Filter the list on this page"),
          row(["Enter"], "Expand the focused row"),
          row(["Esc"], "Close a dialog or menu"),
          row(["?"], "Show these shortcuts"))],
    });
  }
  let pendingG = 0;
  document.addEventListener("keydown", (e) => {
    if (openDialog) return;
    if (accountOpen && e.key === "Escape") { e.preventDefault(); toggleAccount(false); return; }
    if (!current) return;
    if (switcherOpen) {
      if (e.key === "Escape") { e.preventDefault(); toggleSwitcher(false); return; }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const items = [...side.querySelectorAll(".sw-pop a")];
        const at = items.indexOf(document.activeElement);
        items[(at + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
        e.preventDefault();
        return;
      }
    }
    if (e.target.closest?.("input, textarea, select, [contenteditable]") || e.metaKey || e.ctrlKey || e.altKey) return;
    if (Date.now() - pendingG < 1200) {
      pendingG = 0;
      const a = current.manifest.areas.find((x) => x.key === e.key);
      if (a) { e.preventDefault(); nav({ instance: current.to.instance, area: a.id, view: ordered(a.views)[0]?.id || null }); }
      return;
    }
    if (e.key === "g") { pendingG = Date.now(); return; }
    if (e.key === "?") { e.preventDefault(); help(); return; }
    if (e.key === "o") { e.preventDefault(); toggleSwitcher(); return; }
    if (e.key === "/") {
      const f = main.querySelector("[data-filter]");
      if (f) { e.preventDefault(); f.focus(); }
    }
  });
  document.addEventListener("click", (e) => {
    if (switcherOpen && !e.target.closest(".switcher")) toggleSwitcher(false);
    if (accountOpen && !e.target.closest(".acct")) {
      // Focus goes back to the account button, unless the click put it somewhere else.
      const at = document.activeElement;
      toggleAccount(false, { focus: at === document.body || Boolean(at?.closest(".acct")) });
    }
  });

  // ---- Sign in and out.
  function signIn() {
    person = undefined;
    accountOpen = false;
    current = null;
    viewLoad?.abort();
    const status = el("p", { class: "muted", role: "status" });
    const passkey = window.pigeonPasskey;
    const go = button("Sign in with passkey", async () => {
      go.disabled = true;
      status.textContent = "Waiting for your passkey…";
      try {
        const begin = await passkey.post("/console/sign-in/begin", {});
        const assertion = await passkey.approve(begin.options);
        await passkey.post("/console/sign-in/finish", { challenge_id: begin.challenge_id, ...assertion });
        location.reload();
      } catch (err) {
        status.textContent = err.message;
        go.disabled = false;
      }
    }, { variant: "primary", icon: "key" });
    app = null;
    root.replaceChildren(el("main", { class: "signin" }, el("div", { class: "box" },
      el("div", { class: "lockup" }, el("span", { class: "mark", "aria-hidden": "true" }), el("span", { text: site })),
      el("h1", { text: "Sign in" }), el("p", { class: "muted", text: "Use your passkey to open the console." }), go, status)));
  }
  async function signOut(btn, status) {
    if (FIXTURES) return toast("Signed out (fixture; nothing was sent).", "info");
    btn.disabled = true;
    try {
      await window.pigeonPasskey.post("/console/sign-out", {});
      signIn();
    } catch (err) {
      status.textContent = err.message;
      toast(err.message, "bad");
      btn.disabled = false;
    }
  }

  window.pigeonShell = Object.freeze({
    register, op: (name, args) => op(name, args), el, icon, pill, tag, code, button, facts, segmented, input, field,
    panel, strip, bar, meter, diffBlock, dataTable, toast, dialog, confirm, sha256, showOnce, copyField, href,
    get fixtures() { return Boolean(FIXTURES); },
  });

  applyTheme(theme());
  window.addEventListener("popstate", () => render({ focus: true }));
  document.addEventListener("DOMContentLoaded", () => render(), { once: true });
})();
