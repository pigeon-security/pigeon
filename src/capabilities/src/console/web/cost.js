(() => {
  "use strict";
  const { register, el, card } = window.pigeonApp;
  const ranges = { "7d": "7 days", today: "Today", all: "All" };
  const products = ["CI Containers", "Artifacts", "Workers", "Workers AI", "D1", "R2"];
  const gateways = [
    { key: "anthropic", title: "Anthropic BYOK", name: "Anthropic", label: "Models via Anthropic BYOK",
      mark: "A", href: "https://console.anthropic.com/settings/cost" },
    { key: "openai", title: "OpenAI BYOK", name: "OpenAI", label: "Models via OpenAI BYOK",
      mark: "O", href: "https://platform.openai.com/usage" },
    { key: "models", title: "Cloudflare Models", name: "Cloudflare Models", label: "Models via Unified Billing Credits",
      mark: "CF", href: "https://dash.cloudflare.com/?to=/:account/ai/ai-gateway" },
  ];
  const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });
  const smallUsd = new Intl.NumberFormat("en-US", {
    style: "currency", currency: "USD", minimumFractionDigits: 2, maximumFractionDigits: 6,
  });
  const date = new Intl.DateTimeFormat("en-US", { timeZone: "UTC", month: "short", day: "numeric" });
  const money = (n) => n == null ? "Unavailable" : (n && Math.abs(n) < .01 ? smallUsd : usd).format(n || 0);
  const number = (n) => Number.isFinite(n) ? n : null;
  const map = (v) => v && typeof v === "object" && !Array.isArray(v);
  const dayLabel = (day) => date.format(new Date(`${day}T00:00:00Z`));
  const muted = (text, role) => el("p", { class: "app-muted cost-note", text, role });
  const amount = (n) => el("span", { class: `cost-amount${n == null ? " app-muted" : ""}`, text: money(n) });
  const swatch = (key) => el("span", { class: `cost-swatch cost-${key}`, "aria-hidden": "true" });

  function series(rows) {
    const daily = rows.every((row) => map(row.by_day)) ? new Map() : null;
    if (daily) for (const row of rows) for (const [day, value] of Object.entries(row.by_day)) {
      if (Number.isFinite(value)) daily.set(day, (daily.get(day) ?? 0) + value);
    }
    const total = rows.every((row) => Number.isFinite(row.total))
      ? rows.reduce((sum, row) => sum + row.total, 0) : null;
    return { total, daily };
  }

  function readCost(data, range) {
    if (data?.range !== range || !Array.isArray(data.days)) throw new Error("The cost response could not be read.");
    const days = [...new Set(data.days.filter((day) => typeof day === "string" &&
      /^\d{4}-\d{2}-\d{2}$/.test(day) && Number.isFinite(Date.parse(day))))].sort();
    const missing = new Set((data.unavailable ?? []).map((row) => row.source.toLowerCase()));
    const cf = data.cloudflare;
    const ai = data.ai_gateway;
    const cfDown = !cf || cf.status === "unavailable" || missing.has("billable usage");
    const aiDown = !ai || ai.status === "unavailable" || missing.has("ai gateway");
    const lines = !cfDown && Array.isArray(cf.lines) ? cf.lines : null;
    const rows = !aiDown && Array.isArray(ai.by_gateway) ? ai.by_gateway : null;
    const byok = { anthropic: [], openai: [] };
    const unified = [];
    let unassigned = false;
    for (const row of rows ?? []) {
      if (row.billing === "unified") unified.push(row);
      else if (row.billing === "byok" && (row.provider === "anthropic" || row.provider === "openai")) {
        byok[row.provider].push(row);
      } else if (row.id === "pigeon" && row.billing === "byok") {
        // The retired gateway can contain both payers. Use the whole row for a
        // single payer, or its disjoint model rows to split mixed spend.
        const models = row.by_model ?? [];
        const providers = new Set(models.map((model) => model.provider));
        const payer = providers.values().next().value;
        if (providers.size === 1 && Object.hasOwn(byok, payer)) byok[payer].push(row);
        else {
          for (const model of models) {
            if (Object.hasOwn(byok, model.provider)) byok[model.provider].push(model);
          }
          const assigned = models.reduce((sum, model) => sum +
            (Object.hasOwn(byok, model.provider) ? number(model.total) ?? 0 : 0), 0);
          if (Math.abs((number(row.total) ?? 0) - assigned) > .00001) unassigned = true;
        }
      }
    }
    // The Credits route already counts unified gateway usage, including older
    // rollups without per-gateway rows. Never add its provider or billing rows.
    const credits = !aiDown && ai.by_route?.find((row) => row.name === "Credits");
    const breakdown = gateways.map((g) => ({ ...g, ...(g.key === "models" && credits
      ? series([credits]) : rows ? series(g.key === "models" ? unified : byok[g.key]) : {}) }));
    const cloudflare = { key: "infra", title: "Cloudflare",
      daily: lines ? series(lines).daily : null, total: cfDown ? null : number(cf.total),
    };
    return { days, cfDown, aiDown, lines, cloudflare, breakdown, fee: number(data.credits?.fee_usd),
      anthropicEstimate: !aiDown && missing.has("anthropic"),
      gross: number(data.total), net: number(data.net_total), truncated: data.truncated,
      partial: cfDown || aiDown || missing.has("stored rollups"),
      unreconciled: unassigned || (!aiDown && Number.isFinite(ai.total) &&
        Math.abs(ai.total - breakdown.reduce((sum, g) => sum + (g.total ?? 0), 0)) > .00001),
    };
  }

  function overview(cost, series) {
    const period = cost.days.length > 1
      ? `${dayLabel(cost.days[0])} – ${dayLabel(cost.days.at(-1))}` : cost.days.map(dayLabel)[0];
    const gross = cost.gross == null ? "" : `${money(cost.gross)} gross` + (cost.net == null ? ""
      : ` − ${money(Math.round((cost.gross - cost.net) * 1e6) / 1e6)} promotions`);
    return card(
      el("div", { class: "cost-heading" }, el("h2", { text: "Net total" }),
        period && el("p", { class: "app-muted", text: period })),
      el("div", { class: "cost-net" }, amount(cost.net)), gross && muted(gross),
      cost.partial && muted("Totals exclude unavailable billing data.", "status"),
      el("dl", { class: "cost-metrics" }, series.map((item) => el("div", {},
        el("dt", {}, swatch(item.key), item.title), el("dd", {}, amount(item.total))))));
  }

  function chart(days, series, partial, selectDay) {
    const node = card(el("h2", { text: "Spend by day" }));
    const bars = days.map((day) => {
      const amounts = series.map((s) => s.daily ? s.daily.get(day) ?? 0 : null);
      const positive = amounts.reduce((sum, n) => sum + Math.max(0, n ?? 0), 0);
      const negative = amounts.reduce((sum, n) => sum - Math.min(0, n ?? 0), 0);
      return { day, amounts, positive, negative };
    });
    const above = bars.reduce((max, bar) => Math.max(max, bar.positive), 0);
    const below = bars.reduce((max, bar) => Math.max(max, bar.negative), 0);
    const scale = above + below;
    if (!scale) {
      node.append(muted(!days.length || partial ? "Daily detail unavailable." : "No daily spend in this period."));
      return node;
    }
    const tooltip = el("div", { class: "cost-tooltip", "aria-hidden": "true" });
    const plot = el("div", { class: "cost-plot", "aria-label": "Daily spend in USD" });
    plot.style.setProperty("--cost-days", Math.min(days.length, 7));
    const tick = days.length > 14 ? 7 : 1;
    let hovered, focused, pinned;
    const redraw = () => {
      const active = hovered ?? focused ?? pinned;
      tooltip.replaceChildren();
      if (active != null) {
        const { day, amounts } = bars[active];
        tooltip.append(el("strong", { text: day }), ...series.map((s, i) =>
          el("span", {}, swatch(s.key), `${s.title}: ${money(amounts[i])}`)));
      }
    };
    for (const [i, bar] of bars.entries()) {
      const label = [bar.day, ...series.map((s, j) => `${s.title}: ${money(bar.amounts[j])}`)].join(", ");
      const button = el("button", { class: "cost-bar", type: "button", "aria-label": label, "aria-pressed": "false" });
      const stack = (positive) => {
        const group = el("span", { class: `cost-stack cost-stack-${positive ? "up" : "down"}`, "aria-hidden": "true" });
        group.style.height = `${(positive ? above : below) / scale * 100}%`;
        bar.amounts.forEach((value, j) => {
          if (value == null || (positive ? value <= 0 : value >= 0)) return;
          const part = el("span", { class: `cost-segment cost-${series[j].key}` });
          part.style.height = `${Math.abs(value) / (positive ? above : below) * 100}%`;
          group.append(part);
        });
        return group;
      };
      button.append(el("span", { class: "cost-stack-area" }, stack(true), stack(false)),
        el("span", { class: "cost-tick", "aria-hidden": "true", text: i % tick === 0 ? dayLabel(bar.day) : "" }));
      button.addEventListener("pointerenter", (event) => {
        if (event.pointerType === "touch") return;
        hovered = i;
        redraw();
      });
      button.addEventListener("pointerleave", () => { hovered = undefined; redraw(); });
      button.addEventListener("focus", () => { focused = i; redraw(); });
      button.addEventListener("blur", () => { focused = undefined; redraw(); });
      const clear = () => {
        if (pinned != null) plot.children[pinned].setAttribute("aria-pressed", "false");
        pinned = undefined;
      };
      button.addEventListener("click", () => {
        const wasPinned = pinned === i;
        clear();
        if (wasPinned) focused = hovered = undefined;
        else {
          pinned = i;
          button.setAttribute("aria-pressed", "true");
        }
        selectDay(pinned == null ? null : bar.day);
        redraw();
      });
      button.addEventListener("keydown", (event) => {
        if (event.key !== "Escape") return;
        clear();
        selectDay(null);
        focused = hovered = undefined;
        redraw();
      });
      plot.append(button);
    }
    const legend = el("ul", { class: "cost-legend", "aria-label": "Spend series" }, series.map((s) =>
      el("li", {}, swatch(s.key), s.title)));
    node.append(muted("Select a day to filter the billing cards. Select it again to show the period."), legend, plot, tooltip);
    if (partial) node.append(muted("Some daily detail is unavailable."));
    return node;
  }

  const forDay = (item, day) => day == null ? item.total : item.daily ? item.daily.get(day) ?? 0 : null;

  function gatewayCard(cost, day) {
    return card(el("h2", { text: "AI Gateway" }),
      cost.aiDown && muted("AI Gateway billing is unavailable.", "status"),
      el("ul", { class: "cost-rows" }, cost.breakdown.map((g) => {
        const link = el("a", { class: `cost-console cost-${g.key}`, href: g.href,
          "aria-label": `Open ${g.name} console (new tab)`, title: `Open ${g.name} console` },
        el("span", { "aria-hidden": "true", text: g.mark }), el("span", { "aria-hidden": "true", text: "↗" }));
        link.setAttribute("target", "_blank");
        link.setAttribute("rel", "noopener noreferrer");
        return el("li", { class: "cost-gateway-row" }, swatch(g.key), el("div", {},
          el("strong", { text: g.name }), muted(g.label)), amount(forDay(g, day)), link);
      })),
      day == null && cost.fee ? muted(`Gross includes a ${money(cost.fee)} estimated Unified Billing credits fee.`) : null,
      cost.anthropicEstimate && muted("Anthropic billing is unavailable; Anthropic BYOK shows gateway estimates."),
      cost.unreconciled && muted("Some gateway spend could not be assigned to the billing rows."),
      cost.truncated && muted("Gateway logs were truncated; reported spend may be incomplete."));
  }

  function cloudflareCard(cost, day) {
    const rows = (cost.lines ?? []).map(row => ({ name: row.name,
      total: day == null ? number(row.total) : map(row.by_day) ? number(row.by_day[day] ?? 0) : null,
    })).filter(row => row.total !== 0);
    const order = (name) => products.includes(name) ? products.indexOf(name) : products.length;
    rows.sort((a, b) => order(a.name) - order(b.name));
    return card(el("h2", { text: "Cloudflare" }),
      cost.cfDown ? muted("Cloudflare billing is unavailable.", "status") : !cost.lines
        ? muted("Product detail unavailable.") : !rows.length ? muted(day == null ? "No product spend in this period." : "No product spend on this day.") : null,
      el("dl", { class: "cost-rows" }, rows.map((row) => el("div", { class: "cost-product-row" },
        el("dt", {}, swatch(`product-${order(row.name)}`), row.name), el("dd", {}, amount(row.total)))),
      el("div", { class: "cost-product-row cost-total" }, el("dt", { text: "Total" }),
        el("dd", {}, amount(forDay(cost.cloudflare, day))))));
  }

  register("cost", { subtabs: ranges, async render({ subtab, op }) {
    const cost = readCost(await op("cost_daily", { range: subtab }), subtab);
    const allSeries = [cost.cloudflare, ...cost.breakdown];
    const label = el("h2", { "aria-live": "polite" });
    const cards = el("div", { class: "cost-breakdowns" });
    function selectDay(day) {
      label.textContent = day == null ? "By billing · period" : `By billing · ${day}`;
      cards.replaceChildren(gatewayCard(cost, day), cloudflareCard(cost, day));
    }
    selectDay(null);
    return el("div", { class: "cost-view" }, overview(cost, allSeries),
      chart(cost.days, allSeries, allSeries.some((s) => !s.daily) || cost.unreconciled, selectDay), label, cards);
  } });
})();
