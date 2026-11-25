/* The views of the console shell at /app. Each area/view id console_manifest can send has a
 * renderer here or in views/*.js (views/data.js: Storage, Source, CI, Metrics, Ingest;
 * views/sessions.js: Sessions, Cost; views/site.js: Site; views/capabilities.js: Overview,
 * Capabilities; views/access.js: Access; views/platform.js: Platform). tests/smoke/console.rs
 * (P15) reads every register call and checks them against the manifest's ids. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, tag, code, button, facts, diffBlock, dataTable, confirm } = shell;

  const minutes = (ms) => Math.max(0, Math.round(ms / 60000));
  function ago(ms) {
    if (!ms) return "—";
    const m = minutes(Date.now() - ms);
    return m < 60 ? `${m} min` : m < 2880 ? `${Math.floor(m / 60)} h` : `${Math.floor(m / 1440)} d`;
  }
  function left(ms) {
    if (!ms) return null;
    const m = minutes(ms - Date.now());
    return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h`;
  }
  const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "—");
  const short = (s, n = 12) => String(s || "").slice(0, n);
  const branch = (b) => String(b || "").replace(/^refs\/heads\//, "");
  const show = (v) => (v === undefined || v === null ? "none" : JSON.stringify(v));

  // ---- Inbox (P5, M6): every request that waits for this person's passkey, as one table.
  // app_inbox answers approvals_list's calls, drafts, plans, other and landings, filtered to an
  // instance when it names one (landings only on home); a landing has no deny.
  function inboxItems(data, home) {
    const by = (caller, actingFor) => (actingFor ? `${caller} for ${actingFor}` : caller || "—");
    const items = [
      ...(data.calls || []).map((c) => ({
        id: c.id, kind: "Tool call", instance: c.args?.instance || c.instance || c.summary?.instance || home, title: [el("span", { text: "Run " }), code(c.tool)],
        plain: c.tool, by: by(c.caller, c.acting_for), since: c.created_ms, expires: c.expires_ms, action: "approval-approve", deny: true,
        diff: [{ op: "note", text: `call digest ${c.digest || "—"}` }],
        effect: "Approving lets this exact call run once, if its caller calls again within 10 minutes.",
        detail: () => el("div", { class: "cols" }, facts([
          c.run && ["Run", code(c.run)],
          c.grants?.permissions && ["Token permissions", c.grants.permissions.join(", ")],
          c.grants?.ttl_secs && ["Token lifetime", `${c.grants.ttl_secs} s`],
          ["Asked", when(c.created_ms)],
          ["Call digest", code(c.digest)],
        ]), el("pre", { class: "code", text: JSON.stringify(c.args || {}, null, 2) })),
      })),
      ...(data.drafts || []).map((d) => {
        const changes = d.summary?.changes || [];
        const diff = [{ op: "note", text: `record version ${d.summary?.version_from ?? 0} → ${(d.summary?.version_from ?? 0) + 1}` },
          ...changes.flatMap((c) => [c.from != null && { op: "del", text: `${c.path}: ${show(c.from)}` },
            c.to != null && { op: "add", text: `${c.path}: ${show(c.to)}` }].filter(Boolean))];
        return {
          id: d.id, kind: "Instance change", instance: d.instance, title: [el("span", { text: "Change " }), el("strong", { text: d.instance })],
          plain: `change to ${d.instance}`, by: by(d.caller, d.acting_for), since: d.created_ms, expires: d.expires_ms,
          action: "approval-approve", deny: true, diff,
          effect: "Approving applies exactly these changes now, unless the instance changed since this was proposed.",
          detail: () => diffBlock(diff),
        };
      }),
      ...(data.plans || []).map((p) => {
        const steps = (p.summary?.plan || []).map(String);
        return {
          id: p.id, kind: "Linked plan", instance: p.instance, title: [el("span", { text: "Plan for " }), el("strong", { text: p.instance })],
          plain: `plan for ${p.instance}`, by: by(p.caller, p.acting_for), since: p.created_ms, expires: p.expires_ms,
          action: "approval-approve", deny: true, diff: steps.map((s) => ({ op: "add", text: s })),
          effect: "Approving lets exactly these steps run once. If the account no longer gives the same plan, nothing runs. Nothing is deleted.",
          detail: () => el("div", { class: "section" }, facts([["Account", code(p.summary?.account_id)], ["Plan digest", code(p.digest)]]),
            el("ol", { class: "steps" }, steps.map((s) => el("li", { text: s })))),
        };
      }),
      ...(data.other || []).map((o) => {
        const steps = (Array.isArray(o.summary?.plan) ? o.summary.plan : []).map(String);
        return {
          id: o.id, kind: o.kind || "Request", instance: o.instance || home, title: o.title || o.kind, plain: o.title || o.kind,
          by: by(o.caller, o.acting_for), since: o.created_ms, expires: o.expires_ms, action: "approval-approve", deny: true,
          diff: steps.map((s) => ({ op: "add", text: s })),
          detail: () => el("div", { class: "section" }, facts([["Digest", code(o.digest)]]),
            el("ol", { class: "steps" }, steps.map((s) => el("li", { text: s })))),
        };
      }),
      ...(data.landings || []).map((l) => ({
        id: l.id, kind: "Landing", instance: home, title: [el("span", { text: "Land " }), code(branch(l.branch))],
        plain: branch(l.branch), by: l.by || "—", since: l.awaiting_since || l.created_at, expires: null,
        action: "land-approve", deny: false,
        diff: [{ op: "note", text: `commit ${short(l.commit)}` }, { op: "add", text: `land ${branch(l.branch)} on main` }],
        effect: l.needs_approval ? `Why it waits: ${l.needs_approval}` : null,
        detail: () => facts([["Why", l.needs_approval], ["Commit", code(short(l.commit))], ["Started", when(l.created_at)], l.detail && ["Detail", l.detail]]),
      })),
    ];
    // Soonest to expire first; landings, which do not expire, after them, longest waiting first.
    const end = (i) => i.expires || Infinity;
    return items.sort((a, b) => end(a) - end(b) || (a.since || 0) - (b.since || 0));
  }

  function inbox(ctx) {
    const all = Boolean(ctx.instance.home);
    const decide = (item, approve) => confirm({
      title: approve ? `Approve ${item.kind.toLowerCase()}` : `Deny ${item.kind.toLowerCase()}`,
      target: [["Request", item.plain], ["Instance", code(item.instance)], ["Requested by", item.by], ["Id", code(item.id)]],
      diff: approve ? item.diff : null,
      effect: approve ? item.effect : "The caller is told it was denied. Nothing runs.",
      action: approve ? item.action : "approval-deny", id: item.id, danger: !approve,
      confirmLabel: approve ? "Approve with passkey" : "Deny with passkey",
      onDone: () => setTimeout(ctx.refresh, 0),
    });
    const columns = [
      { label: "Kind", cell: (i) => tag(i.kind) },
      { label: "Request", cell: (i) => i.title },
      all && { label: "Instance", cls: "mono", cell: (i) => i.instance },
      { label: "Requested by", cell: (i) => i.by },
      { label: "Waiting", cls: "num", cell: (i) => ago(i.since) },
      { label: "Expires", cls: "num", cell: (i) => {
        const l = left(i.expires);
        return l ? pill(l, minutes(i.expires - Date.now()) < 10 ? "bad" : "warn") : "—";
      } },
      { label: "", cls: "acts", cell: (i) => [
        button("Approve", () => decide(i, true), { variant: "primary", size: "sm" }),
        i.deny ? button("Deny", () => decide(i, false), { size: "sm" }) : null,
      ] },
    ].filter(Boolean);
    return el("div", { class: "view" },
      el("p", { class: "muted", text: all ? "Every request that waits for your passkey." : `Requests for ${ctx.instance.title}.` }),
      dataTable({
        columns, label: "Pending requests", noun: "requests", filter: "Filter requests", signal: ctx.signal,
        empty: "Nothing waits for you here.",
        expand: (i) => el("div", { class: "section" }, i.detail(), el("p", { class: "muted mono", text: i.id })),
        load: async () => {
          // Home shows every request (M6); another instance asks for its own only.
          const data = await ctx.op("app_inbox", all ? {} : { instance: ctx.instance.id });
          return inboxItems(data, ctx.instance.id);
        },
      }));
  }

  // ---- Instances › All instances: every instance, home first, as group rows.
  function instancesList(ctx) {
    const sections = (r) => {
      const caps = Object.keys(r).filter((k) => ["publishing", "site_passkeys", "storage", "source", "ci", "observability", "ingest"].includes(k) && r[k]);
      return caps.length ? caps.map((c) => tag(c)) : el("span", { class: "muted", text: "—" });
    };
    return dataTable({
      label: "Instances", noun: "instances", filter: "Filter instances", empty: "No instances yet.", signal: ctx.signal,
      columns: [
        { label: "Instance", cell: (r) => [ctx.nav ? el("a", { href: shell.href({ instance: r.id, area: "overview" }), text: r.title || r.id,
          on: { click: (e) => { if (e.button === 0 && !e.metaKey && !e.ctrlKey) { e.preventDefault(); ctx.nav({ instance: r.id, area: "overview" }); } } } }) : r.title,
        el("span", { class: "sub mono", text: r.id })] },
        { label: "Host", cls: "mono", cell: (r) => r.zones?.[0] || "—" },
        { label: "Worker", cls: "mono", cell: (r) => r.worker || "—" },
        { label: "Capabilities", cell: sections },
        { label: "Record", cls: "num", cell: (r) => (r.version != null ? `v${r.version}` : "—") },
      ],
      load: async () => {
        const list = await ctx.op("instances_list");
        const home = list.filter((r) => r.account?.kind === "home");
        const linked = list.filter((r) => r.account?.kind !== "home");
        return [
          ...(home.length ? [{ group: "Home account" }, ...home] : []),
          ...(linked.length ? [{ group: `Linked accounts (${linked.length})` }, ...linked] : []),
        ];
      },
    });
  }

  register("inbox", null, inbox);
  register("instances", "list", instancesList);
})();
