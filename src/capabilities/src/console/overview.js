// The console's Overview tab: what needs the person's attention, then each gateway's cost over
// the last week with links to where it is broken down.
(() => {
  const { el, money, get, navigate, tab } = window.pigeonConsole;

  const go = (href) => (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    navigate(href);
  };

  const stat = (label, value, linkText, href) =>
    el("article", { class: "card c-stat" },
      el("span", { class: "eyebrow", text: `${label} · last 7 days` }),
      el("span", { class: "c-stat-n", text: value == null ? "—" : money(value) }),
      el("a", { class: "c-stat-link", href, onclick: go(href) },
        `${linkText} `, el("span", { "aria-hidden": "true", text: "→" })));

  tab("overview", async (main) => {
    let d;
    try {
      d = await get("/console/api/overview");
    } catch (err) {
      if (!err.auth) main.append(el("p", { class: "c-error", role: "alert", text: err.message || String(err) }));
      return;
    }
    const missing = d.missing || [];
    const costs = d.costs || [];
    const one = missing.length === 1;
    const layer = el("div", { class: "layer", style: "gap:24px" });

    if (missing.length) {
      layer.append(el("article", { class: "card accent c-alert" },
        el("div", { class: "msg" },
          el("div", { class: "card-h" },
            el("span", { class: "pill accent", text: "Needs you" }),
            el("h3", { text: one ? `${missing[0].name} is missing` : `${missing.length} secrets are missing` })),
          el("p", { text: one ? `${missing[0].owner} needs it before it can work.` : missing.map((m) => m.name).join(", ") })),
        el("button", {
          class: "button", type: "button", text: "Set it",
          onclick: () => navigate(`/settings#${encodeURIComponent(missing[0].name)}`),
        })));
    }

    layer.append(el("div", { class: "c-stats" },
      stat("Total cost", d.total, "Gateways", "/gateways"),
      ...costs.map((c) => stat(c.name, c.cost,
        c.name === "Model cost" ? "Models" : c.name.replace(/ cost$/, ""), c.path))));

    main.append(layer);
  });
})();
