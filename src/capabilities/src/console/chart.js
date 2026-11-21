// The Cost tab: the range's total and each model family's share as stat cards, then a stacked bar
// chart of model cost per day, by family, with a tooltip by pointer or keyboard and the same
// numbers as a table below.
(() => {
  const { el, money, section } = window.pigeonConsole;
  const NS = "http://www.w3.org/2000/svg";
  const sv = (tag, attrs = {}) => { const n = document.createElementNS(NS, tag); for (const k in attrs) n.setAttribute(k, attrs[k]); return n; };
  // Colour follows the family, never its rank.
  const colour = (name) => { const i = ["Opus", "Sonnet", "Haiku"].indexOf(name); return i < 0 ? "var(--muted)" : `var(--s${i + 1})`; };
  const H = 240, M = { t: 12, r: 8, b: 28, l: 44 };
  const fmt = (d, o) => new Date(d + "T00:00:00Z").toLocaleDateString("en-GB", { timeZone: "UTC", ...o });
  const short = (d) => fmt(d, { day: "numeric", month: "short" });
  const niceStep = (max) => {
    const raw = max / 4, p = 10 ** Math.floor(Math.log10(raw));
    return Math.max(0.01, [1, 2, 2.5, 5, 10].find((m) => m * p >= raw) * p);
  };

  section("cost", (body, data, ctx) => {
    const { days, families } = data, n = days.length;
    const totals = days.map((_, i) => families.reduce((s, f) => s + (f.costs[i] || 0), 0));
    const max = Math.max(0, ...totals);
    const step = niceStep(max || 0.01);
    const top = Math.max(step, Math.ceil(max / step - 1e-9) * step);

    const rangeBtn = (d) => el("button", {
      type: "button", "aria-pressed": String(ctx.days === d), text: `${d} days`,
      onclick: () => { if (ctx.days !== d) ctx.reload(d); },
    });

    const svg = sv("svg", {
      tabindex: "0", role: "img",
      "aria-label": `Stacked bar chart of model cost per day over ${n} days, ${money(data.cost)} in total. ` +
        "Focus to read the latest day, use the left and right arrow keys to move between days, Escape to hide.",
    });
    const tip = el("div", { class: "c-tip", role: "status", hidden: true });
    const chart = el("div", { class: "c-chart" }, svg, tip);

    let cols = [], cur = -1, width = 0, band = 0, bw = 0;

    const hide = () => { cur = -1; cols.forEach((g) => g.classList.remove("dim")); tip.hidden = true; };
    const show = (i) => {
      if (!n || !cols.length) return;
      cur = Math.max(0, Math.min(n - 1, i));
      cols.forEach((g, j) => g.classList.toggle("dim", j !== cur));
      tip.replaceChildren(
        el("div", { class: "eyebrow", text: fmt(days[cur], { weekday: "short", day: "numeric", month: "short", year: "numeric" }) }),
        ...families.map((f) => el("div", { class: "c-tip-row" },
          el("i", { style: `background:${colour(f.name)}` }), el("span", { text: f.name }), el("b", { text: money(f.costs[cur] || 0) }))),
        el("div", { class: "c-tip-row c-tip-total" }, el("span", { text: "Total" }), el("b", { text: money(totals[cur]) })),
      );
      tip.hidden = false;
      const cx = M.l + cur * band + band / 2;
      let left = cx + bw / 2 + 8;
      if (left + tip.offsetWidth > width) left = Math.max(0, cx - bw / 2 - 8 - tip.offsetWidth);
      tip.style.left = `${left}px`;
      tip.style.top = `${M.t}px`;
    };

    const draw = (w) => {
      width = w;
      svg.replaceChildren();
      Object.entries({ width: w, height: H, viewBox: `0 0 ${w} ${H}` }).forEach(([k, v]) => svg.setAttribute(k, v));
      cols = [];
      if (!n) return;
      const pw = w - M.l - M.r, ph = H - M.t - M.b, base = M.t + ph;
      band = pw / n; bw = Math.min(28, band * 0.64);
      const y = (v) => base - (v / top) * ph;

      for (let i = 0; i * step <= top + 1e-9; i++) {
        const v = +(i * step).toFixed(10), yy = y(v);
        svg.append(
          sv("line", { class: i ? "c-grid" : "c-base", x1: M.l, x2: w - M.r, y1: yy, y2: yy }),
          Object.assign(sv("text", { class: "c-axis", "text-anchor": "end", x: M.l - 6, y: yy + 4 }), { textContent: "$" + parseFloat(v.toFixed(2)) }),
        );
      }

      // Walk back from the last day so the newest label always wins the spacing check.
      let prev = null;
      for (let i = n - 1; i >= 0; i--) {
        if (!(n <= 7 || (n - 1 - i) % 7 === 0)) continue;
        const cx = M.l + i * band + band / 2, last = i === n - 1, x = last ? cx + bw / 2 : cx;
        if (prev !== null && Math.abs(prev - x) < 58) continue;
        prev = x;
        const t = sv("text", { class: "c-axis", "text-anchor": last ? "end" : "middle", x, y: H - 8 });
        t.textContent = short(days[i]);
        svg.append(t);
      }

      days.forEach((_, i) => {
        const g = sv("g", { class: "c-col" }), x = M.l + i * band + (band - bw) / 2;
        const segs = families.map((f) => ({ f, c: f.costs[i] || 0 })).filter((s) => s.c > 0);
        let cum = 0;
        segs.forEach((s, j) => {
          // A 2 px gap above each segment but the first, taken from the upper one.
          const full = (s.c / top) * ph, h = Math.max(0, full - (j ? 2 : 0)), yt = base - cum - full;
          cum += full;
          const fill = colour(s.f.name);
          if (j < segs.length - 1) return g.append(sv("rect", { x, y: yt, width: bw, height: h, fill }));
          const r = Math.max(0, Math.min(4, bw / 2, h)), yb = yt + h;
          g.append(sv("path", { fill, d: `M${x} ${yb}V${yt + r}A${r} ${r} 0 0 1 ${x + r} ${yt}H${x + bw - r}A${r} ${r} 0 0 1 ${x + bw} ${yt + r}V${yb}Z` }));
        });
        cols.push(g);
        svg.append(g);
      });
      days.forEach((_, i) => svg.append(sv("rect", { x: M.l + i * band, y: M.t, width: band, height: ph, fill: "transparent" })));
      if (cur >= 0) show(cur);
    };

    const pick = (e) => {
      if (!n) return;
      show(Math.floor((e.clientX - svg.getBoundingClientRect().left - M.l) / band));
    };
    svg.addEventListener("pointermove", pick);
    svg.addEventListener("pointerdown", pick);
    svg.addEventListener("pointerleave", hide);
    svg.addEventListener("blur", hide);
    svg.addEventListener("focus", () => show(n - 1));
    svg.addEventListener("keydown", (e) => {
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        show(cur < 0 ? n - 1 : cur + (e.key === "ArrowLeft" ? -1 : 1));
      } else if (e.key === "Escape") hide();
    });

    const ro = new ResizeObserver(([entry]) => {
      if (!chart.isConnected && width) return ro.disconnect();
      const w = Math.round(entry.contentRect.width);
      if (w && w !== width) draw(w);
    });
    ro.observe(chart);

    const cell = (tag, text) => el(tag, { text });
    const table = el("details", { class: "c-table" }, el("summary", { text: "Show as table" }),
      el("div", { class: "c-scroll" }, el("table", {},
        el("thead", {}, el("tr", {}, cell("th", "Day"), ...families.map((f) => cell("th", f.name)), cell("th", "Total"))),
        el("tbody", {}, ...days.map((d, i) => el("tr", {},
          cell("td", short(d)), ...families.map((f) => cell("td", money(f.costs[i] || 0))), cell("td", money(totals[i])))).reverse()),
      )));

    const range = `${n} day${n === 1 ? "" : "s"}`;
    const stat = (label, value, swatch) => el("article", { class: "card c-stat" },
      el("span", { class: "eyebrow" }, swatch && el("i", { class: "c-swatch", style: `background:${swatch}` }), label),
      el("span", { class: "c-stat-n", text: value }));
    body.append(el("div", { class: "c-stats", style: "margin-bottom:24px" },
      stat(`Total, ${range}`, money(data.cost ?? totals.reduce((s, t) => s + t, 0))),
      ...families.map((f) => stat(f.name, money(f.total), colour(f.name)))));

    body.append(el("article", { class: "card", style: "gap:16px;padding:24px 28px" },
      el("div", { style: "display:flex;justify-content:space-between;align-items:center;flex-wrap:wrap;gap:12px" },
        el("span", { class: "eyebrow", text: "Model cost per day" }),
        el("span", { class: "c-range", role: "group", "aria-label": "Range" }, rangeBtn(7), rangeBtn(30))),
      el("div", { class: "pills" }, ...families.map((f) => {
        const c = colour(f.name);
        return el("span", { class: "spill", style: `border-color:${c}` },
          el("i", { style: `background:${c}` }), el("strong", { text: f.name }), el("span", { text: money(f.total) }));
      })),
      data.truncated && el("p", { class: "muted", text: "The gateway's log listing was cut short, so these figures are lower bounds." }),
      chart,
      table,
    ));
  });
})();
