(() => {
  "use strict";
  const { registerSettingsPanel, el, card, pill } = window.pigeonApp;
  const roles = ["planner", "executor", "mechanical", "summary"];
  const kinds = ["integrate", "docs", "fix", "design"];
  const tierNames = ["top", "middle", "small"];
  const title = (s) => s[0].toUpperCase() + s.slice(1);
  const text = (v, fallback = "Unavailable") => typeof v === "string" && v ? v : fallback;
  const count = (n, label) => `${n} ${label}${n === 1 ? "" : "s"}`;
  const muted = (value) => el("p", { class: "app-muted", text: value });
  const unavailable = (label) => el("p", { class: "app-muted", role: "status", text: `${label} unavailable.` });
  const list = (rows) => el("ul", { class: "settings-list" }, rows);
  const item = (...children) => el("li", { class: "settings-row" }, children);
  const page = (...children) => el("div", { class: "settings-panel" }, children);
  const section = (name, description, ...children) => card(el("h2", { text: name }),
    description ? muted(description) : null, children);
  const details = (label, ...children) => el("details", { class: "settings-details" },
    el("summary", { text: label }), children);
  const facts = (entries) => el("dl", { class: "settings-facts" }, entries.map(([label, value]) =>
    el("div", {}, el("dt", { text: label }), el("dd", {}, value))));
  const code = (value) => el("code", { text: value });
  const link = (value, href) => el("a", { text: value, href });

  // Keep protected controls visible, but route people to the existing passkey UI separately.
  // This view has no enabled mutation handlers, secret inputs or write requests.
  function locked(action, target) {
    return el("div", { class: "settings-locked" },
      el("button", { class: "app-button", type: "button", disabled: true,
        text: action, "aria-label": `${action} ${target} — Needs passkey` }),
      el("small", { text: "Needs passkey" }));
  }

  async function read(op, name) {
    try { return await op(name); }
    catch { return null; }
  }

  // Project the metadata explicitly; nothing here asks for a credential value.
  function slots(data) {
    return Array.isArray(data) && data.every((r) => text(r?.name, "") && typeof r.set === "boolean")
      ? data.map(({ name, purpose, owner, set }) => ({ name, purpose, owner, set })) : null;
  }

  async function access(op) {
    const rows = slots(await read(op, "secrets_list"));
    return page(section("Credentials", "Only slot names, purposes and status are shown. Credential values are never displayed.",
      rows === null ? unavailable("Credential status") : rows.length === 0 ? muted("No credential slots.") :
        list([...rows].sort((a, b) => Number(a.set) - Number(b.set) || a.name.localeCompare(b.name)).map((r) =>
          item(el("div", { class: "settings-copy" }, code(r.name), muted(text(r.purpose, "No declared purpose."))),
            pill(r.set ? "Set" : r.owner ? "Missing" : "Not set", r.set ? "running" : r.owner ? "failed" : null),
            r.owner ? el("div", { class: "settings-slot-action" }, locked(r.set ? "Replace" : "Set", r.name),
              link("Open in Access", `/access#${encodeURIComponent(r.name)}`)) : muted("Unmanaged slot"))))));
  }

  function defaultRows(defaults, keys, byKind) {
    if (!defaults || typeof defaults !== "object" || Array.isArray(defaults)) return unavailable("Model defaults");
    if (!keys.some((key) => Object.hasOwn(defaults, key))) return muted("No model defaults reported.");
    return list(keys.map((key) => {
      const row = defaults[key];
      const label = title(byKind ? key.slice("planner_".length) : key);
      const inherits = byKind && row?.source === "unset";
      const source = { built_in: "Built-in", stored: "Stored", unset: "No override" };
      return item(el("div", { class: "settings-copy" }, el("h3", { text: label }),
        facts([
          ["Model", inherits ? "Uses the planner default" : code(text(row?.model))],
          ["Reasoning", text(byKind ? defaults.planner?.effort : row?.effort)],
          ["Source", Object.hasOwn(source, row?.source) ? source[row.source] : "Unavailable"],
        ])), locked("Edit", byKind ? `${label} planner override` : `${label} default`));
    }));
  }

  function keyStatus(rows, name) {
    if (rows === null) return { label: "Status unavailable", note: "Credential metadata could not be read." };
    const slot = rows.find((r) => r.name === name);
    return slot?.set ? { label: "Key set", tone: "running", note: "A key is stored; gateway health is not verified." }
      : slot ? { label: "Not set", tone: "failed", note: "Set this provider’s key in Access." }
        : { label: "Slot unavailable", note: "No matching credential slot was reported." };
  }

  function tierList(tiers) {
    if (!tiers || typeof tiers !== "object" || Array.isArray(tiers)) return unavailable("Model tiers");
    return facts(tierNames.map((tier) => [title(tier), tiers[tier] === null ? "No tier model" : code(text(tiers[tier]))]));
  }

  function provider(name, status, groups) {
    return el("article", { class: "settings-provider" },
      el("header", {}, el("h3", { text: name }), pill(status.label, status.tone)), muted(status.note), groups);
  }

  async function models(op) {
    const [data, secrets] = await Promise.all([read(op, "ai_agents_models"), read(op, "secrets_list")]);
    const rows = slots(secrets);
    const credits = keyStatus(rows, "CF_PARENT_TOKEN");
    const cloudStatus = credits.tone === "failed"
      ? { label: "Missing prerequisite", tone: "failed", note: "The Cloudflare parent token is not set." }
      : { label: "Readiness unverified", note: "Uses Cloudflare Unified Billing. Billing and gateway health are not reported here." };
    const cloudTiers = [["Anthropic via Cloudflare", "cf-anthropic"], ["OpenAI via Cloudflare", "cf-openai"], ["Workers AI", "workers-ai"]]
      .map(([label, key]) => el("div", {}, el("strong", { text: label }), tierList(data?.tiers?.[key])));
    return page(
      section("Defaults by role", "Effective defaults for each role, and their source.", defaultRows(data?.defaults, roles, false),
        Array.isArray(data?.notes) && data.notes.length ? list(data.notes.filter((note) => typeof note === "string")
          .map((note) => item(muted(note)))) : null),
      section("Planner by kind", "Kinds without an override use the planner default.",
        defaultRows(data?.defaults, kinds.map((k) => `planner_${k}`), true)),
      section("Providers", "Catalog tiers describe model choices, not live availability.",
        el("div", { class: "settings-providers" },
          provider("Anthropic BYOK", keyStatus(rows, "pigeon-anthropic_anthropic_default"), tierList(data?.tiers?.anthropic)),
          provider("OpenAI BYOK", keyStatus(rows, "pigeon-openai_openai_default"), tierList(data?.tiers?.openai)),
          provider("Cloudflare Models", cloudStatus, cloudTiers))));
  }

  function personCard(person, signedIn) {
    const name = text(person.name, "Unnamed person");
    const keys = Array.isArray(person.passkeys) ? person.passkeys : null;
    const delegates = Array.isArray(person.acting_for_me) ? person.acting_for_me : null;
    return card(el("header", { class: "settings-person" },
      el("span", { class: "app-initial", "aria-hidden": "true", text: text(person.initials, "?") }),
      el("div", {}, el("h3", { text: name }), muted(text(person.role, "Person"))),
      signedIn ? pill("Signed in", "done") : null),
      keys === null ? unavailable("Passkeys") : details(title(count(keys.length, "passkey")),
        keys.length ? list(keys.map((key) => item(text(key?.name, "Unnamed passkey")))) : muted("No passkeys registered.")),
      delegates === null ? unavailable(`Delegation for ${name}`) : details(`Acting for ${name} · ${delegates.length}`,
        delegates.length ? list(delegates.map((delegate) => item(text(delegate?.name, "Unnamed delegate"),
          pill(delegate?.kind === "app" ? "App" : delegate?.kind === "service" ? "Service user" : "Unknown kind"))))
          : muted("No apps or service users act for this person.")));
  }

  async function people(op) {
    const [data, clients, self] = await Promise.all([
      read(op, "users_people"), read(op, "oauth_clients"), read(op, "users_whoami"),
    ]);
    const persons = Array.isArray(data?.people) && data.people.every((p) => text(p?.id, "")) ? data.people : null;
    const signedIn = persons?.find((p) => p.id === self?.user?.id);
    const unlinked = Number.isSafeInteger(data?.unlinked) && data.unlinked >= 0 ? data.unlinked : null;
    const apps = Array.isArray(clients) ? clients.filter((c) => text(c?.service_user_id, "") && c.standard_user_id === null) : null;
    return page(section("People", "Who can sign in to the platform, and who acts for them.",
      locked("Invite someone", "to the platform"), link("Invite in Access", "/access#people")),
      persons === null ? card(unavailable("People")) : persons.length === 0 ? card(muted("No people to display.")) :
        [signedIn, ...persons.filter((p) => p !== signedIn)].filter(Boolean).map((p) => personCard(p, p === signedIn)),
      section("Unlinked service users", "These accounts act for no one.",
        unlinked === null ? unavailable("Unlinked service user count") : muted(unlinked === 0
          ? "No unlinked service users." : count(unlinked, "unlinked service user")),
        apps === null ? unavailable("Unlinked OAuth apps") : apps.length > 0 ?
          details("Unlinked OAuth apps", muted("This list covers OAuth apps only."),
            list(apps.map((app) => item(text(app.name, "Unnamed OAuth app"))))) : null,
        link("Review delegation in Access", "/access#delegation")));
  }

  for (const [key, panel] of Object.entries({ access, models, people })) {
    registerSettingsPanel(key, ({ op }) => panel(op));
  }
})();
