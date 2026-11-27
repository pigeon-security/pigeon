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
    // A request a run asked for says which approval's run it follows.
    const follows = (i) => (i.after ? [{ op: "note", text: `asked by the run of approval ${i.after}` }] : []);
    const items = [
      ...(data.calls || []).map((c) => ({
        id: c.id, kind: "Tool call", instance: c.args?.instance || c.instance || c.summary?.instance || home, title: [el("span", { text: "Run " }), code(c.tool)],
        plain: c.tool, by: by(c.caller, c.acting_for), since: c.created_ms, expires: c.expires_ms, action: "approval-approve", deny: true,
        diff: [...follows(c), { op: "note", text: `call digest ${c.digest || "—"}` }],
        effect: c.tool === "source_token"
          ? "Approving lets this exact call run once, if its caller calls again within 10 minutes."
          : "Approving runs this exact call once, now, as its caller. The result shows under Ran on approval.",
        detail: () => el("div", { class: "cols" }, facts([
          c.run && ["Run", code(c.run)],
          ["Asked", when(c.created_ms)],
          ["Call digest", code(c.digest)],
        ]), el("pre", { class: "code", text: JSON.stringify(c.args || {}, null, 2) })),
      })),
      ...(data.drafts || []).map((d) => {
        const changes = d.summary?.changes || [];
        const diff = [...follows(d), { op: "note", text: `record version ${d.summary?.version_from ?? 0} → ${(d.summary?.version_from ?? 0) + 1}` },
          ...changes.flatMap((c) => [c.from != null && { op: "del", text: `${c.path}: ${show(c.from)}` },
            c.to != null && { op: "add", text: `${c.path}: ${show(c.to)}` }].filter(Boolean))];
        return {
          id: d.id, kind: "Instance change", instance: d.instance, title: [el("span", { text: "Change " }), el("strong", { text: d.instance })],
          plain: `change to ${d.instance}`, by: by(d.caller, d.acting_for), since: d.created_ms, expires: d.expires_ms,
          action: "approval-approve", deny: true, diff,
          effect: "Approving applies exactly these changes now, unless the instance changed since this was proposed. Then the instance is planned: a plan with something to do comes back here for your approval.",
          detail: () => diffBlock(diff),
        };
      }),
      ...(data.plans || []).map((p) => {
        const steps = (p.summary?.plan || []).map(String);
        return {
          id: p.id, kind: "Linked plan", instance: p.instance, title: [el("span", { text: "Plan for " }), el("strong", { text: p.instance })],
          plain: `plan for ${p.instance}`, by: by(p.caller, p.acting_for), since: p.created_ms, expires: p.expires_ms,
          action: "approval-approve", deny: true, diff: [...follows(p), ...steps.map((s) => ({ op: "add", text: s }))],
          effect: "Approving runs exactly these steps once, now. If the account no longer gives the same plan, nothing runs. Nothing is deleted.",
          detail: () => el("div", { class: "section" }, facts([["Account", code(p.summary?.account_id)], ["Plan digest", code(p.digest)]]),
            el("ol", { class: "steps" }, steps.map((s) => el("li", { text: s })))),
        };
      }),
      ...(data.other || []).map((o) => {
        const steps = (Array.isArray(o.summary?.plan) ? o.summary.plan : []).map(String);
        return {
          id: o.id, kind: o.kind || "Request", instance: o.instance || home, title: o.title || o.kind, plain: o.title || o.kind,
          by: by(o.caller, o.acting_for), since: o.created_ms, expires: o.expires_ms, action: "approval-approve", deny: true,
          diff: [...follows(o), ...steps.map((s) => ({ op: "add", text: s }))],
          effect: "Approving runs exactly this once, now, as the caller that asked, if it still holds.",
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
      // An approved request runs on its own: read again once it has had time to.
      onDone: () => {
        setTimeout(ctx.refresh, 0);
        if (approve) setTimeout(() => { if (!ctx.signal?.aborted) ctx.refresh(); }, 4000);
      },
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
    // One read for both tables: what waits, and what ran on approval in the last day.
    let read = null;
    const load = () => (read ||= ctx.op("app_inbox", all ? {} : { instance: ctx.instance.id }));
    const STATUS = { running: ["Running", "info"], done: ["Done", "ok"], failed: ["Failed", "bad"] };
    const ran = dataTable({
      label: "Ran on approval", noun: "runs", signal: ctx.signal,
      empty: "Nothing has run on approval in the last day.",
      columns: [
        { label: "Result", cell: (r) => pill(...(STATUS[r.status] || [r.status, "plain"])) },
        { label: "What ran", cell: (r) => [code(r.tool), el("span", { class: "sub mono", text: r.subject })] },
        all && { label: "Instance", cls: "mono", cell: (r) => r.instance || "—" },
        { label: "Outcome", cell: (r) => r.error || r.summary || (r.status === "running" ? "Running now." : "—") },
        { label: "When", cls: "num", cell: (r) => ago(r.finished_ms || r.started_ms) },
        { label: "Next", cls: "acts", cell: (r) => {
          if (!r.next) return "—";
          const next = r.nextItem;
          return next
            ? button("Approve next", () => decide(next, true), { variant: "primary", size: "sm" })
            : el("span", { class: "muted mono", text: r.next });
        } },
      ].filter(Boolean),
      expand: (r) => el("div", { class: "section" },
        facts([["Approval", code(r.id)], ["Kind", r.kind], ["Started", when(r.started_ms)], ["Finished", when(r.finished_ms)],
          r.next && ["Next approval", code(r.next)]]),
        r.error ? el("p", { class: "muted", text: r.error }) : null),
      load: async () => {
        const data = await load();
        const waiting = inboxItems(data, ctx.instance.id);
        const rows = (data.recent || []).map((r) => ({ ...r, nextItem: r.next_waits ? waiting.find((i) => i.id === r.next) : null }));
        // A run still going is read again until it ends.
        if (rows.some((r) => r.status === "running")) setTimeout(() => { if (!ctx.signal?.aborted) ctx.refresh(); }, 5000);
        return rows;
      },
    });
    return el("div", { class: "view" },
      el("p", { class: "muted", text: all ? "Every request that waits for your passkey." : `Requests for ${ctx.instance.title}.` }),
      dataTable({
        columns, label: "Pending requests", noun: "requests", filter: "Filter requests", signal: ctx.signal,
        empty: "Nothing waits for you here.",
        expand: (i) => el("div", { class: "section" }, i.detail(), el("p", { class: "muted mono", text: i.id })),
        load: async () => {
          // Home shows every request (M6); another instance asks for its own only.
          const data = await load();
          return inboxItems(data, ctx.instance.id);
        },
      }),
      el("h3", { text: "Ran on approval" }),
      el("p", { class: "muted", text: "Once you approve, the platform runs the request itself, as the caller that asked. When a run asks for the next approval, approve it here." }),
      ran);
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
