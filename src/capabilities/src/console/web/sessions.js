(() => {
  "use strict";
  const app = window.pigeonApp;
  const { el, card, pill } = app;
  const live = ["queued", "preparing", "running", "waiting"];
  const outcomes = { done: "Done", failed: "Failed", stopped: "Stopped", over_budget: "Over budget" };
  const roles = { all: "All", planner: "Planner", executor: "Executor", mechanical: "Mechanical" };
  const extensions = [["cost_usd", 10, "+$10"], ["turns", 50, "+50 turns"], ["wall_min", 60, "+1 hour"]];
  const runLimit = 100, landingLimit = 50;
  const unavailable = "Unavailable";
  const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 6 });
  const number = (v) => Number.isFinite(v) ? v : null;
  const money = (v) => number(v) === null ? unavailable : usd.format(v);
  const word = (s) => typeof s === "string" && s ? s[0].toUpperCase() + s.slice(1).replaceAll("_", " ") : unavailable;
  const copy = (s, role) => el("p", { class: "app-muted", text: s, role });
  const heading = (s) => el("h2", { text: s });
  const line = (...nodes) => el("div", { class: "ps-line" }, nodes);
  const rows = (nodes) => el("ul", { class: "ps-rows" }, nodes.map((node) => el("li", {}, node)));
  const part = (title, ...nodes) => el("section", { class: "ps-stack" }, heading(title), nodes);
  const name = (r) => r.name || r.title || "Untitled run";
  const waiting = (r) => r.state === "waiting" && ["approval", "answer"].includes(r.waiting?.for);
  const state = (r) => pill(r.stop_asked && live.includes(r.state) ? "Stop requested" : waiting(r) ? "Waiting on you" : word(r.state),
    waiting(r) || ["over_budget", "failed", "awaiting_approval"].includes(r.state) ? "failed" : r.state);
  const ended = (r) => Object.hasOwn(outcomes, r.state);
  const amount = (r) => number(r.spent?.cost_usd);
  const pays = (route) => ({ byok: "Own key", credits: "Cloudflare credits" })[route] || unavailable;
  const model = (r) => r.model || "Model unavailable";
  const elapsed = (r) => {
    const start = number(r.started_at) ?? number(r.created_at);
    const ms = number(r.elapsed_ms) ?? (start === null ? null : (number(r.ended_at) ?? Date.now()) - start);
    if (ms === null) return "Elapsed time unavailable";
    const min = Math.max(0, Math.floor(ms / 60000));
    if (min >= 1440) return `${Math.floor(min / 1440)}d ${Math.floor(min % 1440 / 60)}h`;
    if (min >= 60) return `${Math.floor(min / 60)}h ${min % 60}m`;
    return min ? `${min}m` : "Under 1m";
  };

  function button(text, action, extra = {}) {
    const node = el("button", { type: "button", class: "app-button", text, ...extra });
    node.addEventListener("click", action);
    return node;
  }

  function link(text, subtab) {
    const route = { tab: "sessions", subtab };
    const node = el("a", { class: "app-button", href: app.url(route), text });
    node.addEventListener("click", (event) => {
      if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      app.nav(route);
    });
    return node;
  }

  function list(value, label, valid) {
    if (!Array.isArray(value) || !value.every((r) => r && valid(r))) throw new Error(`${label} response could not be read.`);
    return value;
  }

  // A failure in one read must not hide the other sections or pass as an empty list.
  async function read(load) {
    try { return { data: await load() }; }
    catch (err) { return { error: err.message || String(err) }; }
  }

  const errorCard = (result) => card(copy(`Unavailable: ${result.error}`, "alert"));
  const bar = (segments) => el("div", { class: "ps-bar", "aria-hidden": "true" }, segments.map(([fraction, tone]) => {
    const node = el("span", { class: `ps-fill ps-fill-${tone}` });
    node.style.width = `${Math.min(1, Math.max(0, fraction)) * 100}%`;
    return node;
  }));

  function runRow(r, finished = false) {
    const problem = ["failed", "over_budget"].includes(r.state);
    return el("div", { class: "ps-stack ps-compact" },
      line(el("strong", { text: name(r) }), state(r), el("span", { class: "ps-value", text: money(amount(r)) })),
      finished ? copy(model(r)) : el("code", { class: "app-muted", text: r.run }),
      problem ? copy(r.report || r.step || "Failure reason unavailable.") : null);
  }

  function actions(r, write) {
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(r.run) || r.stop_asked || !live.includes(r.state)) return null;
    const gateway = r.runtime === "gateway";
    const panel = el("div", { class: "ps-stack" });
    const triggers = [];
    const close = (trigger) => {
      panel.replaceChildren();
      triggers.forEach((b) => b.setAttribute("aria-expanded", "false"));
      trigger.focus();
    };
    const open = (label, build) => {
      const trigger = button(label, () => {
        if (trigger.getAttribute("aria-expanded") === "true") return close(trigger);
        triggers.forEach((b) => b.setAttribute("aria-expanded", String(b === trigger)));
        const [body, first] = build(trigger);
        panel.replaceChildren(body);
        first.focus();
      }, { "aria-expanded": "false" });
      triggers.push(trigger);
    };
    const form = (trigger, body, submitLabel, submit, first) => {
      const submitButton = el("button", { class: "app-button", type: "submit", text: submitLabel });
      const status = copy("", "status");
      const node = el("form", { class: "ps-form ps-stack" }, body,
        line(submitButton, button("Cancel", () => close(trigger))), status);
      node.addEventListener("submit", (event) => {
        event.preventDefault();
        submit(status);
      });
      return [node, first || submitButton];
    };
    if (gateway) {
      open("Note", (trigger) => {
        const input = el("textarea", { rows: 3 });
        return form(trigger, el("label", {}, "Note for the next turn", input), "Confirm send", (status) => {
          const text = input.value.trim();
          if (!text || new TextEncoder().encode(text).length > 4000) {
            status.textContent = "Enter a note of 1 to 4,000 UTF-8 bytes.";
            input.focus();
            return;
          }
          write("ai_agents_note", { run: r.run, text }, "Note queued for the next turn.");
        }, input);
      });
      open("Extend", (trigger) => {
        const choices = extensions.map(() => el("input", { type: "checkbox" }));
        return form(trigger, el("fieldset", {}, el("legend", { text: "Add to this run’s budget" }),
          extensions.map(([, , label], i) => el("label", {}, choices[i], ` ${label}`))), "Confirm extension", (status) => {
          const selected = extensions.filter((_, i) => choices[i].checked);
          if (!selected.length) {
            status.textContent = "Choose at least one extension.";
            choices[0].focus();
            return;
          }
          write("ai_agents_extend", { run: r.run, ...Object.fromEntries(selected.map(([k, v]) => [k, v])) }, "Budget extended.");
        }, choices[0]);
      });
    }
    open("Stop", (trigger) => form(trigger,
      copy(r.role === "planner" ? "Stop this run and its live executors? Branches are kept." : "Stop this run? Its branch is kept."),
      "Confirm stop", () => write("ai_agents_stop", { run: r.run }, "Stop requested.")));
    return el("div", { class: "ps-actions ps-stack" }, line(triggers), panel);
  }

  function runCard(r, children, incomplete, write) {
    const own = amount(r);
    const child = number(r.spent?.executors_spent_usd);
    const spent = own !== null && (r.role !== "planner" || child !== null) ? own + (child ?? 0) : null;
    const budget = number(r.budget?.budget_cost_usd);
    const reserved = number(r.spent?.executors_reserved_usd);
    const count = children.filter(ended).length;
    return card(line(el("h3", { text: name(r) }), el("span", { class: "ps-value", text: elapsed(r) })),
      line(state(r), copy([word(r.role), model(r), r.kind && word(r.kind)].filter(Boolean).join(" · "))),
      r.step ? copy(r.step) : null,
      spent !== null && budget > 0 ? bar([[spent / budget, spent / budget >= .8 ? "failed" : "cost"]]) : null,
      copy(`Spent: ${money(spent)} · Budget: ${money(budget)}${reserved > 0 ? ` · Reserved: ${money(reserved)}` : ""}`),
      r.role === "planner" ? copy(`Executors: ${count} of ${children.length} listed finished${incomplete ? " · history may be incomplete" : ""}`) : null,
      children.length ? rows(children.map((childRun) => runRow(childRun))) : null,
      actions(r, write));
  }

  function statistic(label, value, detail) {
    return el("div", { class: "ps-stack ps-compact" }, el("dt", { class: "app-label", text: label }),
      el("dd", { class: "ps-stat", text: value }), detail ? el("dd", {}, copy(detail)) : null);
  }

  function sessions({ op, signal }) {
    const status = copy("", "status");
    const content = el("div", { class: "ps-stack" });
    const approvalPanel = el("div", { class: "ps-stack ps-approval" });
    const refresh = button("Refresh", () => load());
    const root = el("div", { class: "ps-view ps-stack" },
      el("header", { class: "ps-line" }, copy("Live work, landings and today’s results."), link("Compare models", "compare"), refresh),
      status, approvalPanel, content);
    let busy = false;
    let pending = false;

    function lock(on) {
      busy = on;
      refresh.disabled = on || pending;
      approvalPanel.toggleAttribute("inert", on);
      content.toggleAttribute("inert", on || pending);
      content.setAttribute("aria-busy", String(on));
    }

    function clearApproval() {
      pending = false;
      approvalPanel.replaceChildren();
    }

    async function write(tool, args, message, approval) {
      if (busy || signal.aborted || (pending && !approval)) return;
      lock(true);
      status.textContent = "Submitting request…";
      try {
        await op(tool, approval ? { ...args, approval } : args);
      } catch (err) {
        if (signal.aborted) return;
        const detail = err.message || String(err);
        // The gate returns a text error when an exact call needs approval. Reuse only
        // the ID it issued for these arguments; do not reissue the unapproved call.
        const id = /needs the person's passkey for this call: approval `([A-Za-z0-9_-]{1,128})`/.exec(detail)?.[1];
        if (id && !approval) {
          pending = true;
          const resume = button("Confirm approved request", () => write(tool, args, message, id));
          const approvalsLink = el("a", { href: "/approvals", text: "Open Approvals in another tab" });
          approvalsLink.target = "_blank";
          approvalsLink.rel = "noopener";
          approvalPanel.replaceChildren(card(heading("Passkey approval needed"),
            copy(`Approve request ${id}, then return here to confirm the same request.`),
            line(approvalsLink, resume, button("Dismiss", () => {
              clearApproval();
              lock(false);
              status.textContent = "Dismissed here; any pending approval remains in Approvals.";
              status.focus();
            }))));
          status.textContent = "Request not run. Waiting for passkey approval.";
          lock(false);
          approvalsLink.focus();
          return;
        }
        lock(false);
        status.textContent = `Request did not return success: ${detail}. Check Approvals or refresh before trying again.`;
        status.focus();
        return;
      }
      if (!signal.aborted) {
        clearApproval();
        await load(message, true);
      }
    }
    status.tabIndex = -1;

    async function load(message = "", afterWrite = false) {
      if ((busy && !afterWrite) || signal.aborted) return;
      lock(true);
      status.textContent = message ? `${message} Refreshing…` : "Loading sessions…";
      const readRuns = (onlyLive) => read(async () => {
        const data = await op("ai_agents_status", { limit: runLimit, brief: false, report: "short", ...(onlyLive ? { live: true } : {}) });
        return { paused: data?.paused, runs: list(data?.runs, "Run list", (r) => typeof r.run === "string" && typeof r.state === "string") };
      });
      const readLand = (filter) => read(async () => list(await op("land_list", { state: filter, limit: landingLimit }),
        "Landing list", (r) => typeof r.id === "string" && typeof r.state === "string"));
      const [active, history, queue, landed, cost] = await Promise.all([
        readRuns(true), readRuns(false), readLand("live"), readLand("landed"),
        read(async () => {
          const data = await op("cost_daily", { range: "today" });
          const gateway = data?.ai_gateway;
          if (!gateway || gateway.status === "unavailable" || number(gateway.total) === null) {
            throw new Error(gateway?.why || "AI Gateway spend could not be read.");
          }
          return { total: gateway.total, partial: data.truncated === true };
        }),
      ]);
      if (signal.aborted) return;
      const now = Date.now();
      const midnight = new Date(now).setUTCHours(0, 0, 0, 0);
      const today = (at) => number(at) !== null && at >= midnight && at <= now;
      const activeRows = active.data?.runs;
      const historyRows = history.data?.runs;
      const finished = historyRows?.filter((r) => ended(r) && today(r.ended_at)).sort((a, b) => b.ended_at - a.ended_at);
      const merged = new Map((historyRows || []).map((r) => [r.run, r]));
      // The live endpoint can include older runs outside the recent-history limit. If the
      // recent snapshot already shows an end, don't offer controls for that run.
      for (const r of activeRows || []) if (!ended(merged.get(r.run) || {})) merged.set(r.run, r);
      const activeIds = new Set((activeRows || []).map((r) => r.run));
      const running = [...merged.values()].filter((r) => live.includes(r.state) && (activeIds.has(r.run) || !active.error));
      const planners = new Set(running.filter((r) => r.role === "planner").map((r) => r.run));
      const groups = running.filter((r) => r.role === "planner" || !planners.has(r.parent_run)).map((r) => ({ r,
        children: r.role === "planner" ? [...merged.values()].filter((c) => c.run !== r.run && c.parent_run === r.run)
          .sort((a, b) => (a.created_at ?? 0) - (b.created_at ?? 0)) : [],
      })).sort((a, b) => Number(waiting(b.r) || b.children.some(waiting)) - Number(waiting(a.r) || a.children.some(waiting)) ||
        (b.r.created_at ?? 0) - (a.r.created_at ?? 0));
      const attention = running.filter(waiting).length + (queue.data || []).filter((r) => r.state === "awaiting_approval").length;
      const caughtUp = activeRows?.length === 0 && historyRows && !historyRows.some((r) => !ended(r)) &&
        active.data?.paused === false && queue.data?.length === 0;
      const incomplete = !historyRows || historyRows.length === runLimit || activeRows?.length === runLimit;
      const approvals = el("a", { href: "/approvals", text: "Open Approvals" });
      content.replaceChildren(
        card(heading(caughtUp ? "You’re all caught up" : attention ? "Needs your attention" : "Session activity"),
          copy(caughtUp ? "No live sessions or queued landings. Check Approvals for other requests." :
            attention ? `${attention} listed ${attention === 1 ? "item is" : "items are"} waiting on you.` :
              active.error || queue.error ? "Some activity could not be checked." : "Review your sessions and landing queue below."), approvals),
        part("Live sessions", active.data?.paused === true ? copy("New runs are paused.") : null,
          active.error ? errorCard(active) : null,
          history.error && groups.length ? copy("Recent history is unavailable; executor lists can be incomplete.") : null,
          activeRows?.length === runLimit ? copy(`Showing up to ${runLimit} live runs.`) : null,
          groups.length ? groups.map(({ r, children }) => runCard(r, children, incomplete, write)) :
            active.error ? null : card(copy("No live sessions."))),
        part("Landing queue", queue.error ? errorCard(queue) : card(queue.data.length ? rows(queue.data.map((r) =>
          line(el("code", { text: r.id }), state(r), copy(`Workers: ${Array.isArray(r.workers) ? r.workers.join(", ") || "None" : unavailable}`)))) :
          copy("No live landings."), queue.data?.length === landingLimit ? copy(`Showing up to ${landingLimit} landings.`) : null)),
        part("Today · UTC", card(el("dl", { class: "ps-stats" },
          statistic("Finished runs", finished ? `${finished.length}${historyRows.length === runLimit ? "+" : ""}` : unavailable),
          statistic("Landed", landed.data ? `${landed.data.filter((r) => today(r.updated_at)).length}${landed.data.length === landingLimit ? "+" : ""}` : unavailable, landed.error),
          statistic("Model spend", cost.data ? money(cost.data.total) : unavailable,
            cost.data ? `AI Gateway · platform-wide${cost.data.partial ? " · partial data" : ""}` : cost.error)))),
        part("Finished today", history.error ? errorCard(history) : card(finished.length ? rows(finished.map((r) => runRow(r, true))) :
          copy("No runs in recent history finished today."), historyRows?.length === runLimit ? copy(`Based on the ${runLimit} newest runs.`) : null)));
      status.textContent = message;
      lock(false);
      if (afterWrite) status.focus();
    }
    void load();
    return root;
  }

  function compareResults(groups) {
    if (!groups.length) return card(copy("No ended runs match these filters."));
    const median = card(heading("Median cost per run"), copy("Grouped by model, role and kind. Source shows who pays."));
    const endedCard = card(heading("Ended runs"));
    const medMax = groups.reduce((max, g) => Math.max(max, number(g.cost_usd?.median) ?? 0), 0);
    const runsMax = groups.reduce((max, g) => Math.max(max, number(g.runs) ?? 0), 0);
    const label = (g) => [model(g), word(g.role), g.kind && word(g.kind)].filter(Boolean).join(" · ");
    const chartRow = (title, detail, graphic) => el("div", { class: "ps-stack ps-compact" },
      el("strong", { text: title }), graphic, copy(detail));
    median.append(rows([...groups].sort((a, b) => (b.cost_usd?.median ?? -1) - (a.cost_usd?.median ?? -1)).map((g) => {
      const value = number(g.cost_usd?.median);
      return chartRow(label(g), `${pays(g.route)} · ${money(value)}`, value === null ? null : bar([[medMax ? value / medMax : 0, "cost"]]));
    })));
    endedCard.append(rows(groups.map((g) => {
      const counts = Object.entries(outcomes).map(([key, title]) => [key, title, number(g[key])]);
      return chartRow(label(g), counts.map(([, title, count]) => `${title}: ${count ?? unavailable}`).join(" · "),
        counts.every(([, , count]) => count !== null) ? bar(counts.map(([key, , count]) => [runsMax ? count / runsMax : 0, key])) : null);
    })));
    const columns = ["Model", "Role", "Kind", "Pays", "Runs", "Done", "Failed", "Median cost", "P90 cost", "Total cost", "Median turns", "Median minutes"];
    const table = el("table", {}, el("caption", { text: "Run comparison by role, model and kind" }),
      el("thead", {}, el("tr", {}, columns.map((title) => el("th", { scope: "col", text: title })))),
      el("tbody", {}, groups.map((g) => el("tr", {}, [model(g), word(g.role), g.kind ? word(g.kind) : "Not set", pays(g.route),
        number(g.runs), number(g.done), number(g.failed), money(g.cost_usd?.median), money(g.cost_usd?.high), money(g.cost_usd?.total),
        number(g.turns?.median), number(g.minutes?.median)].map((v, i) => el(i ? "td" : "th", { scope: i ? null : "row", text: v ?? unavailable }))))));
    return el("div", { class: "ps-stack" }, el("div", { class: "ps-charts" }, median, endedCard),
      card(heading("Role and model detail"), copy("Scroll the table to see all columns."),
        el("div", { class: "ps-table", role: "region", "aria-label": "Run comparison table", tabindex: "0" }, table)));
  }

  function compare({ op, signal }) {
    const role = el("select", {}, ["planner", "executor", "mechanical", "all"].map((key) => el("option", { value: key, text: roles[key] })));
    const days = el("select", {}, [1, 7, 30].map((d) => el("option", { value: d, selected: d === 7, text: `${d} ${d === 1 ? "day" : "days"}` })));
    const status = copy("", "status");
    const results = el("div", { class: "ps-stack" });
    const settings = card(heading("Planner model by kind"), copy("Loading model choices…", "status"));
    const root = el("div", { class: "ps-view ps-stack" },
      el("header", { class: "ps-line" }, heading("Compare models"), link("Back to sessions", "main")),
      card(line(el("label", {}, "Role ", role), el("label", {}, "Window ", days)),
        el("label", {}, el("input", { type: "checkbox", disabled: true, "aria-describedby": "ps-incident-help" }), " Exclude incident windows"),
        el("p", { class: "app-muted", id: "ps-incident-help", text: "Unavailable: comparison data does not include incident windows. All ended runs in the selected range are included." })),
      status, results, settings);
    let request = 0;
    async function load() {
      const current = ++request;
      const args = { days: Number(days.value), ...(role.value === "all" ? {} : { role: role.value }) };
      status.textContent = "Loading comparison…";
      results.replaceChildren();
      results.setAttribute("aria-busy", "true");
      const result = await read(async () => list((await op("ai_agents_compare", args))?.groups,
        "Model comparison", (g) => typeof g.model === "string" && typeof g.role === "string"));
      if (signal.aborted || request !== current) return;
      results.setAttribute("aria-busy", "false");
      status.textContent = result.error ? "" : `${result.data.length} comparison groups.`;
      results.replaceChildren(result.error ? card(copy(`Unavailable: ${result.error}`, "alert"), button("Retry comparison", load)) : compareResults(result.data));
    }
    async function loadModels() {
      settings.replaceChildren(heading("Planner model by kind"), copy("Loading model choices…", "status"));
      const result = await read(async () => {
        const data = await op("ai_agents_models");
        if (!data?.defaults || typeof data.defaults !== "object" || Array.isArray(data.defaults)) throw new Error("Model defaults could not be read.");
        return data;
      });
      if (signal.aborted) return;
      const defaults = result.data?.defaults;
      const kinds = ["design", "fix", "integrate", "docs"];
      settings.replaceChildren(heading("Planner model by kind"), result.error ?
        el("div", { class: "ps-stack" }, copy(`Unavailable: ${result.error}`, "alert"), button("Retry models", loadModels)) :
        el("div", { class: "ps-stack" }, rows(kinds.map((kind) => {
          const slot = defaults[`planner_${kind}`];
          const effective = slot?.source === "unset" ? defaults.planner : slot;
          return line(el("strong", { text: word(kind) }), copy(effective?.model ? `${effective.model} · ${pays(effective.route)}${slot.source === "unset" ? " · inherits planner default" : ""}` : unavailable));
        })), Array.isArray(result.data.notes) ? result.data.notes.map((note) => copy(note)) : null),
      el("button", { class: "app-button", type: "button", text: "Save · needs passkey", disabled: true, "aria-describedby": "ps-save-help" }),
      el("p", { class: "app-muted", id: "ps-save-help", text: "Read-only here. Saving requires a passkey and is not available in Sessions." }));
    }
    role.addEventListener("change", load);
    days.addEventListener("change", load);
    void load();
    void loadModels();
    return root;
  }

  app.register("sessions", {
    subtabs: { main: "Overview", compare: "Compare" },
    render: (context) => context.subtab === "compare" ? compare(context) : sessions(context),
  });
})();
