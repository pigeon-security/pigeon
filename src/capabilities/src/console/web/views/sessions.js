/* Sessions › Live and Compare, and Cost (7 days, Today, 30 days): the platform group's views,
 * on the home instance only, built on the shell's components: one DataTable per list, inline
 * bars and meters in its cells (P6), no per-view stylesheet (P13).
 * Reads (P11): ai_agents_status, land_list, ai_agents_compare, cost_daily.
 * Writes (P2, P10): run-stop, run-note, run-extend, each through the shell's confirm dialog,
 * which asks for the passkey in place; there is no round trip through another tab.
 * Sessions keeps only what is live or waits on the person (C5): no "Today" figures and no
 * "Finished today" list. Planner model by kind is read here, in Compare's Kind column (C3);
 * it is set in Platform › Models. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, code, button, facts, strip, bar, meter, segmented, dataTable, dialog, confirm, href } = shell;

  const LIVE = ["queued", "preparing", "running", "waiting"];
  const RUN_LIMIT = 100;
  const LANDING_LIMIT = 50;
  // Dollars, as $1,234.56, or to four places under a cent.
  const digits = (max) => new Intl.NumberFormat("en-US", { minimumFractionDigits: 2, maximumFractionDigits: max });
  const [cents, fine] = [digits(2), digits(4)];
  const dollars = (v, f = cents) => `${v < 0 ? "-" : ""}$${f.format(Math.abs(v))}`;
  const dayFormat = new Intl.DateTimeFormat("en-US", { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" });
  const num = (v) => (typeof v === "number" && Number.isFinite(v) ? v : null);
  const money = (v) => (num(v) === null ? "—" : dollars(v, v !== 0 && Math.abs(v) < 0.01 ? fine : cents));
  const word = (s) => (typeof s === "string" && s ? s[0].toUpperCase() + s.slice(1).replaceAll("_", " ") : "—");
  const pays = (route) => ({ byok: "Own key", credits: "Cloudflare credits", unified: "Cloudflare credits" })[route] || "—";
  const dayLabel = (day) => dayFormat.format(new Date(`${day}T00:00:00Z`));
  const sum = (xs) => xs.reduce((a, b) => a + (num(b) ?? 0), 0);
  function duration(ms) {
    if (num(ms) === null) return "—";
    const m = Math.max(0, Math.floor(ms / 60000));
    if (m >= 1440) return `${Math.floor(m / 1440)}d ${Math.floor((m % 1440) / 60)}h`;
    if (m >= 60) return `${Math.floor(m / 60)}h ${m % 60}m`;
    return m ? `${m}m` : "Under 1m";
  }
  // A time as HH:MM UTC today, else with its date.
  function at(ms) {
    if (num(ms) === null) return "—";
    const iso = new Date(ms).toISOString();
    return iso.slice(0, 10) === new Date().toISOString().slice(0, 10) ? `${iso.slice(11, 16)} UTC` : `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
  }
  // A same-page link to another area, as the shell's own links behave.
  function areaLink(ctx, to, text, cls) {
    const target = { instance: ctx.instance.id, ...to };
    return el("a", { class: cls || null, href: href(target), text, on: { click: (e) => {
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      e.preventDefault();
      ctx.nav(target);
    } } });
  }

  // ---- Sessions › Live: live runs as a tree (planners, then their executors), the landing
  // queue, and a strip that says what waits on the person.
  const runName = (r) => r.name || r.title || "Untitled run";
  const waitsOnYou = (r) => r.state === "waiting" && ["approval", "answer"].includes(r.waiting?.for);
  const isLive = (r) => LIVE.includes(r.state);
  function runState(r) {
    if (r.stop_asked && isLive(r)) return pill("Stop requested");
    if (waitsOnYou(r)) return pill("Waiting on you", "warn");
    const tone = { preparing: "info", running: "info", waiting: "info", done: "ok", failed: "bad", over_budget: "bad", stopped: "plain" }[r.state];
    return pill(word(r.state), tone);
  }
  // A planner's spend includes what its executors spent.
  function spent(r) {
    const own = num(r.spent?.cost_usd);
    if (own === null) return null;
    return r.role === "planner" ? own + (num(r.spent?.executors_spent_usd) ?? 0) : own;
  }
  function runTree(live, recent) {
    const all = new Map((recent || []).map((r) => [r.run, r]));
    for (const r of live) if (!["done", "failed", "stopped", "over_budget"].includes(all.get(r.run)?.state)) all.set(r.run, r);
    const running = [...all.values()].filter(isLive);
    const planners = new Set(running.filter((r) => r.role === "planner").map((r) => r.run));
    const newest = (a, b) => (b.created_at ?? 0) - (a.created_at ?? 0);
    return running.filter((r) => r.role === "planner" || !planners.has(r.parent_run))
      .map((r) => ({ ...r, children: r.role === "planner"
        ? [...all.values()].filter((c) => c.run !== r.run && c.parent_run === r.run).sort((a, b) => (a.created_at ?? 0) - (b.created_at ?? 0))
        : [] }))
      .sort((a, b) => Number(waitsOnYou(b) || b.children.some(waitsOnYou)) - Number(waitsOnYou(a) || a.children.some(waitsOnYou)) || newest(a, b));
  }

  const runTarget = (r) => [["Run", el("strong", { text: runName(r) })], ["Id", code(r.run)], ["Role", word(r.role)]];
  const after = (ctx) => () => setTimeout(ctx.refresh, 0);
  function stopRun(ctx, r) {
    confirm({
      title: "Stop run", target: runTarget(r), action: "run-stop", id: r.run, danger: true, confirmLabel: "Stop with passkey",
      effect: r.role === "planner" ? "Stops this run and its live executors. Branches are kept." : "Stops this run. Its branch is kept.",
      onDone: after(ctx),
    });
  }
  // Note and Extend first ask what to send in a small form, then hand it to the confirm dialog.
  function noteRun(ctx, r) {
    const text = el("textarea", { class: "input", rows: "4", maxlength: "4000", "aria-describedby": "note-hint" });
    const review = button("Review note", () => {
      const note = text.value.trim();
      if (!note || new TextEncoder().encode(note).length > 4000) {
        d.say("Write a note of 1 to 4,000 bytes.", true);
        text.focus();
        return;
      }
      confirm({
        title: "Send a note", target: runTarget(r), diff: [{ op: "add", text: note }], action: "run-note", id: r.run,
        change: { text: note }, effect: "The run reads it at its next turn.", confirmLabel: "Send with passkey", onDone: after(ctx),
      });
    }, { variant: "primary" });
    const d = dialog({
      title: "Send a note", actions: [button("Cancel", () => d.close()), review],
      body: [facts(runTarget(r)), el("label", { class: "field" }, el("span", { text: "Note for the next turn" }), text,
        el("small", { id: "note-hint", text: "1 to 4,000 bytes. The run reads it at its next turn." }))],
    });
  }
  const EXTENSIONS = [["cost_usd", 10, "+$10 of budget", (v) => `budget +${dollars(v)}`],
    ["turns", 50, "+50 turns", (v) => `turns +${v}`], ["wall_min", 60, "+1 hour of wall time", (v) => `wall time +${v} min`]];
  function extendRun(ctx, r) {
    const boxes = EXTENSIONS.map(() => el("input", { type: "checkbox" }));
    const review = button("Review extension", () => {
      const chosen = EXTENSIONS.filter((_, i) => boxes[i].checked);
      if (!chosen.length) {
        d.say("Choose at least one extension.", true);
        boxes[0].focus();
        return;
      }
      confirm({
        title: "Extend run", target: runTarget(r), diff: chosen.map(([k, v, , say]) => ({ op: "add", text: say(v) })),
        action: "run-extend", id: r.run, change: Object.fromEntries(chosen.map(([k, v]) => [k, v])),
        effect: "Raises this run’s limits now. Its spend so far is unchanged.", confirmLabel: "Extend with passkey", onDone: after(ctx),
      });
    }, { variant: "primary" });
    const budget = num(r.budget?.budget_cost_usd);
    const d = dialog({
      title: "Extend run", actions: [button("Cancel", () => d.close()), review],
      body: [facts([...runTarget(r), ["Spent", `${money(spent(r))} of ${money(budget)}`]]),
        el("div", { class: "section", role: "group", "aria-label": "Add to this run’s limits" }, el("div", { class: "label", text: "Add to this run’s limits" }),
          EXTENSIONS.map(([, , label], i) => el("label", {}, boxes[i], ` ${label}`)))],
    });
  }
  function runActions(ctx, r) {
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(r.run || "") || r.stop_asked || !isLive(r)) return [];
    const gateway = r.runtime === "gateway";
    return [
      gateway && button("Note", () => noteRun(ctx, r), { variant: "ghost", size: "sm" }),
      gateway && button("Extend", () => extendRun(ctx, r), { variant: "ghost", size: "sm" }),
      button("Stop", () => stopRun(ctx, r), { variant: "danger", size: "sm" }),
    ];
  }

  const LANDING = { awaiting_approval: ["Awaiting approval", "warn"], queued: ["Queued", null], checking: ["Checking", "info"],
    testing: ["Testing", "info"], ready: ["Ready", "info"], merging: ["Merging", "info"], merged: ["Merged", "ok"], deploying: ["Deploying", "info"] };
  const landingState = (l) => pill(...(LANDING[l.state] || [word(l.state), null]));

  function live(ctx) {
    const said = el("div", { class: "section", "aria-live": "polite" });
    const seen = { runs: null, landings: null, paused: false };
    function tell() {
      if (!seen.runs || !seen.landings) return;
      const runs = seen.runs.flatMap((r) => [r, ...r.children]).filter(waitsOnYou).length;
      const lands = seen.landings.filter((l) => l.state === "awaiting_approval").length;
      const n = runs + lands;
      const parts = [runs && `${runs} ${runs === 1 ? "run" : "runs"}`, lands && `${lands} ${lands === 1 ? "landing" : "landings"}`].filter(Boolean);
      said.replaceChildren(n
        ? strip(null, el("span", { class: "grow" }, el("strong", { text: `${n} ${n === 1 ? "request waits" : "requests wait"} on you` }),
          el("span", { class: "ink2", text: ` · ${parts.join(", ")}` })), areaLink(ctx, { area: "inbox" }, "Open Inbox", "btn btn-sm"))
        : strip("neutral", el("span", { class: "grow", text: seen.paused ? "New runs are paused. Nothing waits on you here." : "Nothing here waits on you." })));
    }
    const runsTable = dataTable({
      label: "Live runs", noun: "runs", filter: "Filter runs", signal: ctx.signal, empty: "No live runs.",
      columns: [
        { label: "Run", cls: "wide", cell: (r) => [el("strong", { text: runName(r) }), el("span", { class: "sub mono", text: r.run })] },
        { label: "State", cell: runState },
        { label: "Role · model", cell: (r) => [word(r.role), el("span", { class: "sub mono", text: r.model || "—" })] },
        { label: "Current step", cls: "ink2 wide", cell: (r) => r.step || "—" },
        { label: "Spent / budget", cell: (r) => {
          const s = spent(r);
          const b = num(r.budget?.budget_cost_usd);
          return meter(s, b, [money(s), el("span", { class: "muted", text: ` / ${money(b)}` })]);
        } },
        { label: "Elapsed", cls: "num", cell: (r) => duration(r.elapsed_ms) },
        { label: "", cls: "acts", cell: (r) => runActions(ctx, r) },
      ],
      load: async () => {
        const args = { limit: RUN_LIMIT, brief: false };
        const [now, recent] = await Promise.allSettled([ctx.op("ai_agents_status", { ...args, live: true }), ctx.op("ai_agents_status", args)]);
        if (now.status === "rejected") throw now.reason;
        const runs = Array.isArray(now.value?.runs) ? now.value.runs : [];
        seen.paused = now.value?.paused === true;
        seen.runs = runTree(runs, recent.status === "fulfilled" ? recent.value?.runs : null);
        tell();
        return seen.runs;
      },
    });
    const landingsTable = dataTable({
      label: "Landing queue", noun: "landings", signal: ctx.signal, empty: "No landings in the queue.",
      columns: [
        { label: "Landing", cell: (l) => code(l.id) },
        { label: "Branch", cls: "wide", cell: (l) => [code(l.branch || "—"), l.commit ? el("span", { class: "sub mono", text: l.commit }) : null] },
        { label: "State", cell: (l) => [landingState(l), l.state === "awaiting_approval" ? [" ", areaLink(ctx, { area: "inbox" }, "Review in Inbox")] : null] },
        { label: "Workers", cell: (l) => (Array.isArray(l.workers) && l.workers.length ? l.workers.join(", ") : "—") },
        { label: "Updated", cls: "num", cell: (l) => at(l.updated_at) },
      ],
      load: async () => {
        const list = await ctx.op("land_list", { state: "live", limit: LANDING_LIMIT });
        seen.landings = Array.isArray(list) ? list : [];
        tell();
        return seen.landings;
      },
    });
    const refresh = button("Refresh", () => {
      seen.runs = seen.landings = null;
      runsTable.reload();
      landingsTable.reload();
    }, { size: "sm" });
    return el("div", { class: "view" }, said,
      el("section", { class: "section" }, el("div", { class: "section-head" }, el("h2", { text: "Live runs" }),
        el("span", { class: "muted", text: "Planners and their executors" }), el("span", { class: "grow" }), refresh), runsTable),
      el("section", { class: "section" }, el("div", { class: "section-head" }, el("h2", { text: "Landing queue" })), landingsTable));
  }

  // ---- Sessions › Compare: ended runs by model, role and kind, as one table whose outcome and
  // cost columns carry inline bars (P6). Planner rows are by kind (C3).
  const ROLES = ["planner", "executor", "mechanical"];
  const OUTCOMES = [["done", "Done", "ok"], ["failed", "Failed", "bad"], ["over_budget", "Over budget", "warn"], ["stopped", "Stopped", "muted"]];
  function compare(ctx) {
    let role = "all";
    let days = "7";
    let top = 0;
    const roleSelect = el("select", { class: "input", id: "compare-role" },
      ["all", ...ROLES].map((r) => el("option", { value: r, selected: r === role, text: r === "all" ? "All roles" : word(r) })));
    const table = dataTable({
      label: "Model comparison", noun: "the comparison", filter: "Filter models", signal: ctx.signal, empty: "No ended runs match these filters.",
      columns: [
        { label: "Model", cell: (g) => [el("span", { class: "mono", text: g.model || "—" }), el("span", { class: "sub", text: pays(g.route) })] },
        { label: "Role", cell: (g) => word(g.role) },
        { label: "Kind", cell: (g) => (g.kind ? word(g.kind) : el("span", { class: "muted", text: "—" })) },
        { label: "Runs", cls: "num", cell: (g) => String(num(g.runs) ?? "—") },
        { label: "Outcome", cell: (g) => {
          const runs = num(g.runs) || 0;
          const said = OUTCOMES.map(([k, t]) => `${t} ${num(g[k]) ?? 0}`).join(", ");
          return [bar(OUTCOMES.map(([k, , tone]) => ({ value: runs ? (num(g[k]) ?? 0) / runs : 0, tone })), { label: said }),
            el("span", { class: "sub", text: OUTCOMES.map(([k]) => num(g[k]) ?? 0).join(" · ") })];
        } },
        { label: "Median cost", cell: (g) => meter(num(g.cost_usd?.median), top, money(g.cost_usd?.median), { tone: "accent" }) },
        { label: "P90", cls: "num", cell: (g) => money(g.cost_usd?.high) },
        { label: "Total", cls: "num", cell: (g) => money(g.cost_usd?.total) },
        { label: "Med. turns", cls: "num", cell: (g) => String(num(g.turns?.median) ?? "—") },
        { label: "Med. min", cls: "num", cell: (g) => String(num(g.minutes?.median) ?? "—") },
      ],
      load: async () => {
        const data = await ctx.op("ai_agents_compare", { days: Number(days), ...(role === "all" ? {} : { role }) });
        const groups = (Array.isArray(data?.groups) ? data.groups : []).filter((g) => g && typeof g.model === "string");
        top = Math.max(0, ...groups.map((g) => num(g.cost_usd?.median) ?? 0));
        const order = (r) => (ROLES.includes(r) ? ROLES.indexOf(r) : ROLES.length);
        groups.sort((a, b) => order(a.role) - order(b.role) || String(a.kind || "").localeCompare(String(b.kind || "")) || (b.runs ?? 0) - (a.runs ?? 0));
        if (role !== "all") return groups;
        return [...new Set(groups.map((g) => g.role))].flatMap((r) => [
          { group: { planner: "Planners, by kind", executor: "Executors", mechanical: "Mechanical runs" }[r] || word(r) }, ...groups.filter((g) => g.role === r)]);
      },
    });
    roleSelect.addEventListener("change", () => { role = roleSelect.value; table.reload(); });
    const windowSeg = segmented({ label: "Window", value: days, onChange: (id) => { days = id; table.reload(); },
      options: [{ id: "1", title: "1 day" }, { id: "7", title: "7 days" }, { id: "30", title: "30 days" }] });
    return el("div", { class: "view" },
      el("div", { class: "toolbar" }, el("label", { class: "muted", for: "compare-role", text: "Role" }), roleSelect,
        el("span", { class: "muted", text: "Window" }), windowSeg, el("span", { class: "grow" }),
        el("span", { class: "muted", text: "Ended runs only · outcome: done · failed · over budget · stopped" })),
      table,
      el("p", { class: "muted" }, "Model defaults, planner by kind among them, are set in ",
        areaLink(ctx, { area: "platform", view: "models" }, "Platform › Models"), "."));
  }

  // ---- Cost: spend over a range (the view's id: 7d, today or all), as headline figures and one
  // table: by day, by AI Gateway, by Cloudflare product, and promotional credits as meters.
  // A headline figure; a plain one (a period, not an amount) is set in body text.
  const fig = (label, value, { note, plain } = {}) => el("div", { class: "fig" }, el("span", { class: "label", text: label }),
    el(plain ? "span" : "strong", { class: plain ? "ink2" : null, text: value }), note ? el("small", { text: note }) : null);
  function cost(ctx) {
    const range = ["7d", "today", "all"].includes(ctx.view?.id) ? ctx.view.id : "7d";
    const figs = el("div", { class: "figs", "aria-live": "polite" });
    const notes = el("div", { class: "section" });
    const table = dataTable({
      label: "Spend", noun: "spend", filter: "Filter spend", signal: ctx.signal, empty: "No spend recorded in this range.",
      columns: [
        { label: "Item", cls: "wide", cell: (r) => [r.mono ? el("span", { class: "mono", text: r.item }) : el("strong", { text: r.item }), r.sub ? el("span", { class: "sub mono", text: r.sub }) : null] },
        { label: "Detail", cls: "ink2 wide", cell: (r) => r.detail || "—" },
        { label: "Spend", cls: "num", cell: (r) => money(r.spend) },
        { label: "Share", cell: (r) => r.bar },
      ],
      load: async () => {
        const data = await ctx.op("cost_daily", { range });
        const days = Array.isArray(data?.days) ? data.days.filter((d) => /^\d{4}-\d{2}-\d{2}$/.test(d)).sort() : [];
        const cf = data?.cloudflare || {};
        const ai = data?.ai_gateway || {};
        const lines = Array.isArray(cf.lines) ? cf.lines : [];
        const routes = Array.isArray(ai.by_route) ? ai.by_route : [];
        const gateways = Array.isArray(ai.by_gateway) ? ai.by_gateway : [];
        const grants = Array.isArray(data?.credits?.grants) ? data.credits.grants : [];
        const period = days.length > 1 ? `${days.length} UTC days, to ${dayLabel(days.at(-1))}` : days.length ? `${dayLabel(days[0])}, UTC` : "No days";
        figs.replaceChildren(
          fig("Net total", money(data?.net_total ?? data?.total), { note: num(data?.total) !== null && num(data?.net_total) !== null
            && data.total !== data.net_total ? `${money(data.total)} gross, less promotions` : null }),
          fig("AI Gateway", money(ai.total)), fig("Cloudflare", money(cf.total)), fig("Period", period, { plain: true, note: "Platform-wide" }));
        const gaps = (Array.isArray(data?.unavailable) ? data.unavailable : []).map((u) => `${u.source || "Some billing"} is unavailable${u.why ? ` (${u.why})` : ""}.`);
        if (data?.truncated) gaps.push("Gateway logs were truncated; model spend may be incomplete.");
        notes.replaceChildren(...(gaps.length ? [strip("neutral", el("span", { class: "grow", text: `Partial totals. ${gaps.join(" ")}` }))] : []),
          num(data?.credits?.fee_usd) ? el("p", { class: "muted", text: `Totals include a ${money(data.credits.fee_usd)} estimated Unified Billing credits fee.` }) : null);
        if (!days.length) return [];

        const rows = [];
        if (days.length > 1) {
          const aiDay = (d) => sum(routes.map((r) => r.by_day?.[d]));
          const cfDay = (d) => sum(lines.map((l) => l.by_day?.[d]));
          const most = Math.max(0, ...days.map((d) => aiDay(d) + cfDay(d)));
          rows.push({ group: "By day" }, ...[...days].reverse().map((d) => {
            const [a, c] = [aiDay(d), cfDay(d)];
            return { item: dayLabel(d), sub: d, detail: `AI Gateway ${money(a)} · Cloudflare ${money(c)}`, spend: a + c,
              bar: bar([{ value: most ? a / most : 0 }, { value: most ? c / most : 0, tone: "info" }], { label: `AI Gateway ${money(a)}, Cloudflare ${money(c)}` }) };
          }));
        }
        const share = (items, total, tone) => items.filter((x) => num(x.total)).sort((a, b) => b.total - a.total).map((x) => ({ x,
          bar: bar([{ value: total > 0 ? x.total / total : 0, tone }], { label: `${total > 0 ? Math.round((x.total / total) * 100) : 0}% of the group` }) }));
        const aiRows = share(gateways, sum(gateways.map((g) => g.total)));
        if (aiRows.length) {
          rows.push({ group: "AI Gateway · by gateway" }, ...aiRows.map(({ x, bar: b }) => ({ item: x.label || x.id, sub: x.id, detail: [pays(x.billing),
            ...(Array.isArray(x.by_model) ? x.by_model.filter((m) => num(m.total)).slice(0, 2).map((m) => m.name) : [])].join(" · "), spend: x.total, bar: b })));
        }
        const cfRows = share(lines, sum(lines.map((l) => l.total)), "info");
        if (cfRows.length) rows.push({ group: "Cloudflare · by product" }, ...cfRows.map(({ x, bar: b }) => ({ item: x.name, detail: "Billable usage", spend: x.total, bar: b })));
        if (grants.length) {
          rows.push({ group: "Promotional credits" }, ...grants.map((g) => ({ item: g.name, detail: `${money(g.remaining)} left of ${money(g.amount)} · expires ${g.expiry || "—"}`,
            spend: g.used, bar: meter(num(g.used), num(g.amount), `${num(g.amount) ? Math.round((g.used / g.amount) * 100) : 0}% used`) })));
        }
        return rows;
      },
    });
    return el("div", { class: "view" }, figs, notes, table);
  }

  register("sessions", "live", live);
  register("sessions", "compare", compare);
  register("cost", null, cost);
})();
