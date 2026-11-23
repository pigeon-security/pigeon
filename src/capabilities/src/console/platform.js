// The Cost tab's Platform section: what `observability_summary` answers. One row per Worker,
// its top errors folded away, the cost lines, and the two totals kept apart because one is an
// estimate for the window and the other is billed for the billing period. Every value goes in
// as text, never as markup.
(() => {
  const { el, money, section } = window.pigeonConsole;
  const num = (n) => Number(n || 0).toLocaleString("en-GB", { maximumFractionDigits: 0 });
  const ms = (n) => Number(n || 0).toFixed(1);
  const pct = (r) => (Number(r || 0) * 100).toFixed(2) + "%";
  const usd = (n) => (n == null ? "—" : money(n));
  const cell = (tag, text) => el(tag, { text });
  const table = (head, rows) => el("div", { class: "c-scroll" }, el("table", {},
    el("thead", {}, el("tr", {}, ...head.map((h) => cell("th", h)))),
    el("tbody", {}, ...rows)));
  const stat = (label, value) => el("article", { class: "card c-stat" },
    el("span", { class: "eyebrow", text: label }),
    el("span", { class: "c-stat-n", text: value }));
  const spiked = (w) => Array.isArray(w.spike_after_deploy) && w.spike_after_deploy.length > 0;

  section("platform", (body, data) => {
    const win = data.window || {};
    const workers = data.workers || [], cost = data.cost || [], totals = data.totals || {};
    const unavailable = data.unavailable || [];

    body.append(el("p", { class: "muted", text: `Window: ${win.from || "?"} to ${win.to || "?"} (${win.hours || "?"} hours).` }));

    body.append(el("article", { class: "card c-table", style: "gap:12px;padding:24px 28px;margin-bottom:24px" },
      el("span", { class: "eyebrow", text: "Workers" }),
      table(["Worker", "Requests", "Errors", "Error rate", "CPU p50 ms", "CPU p99 ms", "Deploys", ""],
        workers.map((w) => el("tr", {},
          cell("td", w.name), cell("td", num(w.requests)), cell("td", num(w.errors)), cell("td", pct(w.error_rate)),
          cell("td", ms(w.cpu_ms_p50)), cell("td", ms(w.cpu_ms_p99)),
          cell("td", String((w.deploys || []).length)),
          el("td", {}, spiked(w) && el("span", {
            class: "pill warn",
            title: "Errors rose after a deploy in this window",
            text: `spike after ${w.spike_after_deploy.length} deploy${w.spike_after_deploy.length === 1 ? "" : "s"}`,
          }))))),
      ...workers.filter((w) => (w.top_errors || []).length).map((w) => el("details", {},
        el("summary", { text: `${w.name}: top errors` }),
        el("ul", {}, ...w.top_errors.map((e) => el("li", { text: `${e.count} × ${e.message}` })))))));

    body.append(el("article", { class: "card c-table", style: "gap:12px;padding:24px 28px;margin-bottom:24px" },
      el("span", { class: "eyebrow", text: "Cost" }),
      table(["Line", "Kind", "USD", "Source"],
        cost.map((c) => el("tr", { title: c.detail || null },
          cell("td", c.name), cell("td", c.kind), cell("td", usd(c.usd)), cell("td", c.source))))));

    body.append(el("div", { class: "c-stats", style: "margin-bottom:24px" },
      stat("estimated, this window", usd(totals.estimated_usd)),
      stat("billed, current billing period", usd(totals.billed_usd))));

    if (unavailable.length) {
      body.append(el("div", {}, ...unavailable.map((u) => el("p", { class: "muted", text: `${u.source}: ${u.why}` }))));
    }
  });
})();
