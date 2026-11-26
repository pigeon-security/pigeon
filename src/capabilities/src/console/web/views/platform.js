/* Platform: Architecture (the selected instance's), Models and Endpoints. Ported from
 * architecture.js, the read-only Settings › Models panel (settings.js) and Settings ›
 * Connections' endpoint list (connections.js); C1: they are shown only here. Models edits the
 * defaults (`model-defaults`) and the concurrency caps (`agents-config`) through the one write
 * path (N1). Endpoints is read-only: no write exists for it. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, code, button, segmented, field, input, strip, dataTable, confirm, copyField } = shell;

  const title = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);
  const refresh = (ctx) => () => setTimeout(ctx.refresh, 0);
  const head = (text, note) => el("div", { class: "section-head" }, el("h2", { text }), note ? el("span", { class: "muted", text: note }) : null);
  const list = (items) => (items?.length ? el("ul", { class: "plain" }, items.map((t) => el("li", {}, code(t)))) : "—");

  // ---- Architecture: counts, then the selected instance's map (SVGs the platform drew, shown
  // as images so they run nothing), its Workers or its MCP tools. An image cannot see the
  // console's theme, so the map comes drawn light and dark, each wide and, for a phone,
  // stacked; the stylesheet shows the one that matches (.arch-map in shell.css). The Workers
  // and Tools views are its text alternative.
  const dataUri = (svg) => {
    const bytes = new TextEncoder().encode(svg);
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    return `data:image/svg+xml;base64,${btoa(bin)}`;
  };
  const isSvg = (d) => typeof d?.svg === "string" && d.svg.startsWith("<svg ");

  function mapImage(drawn) {
    if (!["light", "dark"].every((t) => isSvg(drawn[t]?.wide))) return strip("neutral", el("span", { text: "The platform did not supply a diagram." }));
    const picture = (theme) => {
      const { wide, narrow } = drawn[theme];
      return el("picture", { class: `arch-${theme}` },
        isSvg(narrow) ? el("source", { media: "(max-width: 640px)", srcset: dataUri(narrow.svg) }) : null,
        el("img", { src: dataUri(wide.svg), alt: wide.title || "Architecture diagram" }));
    };
    return el("div", { class: "figure arch-map", role: "region", tabindex: "0", "aria-label": "Architecture diagram" },
      picture("light"), picture("dark"));
  }

  async function drawings(ctx) {
    const one = (theme, width) => ctx.op("architecture_draw", { instance: ctx.instance.id, mode: "full", theme, width }).catch(() => null);
    const [lw, ln, dw, dn] = await Promise.all([one("light", "wide"), one("light", "narrow"), one("dark", "wide"), one("dark", "narrow")]);
    return { light: { wide: lw, narrow: ln }, dark: { wide: dw, narrow: dn } };
  }

  async function architecture(ctx) {
    // The selected instance's own architecture: on home the platform's, as before.
    const [d, drawn] = await Promise.all([ctx.op("architecture_describe", { instance: ctx.instance.id }), drawings(ctx)]);
    const workers = d.workers || [];
    const tools = new Map();
    for (const w of workers) for (const t of new Set(w.tools || [])) tools.set(t, [...(tools.get(t) || []), w.name]);
    const toolRows = [...tools].sort(([a], [b]) => a.localeCompare(b)).map(([name, by]) => ({ name, by }));
    const stat = (n, label) => el("span", {}, el("strong", { text: String(n) }), ` ${label}`);
    const body = el("div", { class: "view" });
    const views = {
      map: () => mapImage(drawn),
      workers: () => dataTable({
        label: "Workers", noun: "Workers", filter: "Filter Workers", signal: ctx.signal, load: workers,
        columns: [
          { label: "Worker", cls: "mono", cell: (w) => [el("strong", { text: w.name }), w.lane ? el("span", { class: "sub", text: `lane ${w.lane}` }) : null] },
          { label: "MCP tools", cls: "num", cell: (w) => String(new Set(w.tools || []).size) },
          { label: "Durable Objects", cell: (w) => list((w.objects || []).map((o) => (o.owner === w.name ? o.class : `${o.class} → ${o.owner}`))) },
          { label: "Containers", cell: (w) => list(w.containers) },
          { label: "Routes", cell: (w) => list(w.routes) },
        ],
      }),
      tools: () => dataTable({
        label: "MCP tools", noun: "tools", filter: `Filter ${toolRows.length} tools`, signal: ctx.signal, load: toolRows,
        columns: [{ label: "Tool", cls: "mono", cell: (t) => t.name }, { label: "Worker", cell: (t) => list(t.by) }],
      }),
    };
    const show = (id) => body.replaceChildren(views[id]());
    show("map");
    return el("div", { class: "view" },
      el("div", { class: "toolbar" },
        el("span", { class: "stats" }, stat(workers.length, "Workers"), stat((d.parts || []).filter((p) => p.capability).length, "capabilities"),
          stat(tools.size, "MCP tools"), stat((d.hostnames || []).length, "hostnames")),
        el("span", { class: "grow" }),
        segmented({ label: "Architecture view", value: "map", onChange: show,
          options: [{ id: "map", title: "Map" }, { id: "workers", title: "Workers" }, { id: "tools", title: "Tools" }] })),
      body,
      el("p", { class: "muted mono", text: `${d.commit ? `deployed ${String(d.commit).slice(0, 12)}` : "local build"} · digest ${d.digest || "—"}` }));
  }

  // ---- Models: defaults by role and planner kind (edit: model-defaults), the concurrency caps
  // (edit: agents-config), and the providers' catalog tiers with whether their key is set.
  const ROLES = ["planner", "executor", "mechanical", "summary"];
  const KINDS = ["integrate", "docs", "fix", "design"];
  const PROVIDERS = [
    ["anthropic", "Anthropic (own key)", "pigeon-anthropic_anthropic_default"],
    ["openai", "OpenAI (own key)", "pigeon-openai_openai_default"],
    ["cf-anthropic", "Anthropic via Cloudflare", "CF_PARENT_TOKEN"],
    ["cf-openai", "OpenAI via Cloudflare", "CF_PARENT_TOKEN"],
    ["workers-ai", "Workers AI", "CF_PARENT_TOKEN"],
  ];
  const CAPS = [
    ["max_live_runs", "Live runs", "Live runs of every role at once.", 1, 64],
    ["max_executors_per_planner", "Executors per planner", "Executors one planner may run at once.", 1, 16],
    ["executor_reserve", "Executor reserve", "Slots planners leave free for executors; less than live runs.", 0, 63],
  ];

  // Every choice the catalog knows, as `provider/model` and bare provider names, current first.
  function choices(models, current) {
    const out = new Set(current ? [current] : []);
    for (const [p, t] of Object.entries(models?.tiers || {})) {
      out.add(p);
      for (const tier of ["top", "middle", "small"]) if (t[tier]) out.add(`${p}/${t[tier]}`);
      for (const extra of t.other || []) out.add(`${p}/${extra}`);
    }
    return [...out];
  }

  function editModel(ctx, models, slot, label, current) {
    const select = el("select", { class: "input mono", "aria-label": "Model" },
      choices(models, current).map((m) => el("option", { value: m, selected: m === current, text: m })));
    confirm({
      title: `Edit ${label} default`,
      target: [["Slot", code(slot)], ["Now", current ? code(current) : "the planner default"]],
      form: field({ label: "Model", control: select, hint: "A bare provider name means its model for this slot’s tier. A run can still choose its own." }),
      action: "model-defaults", id: "models", confirmLabel: "Save with passkey",
      prepare: () => {
        if (select.value === current) throw new Error("Choose a different model first.");
        return { change: { set: { [slot]: select.value } } };
      },
      onDone: refresh(ctx),
    });
  }

  function editCap(ctx, cap, config) {
    const [key, label, hint, min, max] = cap;
    const value = input({ type: "number", min: String(min), max: String(max), step: "1", value: String(config[key] ?? ""), "aria-label": label });
    confirm({
      title: `Set ${label.toLowerCase()}`,
      target: [["Limit", code(key)], ["Now", `${config[key] ?? "—"}${config.is_default?.[key] ? " (default)" : ""}`]],
      form: field({ label, control: value, hint: `${hint} ${min} to ${max}.` }),
      effect: "A run beyond a cap stays queued and starts as slots free.",
      action: "agents-config", id: "limits", confirmLabel: "Save with passkey",
      prepare: () => {
        const n = Number(value.value);
        if (!Number.isInteger(n) || n < min || n > max) throw new Error(`${label} is a whole number from ${min} to ${max}.`);
        if (n === config[key]) throw new Error("Choose a different value first.");
        return { change: { [key]: n } };
      },
      onDone: refresh(ctx),
    });
  }

  const source = (s) => (s === "stored" ? pill("Stored", "accent") : s === "built_in" ? pill("Built-in", "plain") : pill(s || "—", "plain"));

  async function models(ctx) {
    const [m, config, secrets] = await Promise.all([
      ctx.op("ai_agents_models"),
      ctx.op("ai_agents_config").catch(() => null),
      ctx.op("secrets_list").catch(() => null),
    ]);
    const defaults = m?.defaults || {};
    const planner = defaults.planner?.model;
    const roles = dataTable({
      label: "Defaults by role", noun: "model defaults", signal: ctx.signal,
      load: ROLES.filter((r) => defaults[r]).map((r) => ({ id: r, ...defaults[r] })),
      columns: [
        { label: "Role", cell: (r) => el("strong", { text: title(r.id) }) },
        { label: "Model", cls: "mono", cell: (r) => r.model || "—" },
        { label: "Reasoning", cell: (r) => r.effort || "—" },
        { label: "Pays", cell: (r) => (r.route === "byok" ? "Own key" : r.route === "credits" ? "Cloudflare credits" : "—") },
        { label: "Source", cell: (r) => source(r.source) },
        { label: "", cls: "acts", cell: (r) => button("Edit", () => editModel(ctx, m, r.id, title(r.id), r.model), { size: "sm" }) },
      ],
    });
    const kinds = dataTable({
      label: "Planner by kind", noun: "planner overrides", signal: ctx.signal,
      load: KINDS.map((k) => ({ id: k, ...(defaults[`planner_${k}`] || {}) })),
      columns: [
        { label: "Kind", cell: (k) => el("strong", { text: title(k.id) }) },
        { label: "Model", cell: (k) => (k.model ? [code(k.model), " ", pill("Override", "accent")]
          : [el("span", { class: "muted", text: "Planner default · " }), code(planner || "—")]) },
        { label: "", cls: "acts", cell: (k) => button("Edit", () => editModel(ctx, m, `planner_${k.id}`, `${title(k.id)} planner`, k.model || null), { size: "sm" }) },
      ],
    });
    const caps = config ? dataTable({
      label: "Concurrency caps", noun: "limits", signal: ctx.signal, load: CAPS,
      columns: [
        { label: "Limit", cell: (c) => [el("strong", { text: c[1] }), el("span", { class: "sub", text: c[2] })] },
        { label: "Value", cls: "num", cell: (c) => String(config[c[0]] ?? "—") },
        { label: "Source", cell: (c) => (config.is_default?.[c[0]] ? pill("Default", "plain") : pill("Stored", "accent")) },
        { label: "", cls: "acts", cell: (c) => button("Edit", () => editCap(ctx, c, config), { size: "sm" }) },
      ],
    }) : strip("neutral", el("span", { text: "The concurrency caps couldn’t be read." }));
    const set = (name) => (Array.isArray(secrets) ? secrets.find((s) => s.name === name) : undefined);
    const keyPill = (name) => {
      if (!Array.isArray(secrets)) return pill("Unknown", "plain");
      const s = set(name);
      return s?.set ? pill(name === "CF_PARENT_TOKEN" ? "Parent token set" : "Key set", "ok") : s ? pill("Key missing", "bad") : pill("No slot", "plain");
    };
    const providers = dataTable({
      label: "Providers", noun: "providers", signal: ctx.signal,
      load: PROVIDERS.filter(([id]) => m?.tiers?.[id]).map(([id, name, secret]) => ({ id, name, secret, tiers: m.tiers[id] })),
      columns: [
        { label: "Provider", cell: (p) => [el("strong", { text: p.name }), el("span", { class: "sub mono", text: p.id })] },
        { label: "Key", cell: (p) => keyPill(p.secret) },
        { label: "Top", cls: "mono", cell: (p) => p.tiers.top || "—" },
        { label: "Middle", cls: "mono", cell: (p) => p.tiers.middle || "—" },
        { label: "Small", cls: "mono", cell: (p) => p.tiers.small || "—" },
      ],
    });
    const secretsLink = el("a", { href: shell.href({ instance: ctx.instance.id, area: "access", view: "secrets" }), text: "Access › Secrets",
      on: { click: (e) => { if (e.button === 0 && !e.metaKey && !e.ctrlKey && ctx.nav) { e.preventDefault(); ctx.nav({ instance: ctx.instance.id, area: "access", view: "secrets" }); } } } });
    return el("div", { class: "view" },
      el("section", { class: "section" }, head("Defaults by role", "What a run uses when it names no model."), roles,
        (m?.notes || []).length ? strip("neutral", el("span", { text: m.notes.join(" ") })) : null),
      el("section", { class: "section" }, head("Planner by kind", "Kinds without an override use the planner default."), kinds),
      el("section", { class: "section" }, head("Concurrency caps", "How many runs may be live at once."), caps),
      el("section", { class: "section" }, el("div", { class: "section-head" }, el("h2", { text: "Providers" }),
        el("span", { class: "muted" }, "Catalog tiers, not live availability. Keys are set in ", secretsLink, ".")), providers));
  }

  // ---- Endpoints: the MCP URLs to copy into a client. The portal is mcp.<zone>; an instance's
  // endpoint is the platform's with ?instance=, exposing only its enabled capabilities' tools.
  async function endpoints(ctx) {
    const list = await ctx.op("instances_list").catch(() => []);
    const all = Array.isArray(list) ? list : [];
    const home = all.find((r) => r.account?.kind === "home");
    // Fixture mode runs on a local server, so it shows the recorded home host instead.
    const host = shell.fixtures ? home?.zones?.[0] || "platform.example.dev" : location.host;
    const labels = host.split(":")[0].split(".");
    const rows = [
      { title: "Direct platform", note: "Every tool you may call", url: `https://${host}/mcp` },
      labels.length >= 3 && { title: "Cloudflare Access portal", note: "Through the MCP server portal", url: `https://mcp.${labels.slice(1).join(".")}/mcp` },
      ...all.filter((r) => r !== home).map((r) => ({ title: r.title || r.id, note: "Instance-scoped", url: `https://${host}/mcp?instance=${encodeURIComponent(r.id)}` })),
    ].filter(Boolean);
    return el("div", { class: "view" },
      el("p", { class: "muted", text: "Copy a URL into your MCP client. The client still needs authorization. An instance-scoped endpoint exposes only the tools its enabled capabilities register." }),
      dataTable({
        label: "MCP endpoints", noun: "endpoints", signal: ctx.signal, load: rows,
        columns: [
          { label: "Endpoint", cell: (r) => [el("strong", { text: r.title }), el("span", { class: "sub", text: r.note })] },
          { label: "URL", cell: (r) => copyField(r.url) },
        ],
      }));
  }

  register("platform", "architecture", architecture);
  register("platform", "models", models);
  register("platform", "endpoints", endpoints);
})();
