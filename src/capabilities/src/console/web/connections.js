(() => {
  "use strict";
  const { register, registerSettingsPanel, el, card, pill, url } = window.pigeonApp;
  const text = (value, fallback = "Unavailable") => typeof value === "string" && value ? value : fallback;
  const muted = (value) => el("p", { class: "app-muted", text: value });
  const status = (value = "") => el("p", { class: "app-muted connection-status", role: "status", text: value });
  const code = (value) => el("code", { text: value });
  const link = (label, href) => el("a", { text: label, href });
  const page = (...children) => el("div", { class: "settings-panel" }, children);
  const section = (title, description, ...children) => card(el("h2", { text: title }), description && muted(description), children);
  const list = (...rows) => el("ul", { class: "settings-list" }, rows);
  const row = (...children) => el("li", { class: "settings-row" }, children);
  const copy = (title, ...children) => el("div", { class: "settings-copy" }, el("h3", { text: title }), children);
  const facts = (pairs) => el("dl", { class: "settings-facts" }, pairs.map(([label, value]) =>
    el("div", {}, el("dt", { text: label }), el("dd", {}, value))));
  const locked = (action, target) => el("span", { class: "settings-locked" },
    el("button", { type: "button", class: "app-button", disabled: true, text: action,
      "aria-label": `${action} ${target} — Needs passkey` }), el("small", { text: "Needs passkey" }));
  const read = async (op, name, args) => {
    try { return await op(name, args); } catch { return null; }
  };
  const date = (ms) => Number.isFinite(ms) && Number.isFinite(new Date(ms).getTime())
    ? new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(ms) : "Unavailable";
  const manageInstance = (id) => `/app?tab=instances&i=${encodeURIComponent(id)}`;
  const isLinked = (record) => record?.account?.kind === "linked" && typeof record.id === "string";
  const asArray = (value) => Array.isArray(value) ? value : [];

  function endpoint(title, endpointUrl) {
    const message = status();
    const button = el("button", { type: "button", class: "app-button", text: "Copy",
      "aria-label": `Copy ${title} MCP URL` });
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(endpointUrl);
        message.textContent = "Copied.";
      } catch { message.textContent = "Could not copy. Select the URL to copy it manually."; }
    });
    return row(copy(title, code(endpointUrl), message), button);
  }

  async function connections({ op }) {
    const [records, self, people] = await Promise.all([
      read(op, "instances_list"), read(op, "users_whoami"), read(op, "users_people"),
    ]);
    const accounts = Array.isArray(records) ? records.filter(isLinked) : null;
    const me = self?.user?.id && asArray(people?.people).find((p) => p.id === self.user.id);
    const acting = Array.isArray(me?.acting_for_me) ? me.acting_for_me : null;
    const delegates = (kind) => {
      if (acting === null) return muted("Delegation unavailable.");
      const matches = acting.filter((d) => d.kind === kind);
      return matches.length ? list(matches.map((d) =>
        row(copy(text(d.name, "Unnamed connection")), locked(kind === "app" ? "Revoke" : "Unlink", text(d.name))))) :
        muted(kind === "app" ? "No OAuth apps act for you." : "No service users act for you.");
    };
    return page(
      section("MCP endpoints", "Copy the URL for your MCP client. Clients still need authorization.", list(
        endpoint("Direct platform", new URL("/mcp", location.origin).href),
        endpoint("Cloudflare Access portal", "https://mcp.pigeon.dev/mcp"))),
      section("Linked accounts", "Manage Cloudflare access per instance.",
        accounts?.length ? list(accounts.map((i) => row(copy(text(i.title, i.id),
          muted(asArray(i.zones).join(", ") || "No zone recorded")),
          link("Manage", url({ instance: i.id, tab: "settings", subtab: "access" }))))) :
          muted(accounts ? "No linked instances." : "Linked instances unavailable."),
        link("Open Instances", "/app?tab=instances")),
      section("Acting for you", "Connections assigned to the signed-in person.",
        el("h3", { text: "OAuth apps" }), delegates("app"),
        el("h3", { text: "Service users" }), delegates("service"),
        link("Manage delegation in Access", "/app?tab=management#delegation")));
  }

  async function linkedAccess({ instance, op }) {
    const details = await read(op, "app_linked_details", { instance: instance.id });
    const state = details?.id === instance.id ? details.link : null;
    const token = { linked: state?.fresh === true ? "Access token current" : state?.fresh === false ? "Access token expired" : "Linked; expiry unavailable",
      "not linked": "Not linked", "refresh failed": "Refresh failed", "account mismatch": "Account mismatch; link details hidden" };
    return page(section("Cloudflare account", "Access to this instance’s Cloudflare account.", facts([
      ["Account", text(state?.account_name)],
      ["Account ID", code(text(instance.account.account_id ?? state?.account_id))],
      ["Linked", date(state?.linked_ms)],
      ["Token state", Object.hasOwn(token, state?.status) ? token[state.status] : "Unavailable"],
      ["Access expiry", date(state?.access_expires_ms)],
      ["Zones", asArray(instance.zones).join(", ") || "None recorded"],
      ["Scopes", text(state?.scope)],
    ]), el("div", { class: "connection-actions" },
      locked(state?.status === "not linked" ? "Link" : "Relink", instance.title), locked("Unlink", instance.title),
      link("Manage Cloudflare link in Instances", manageInstance(instance.id)))),
    section("Instance secrets", "Only credentials owned by this instance belong here.",
      muted("Instance-owned secret metadata is unavailable. Platform credentials are not shown."),
      locked("Add secret", instance.title)));
  }

  function repository(repo) {
    if (repo?.kind === "github") return text(repo.full_name);
    if (repo?.kind === "artifacts" && repo.namespace && repo.repo) return `${repo.namespace}/${repo.repo}`;
    return "Unavailable";
  }

  async function site({ instance, op }) {
    const details = instance.worker ? null : await read(op, "app_linked_details", { instance: instance.id });
    const worker = instance.worker ?? (details?.id === instance.id ? details.worker : null);
    const targets = (site) => asArray(site.targets).map((t) => {
      if (t.kind === "preview") return "Preview";
      if (t.kind === "subdomain" && t.label) return `${t.label}.pigeon.dev`;
      if (t.kind === "zone" && t.hostname) return `${t.hostname}${t.enabled === false ? " (route off)" : ""}`;
      if (t.kind === "domain" && t.hostname) return t.hostname;
      return "Unavailable";
    });
    return page(section("Site configuration", "Declared configuration, not deployment status.", facts([
      ["Repository", code(repository(instance.repo))], ["Worker", code(text(worker))],
    ]), Array.isArray(instance.sites) && instance.sites.length ? list(instance.sites.map((s) =>
      row(copy(text(s.name, "Unnamed site"), facts([
        ["Bucket", code(text(s.bucket))], ["Domains and targets", targets(s).join(", ") || "None recorded"],
      ]))))) : muted(Array.isArray(instance.sites) ? "No sites recorded." : "Site records unavailable."),
    link("Manage in Instances", manageInstance(instance.id))));
  }

  const major = (version) => typeof version === "string" ? version.match(/^\s*\^?(\d+)(?:\.|$)/)?.[1] : null;
  const versionOrder = (a, b) => a.localeCompare(b, undefined, { numeric: true });
  function componentRow(name, component, pin, step, savedPin) {
    const problems = asArray(component?.problems);
    const pinned = typeof savedPin === "string";
    // Show an upgrade only when the record, version check and plan all agree.
    const matchingPin = pinned && pin?.requirement === savedPin;
    const fromMajor = major(step?.from ?? savedPin);
    const toMajor = major(step?.to);
    const upgrade = matchingPin && step?.pin === savedPin && step?.from === pin.selected &&
      step?.to === component?.version && !problems.length && fromMajor && toMajor && Number(toMajor) > Number(fromMajor);
    const version = problems.length ? "Unavailable" : pinned
      ? text(matchingPin ? pin.selected : null) : text(component?.version);
    const served = [...new Set([component?.version, ...asArray(component?.serves)].filter(Boolean))];
    return row(copy(name,
      facts(pinned ? [["Pin", code(savedPin)], ["Selected version", code(version)]] : [["Version", code(version)]]),
      problems.length ? muted(problems.join(" · ")) : null,
      matchingPin && pin.note ? muted(pin.note) : null,
      served.length > 1 ? muted(`Serves ${served.join(", ")}`) : null,
      asArray(component?.changelog).length ? el("details", { class: "settings-details" },
        el("summary", { text: "Changelog" }), el("ul", {}, component.changelog.map((entry) =>
          el("li", { text: `${entry.version}: ${entry.summary}` })))) : null),
      !pinned && component?.version && !problems.length ? pill("Latest", "done") : null,
      upgrade ? el("div", { class: "connection-upgrade" }, pill(`Upgrade available · ${step.to}`, "running"),
        locked("Upgrade", name)) : null);
  }

  async function capabilities({ instance, op }) {
    const linked = isLinked(instance);
    const [versions, plan] = await Promise.all([
      read(op, "capabilities_versions", { changelog: true, ...(linked ? { instance: instance.id } : {}) }),
      linked ? read(op, "capabilities_upgrade_plan", { instance: instance.id }) : null,
    ]);
    if (!Array.isArray(versions?.components)) return page(section("Capabilities", "Version catalog unavailable."));
    const pins = linked ? asArray(versions.instances).find((i) => i.id === instance.id)?.pins : null;
    const steps = linked ? asArray(plan?.instances).find((i) => i.id === instance.id)?.steps : null;
    const saved = linked ? (instance.pins ?? {}) : {};
    const toRow = (name, component) => componentRow(name, component, asArray(pins).find((p) => p.component === name),
      asArray(steps).find((s) => s.component === name), Object.hasOwn(saved, name) ? saved[name] : undefined);
    const workers = new Map();
    const other = [];
    for (const c of [...versions.components].sort((a, b) => versionOrder(a.name, b.name))) {
      if (c.kind !== "capability") { other.push(c); continue; }
      const worker = text(c.worker, "Worker unavailable");
      if (!workers.has(worker)) workers.set(worker, []);
      workers.get(worker).push(toRow(c.name, c));
    }
    const undeclared = [...new Set([...asArray(versions.undeclared), ...Object.keys(saved)])]
      .filter((name) => !versions.components.some((c) => c.name === name)).sort(versionOrder);
    return page(section("Capabilities", linked ? "The platform catalog and this instance’s own pins." :
      "Latest declarations, grouped by the Worker that serves them.",
      linked && !Array.isArray(pins) ? status("Pin status unavailable.") : null,
      linked && !Array.isArray(steps) ? status("Upgrade plan unavailable.") : null,
      !workers.size && !undeclared.length ? muted("No capabilities declared.") : null),
    [...workers].sort(([a], [b]) => versionOrder(a, b)).map(([worker, rows]) => section(worker, null, list(rows))),
    undeclared.length ? section("No version declared", null, list(undeclared.map((name) => toRow(name)))) : null,
    section("Connectors and other components", "Versions declared by the platform, not external service status.",
      other.length ? list(other.map((c) => toRow(c.name, c))) : muted("No connectors or other components declared.")));
  }

  registerSettingsPanel("connections", connections);
  registerSettingsPanel("capabilities", capabilities);
  register("linked-home", ({ instance }) => {
    const rows = [];
    if (instance.zones?.length) rows.push(["Zones", instance.zones.join(", ")]);
    if (instance.sites?.length) rows.push(["Sites", instance.sites.map(s => s.name).join(", ")]);
    if (instance.repo) rows.push(["Repository", code(repository(instance.repo))]);
    return page(section("Nothing here yet", "Sessions and costs are on the platform, not this instance."),
      section(text(instance.title, instance.id), null, rows.length ? facts(rows) : muted("No site or repository recorded."),
        link("Open instance Settings", url({ tab: "settings" }))),
      link("Switch to Pigeon Platform", url({ instance: null, tab: "sessions" })));
  });
  const linkedPanels = { access: linkedAccess, site, capabilities };
  register("linked-settings", {
    subtabs: { access: "Access", site: "Site", capabilities: "Capabilities" },
    render: (ctx) => linkedPanels[ctx.subtab](ctx),
  });
})();
