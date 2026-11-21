// The console's Overview tab: what needs the person's attention (missing secrets, credentials
// expiring within a week or expired), each gateway's cost over the last week with links to
// where it is broken down, then every credential and when it expires.
(() => {
  const { el, money, expiryPill, get, navigate, tab } = window.pigeonConsole;

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
    const expiring = d.expiring || [];
    const credentials = d.credentials || [];
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

    if (expiring.length) {
      const expired = expiring.filter((r) => r.status === "expired");
      const first = expiring[0];
      const where = first.kind === "proxy" ? "/settings#credentials" : `/settings#${encodeURIComponent(first.name)}`;
      layer.append(el("article", { class: "card accent c-alert" },
        el("div", { class: "msg" },
          el("div", { class: "card-h" },
            el("span", { class: "pill accent", text: expired.length ? "Expired" : "Expiring" }),
            el("h3", { text: expiring.length === 1
              ? `${first.name} ${first.status === "expired" ? "has expired" : `expires ${first.expires}`}`
              : `${expiring.length} credentials expire within a week or have expired` })),
          el("p", { text: expiring.map((r) => `${r.name} (${r.status === "expired" ? "expired" : r.expires})`).join(", ") })),
        el("button", { class: "button", type: "button", text: "Replace it", onclick: () => navigate(where) })));
    }

    layer.append(el("div", { class: "c-stats" },
      stat("Total cost", d.total, "Gateways", "/gateways"),
      ...costs.map((c) => stat(c.name, c.cost,
        c.name === "Model cost" ? "Models" : c.name.replace(/ cost$/, ""), c.path))));

    if (credentials.length) {
      layer.append(el("div", { class: "layer" },
        el("h3", { text: "Credentials" }),
        el("p", { class: "muted", style: "margin:0", text: "Each secret's expiry as its issuer reports it, checked at most daily, and each proxy credential's. Values are never shown." }),
        el("ul", { class: "c-list c-expiry" }, ...credentials.map((r) => el("li", {},
          el("div", {},
            el("strong", { text: r.name }),
            el("p", { class: "muted", style: "margin:0", text: `${r.kind === "proxy" ? "proxy credential" : "secret"} · ${r.owner}` })),
          expiryPill(r))))));
    }

    main.append(layer);
  });
})();
