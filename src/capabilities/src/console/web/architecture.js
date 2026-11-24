(() => {
  "use strict";
  const { register, el } = window.pigeonApp;
  const dash = "—";

  function table(label, columns, rows) {
    if (!rows.length) return el("p", { class: "app-muted", text: `No ${label.toLowerCase()} in this build.` });
    return el("div", { class: "arch-frame arch-table", role: "region", "aria-label": label, tabindex: "0" },
      el("table", { "aria-label": label },
        el("thead", {}, el("tr", {}, columns.map(text => el("th", { scope: "col", text })))),
        el("tbody", {}, rows.map(([name, ...cells]) => el("tr", {},
          el("th", { scope: "row" }, el("code", { text: name })),
          cells.map(content => el("td", {}, content)))))));
  }

  function list(items) {
    return items.length ? el("ul", {}, items.map(text => el("li", {}, el("code", { text })))) : dash;
  }

  function workersView(workers) {
    return table("Workers", ["Worker", "MCP tools", "Durable Objects", "Containers", "Routes"],
      workers.map(w => [w.name, String(w.tools.length), list(w.objects.map(o =>
        o.owner === w.name ? o.class : `${o.class} → ${o.owner}`)), list(w.containers), list(w.routes)]));
  }

  function toolsView(owners) {
    const tools = [...owners].sort(([a], [b]) => a.localeCompare(b));
    return table("MCP tools", ["Tool", "Worker"], tools.map(([name, workers]) => [name, list(workers)]));
  }

  function mapView(svg) {
    if (typeof svg !== "string" || !svg.startsWith("<svg ")) throw new Error("The platform did not supply a diagram.");
    // SVG in image mode cannot run scripts or load external resources. Never insert markup.
    const bytes = new TextEncoder().encode(svg);
    let binary = "";
    for (const byte of bytes) binary += String.fromCharCode(byte);
    const img = el("img", { alt: "Platform architecture: routes, Workers, Durable Objects, containers and services" });
    img.src = `data:image/svg+xml;base64,${btoa(binary)}`;
    return el("div", { class: "arch-frame arch-map", role: "region", "aria-label": "Platform diagram", tabindex: "0" }, img);
  }

  register("architecture", {
    subtabs: { map: "Map", workers: "Workers", tools: "Tools" },
    async render({ op, subtab }) {
      const { workers, parts, hostnames, commit, digest, drawing } = await op("architecture_describe", subtab === "map" ? { svg: true } : {});
      const tools = new Map();
      for (const w of workers) {
        for (const name of new Set(w.tools)) {
          if (!tools.has(name)) tools.set(name, []);
          tools.get(name).push(w.name);
        }
      }
      const counts = el("dl", { class: "arch-counts" }, [
        ["Workers", workers.length], ["Capabilities", parts.filter(p => p.capability).length],
        ["MCP tools", tools.size], ["Hostnames", hostnames.length],
      ].map(([label, count]) => el("div", {}, el("dt", { text: label }), el("dd", { text: count }))));
      const body = subtab === "workers" ? workersView(workers)
        : subtab === "tools" ? toolsView(tools) : mapView(drawing);
      return el("section", { class: "arch-view", "aria-label": "Architecture details" }, counts, body,
        el("p", { class: "arch-build", text: `${commit ? `deployed ${commit.slice(0, 12)}` : "local build"} · digest ${digest}` }));
    },
  });
})();
