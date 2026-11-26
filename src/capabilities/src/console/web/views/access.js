/* Access: Secrets (secrets), Tokens (proxy), People and Apps (users). Ported from the first
 * console's keys.js (Credentials) and users.js (People, Delegation), mounted until now through
 * management.js, and the read-only copies in settings.js (C1: this is the only place they are
 * shown). Reads go through /app/op; every write is a ConfirmDialog on /console/write. A secret's
 * value is typed into the dialog, sent once at finish (begin carries only its SHA-256), and
 * never shown or kept; a token or invite link is shown once (ShowOnce). */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, tag, code, button, field, input, dataTable, confirm } = shell;

  const day = (ms) => (ms ? new Date(ms).toISOString().slice(0, 10) : "—");
  const DAY = 86_400_000;
  const refresh = (ctx) => () => setTimeout(ctx.refresh, 0);
  const more = (summary, ...children) => el("details", { class: "more" }, el("summary", { text: summary }), el("div", { class: "section" }, ...children));
  const intro = (text, ...right) => el("div", { class: "toolbar" }, el("p", { class: "muted grow", text }), ...right);

  // base64url SHA-256, as the write path binds a secret's value at begin.
  async function sha256(text) {
    const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  // ---- Secrets: every declared secret (missing first), its owner, purpose, state and expiry;
  // the store's undeclared ones listed only; Apple signing once APPLE_ASC_KEY is set (P8).
  function expiryPill(row) {
    if (!row) return "—";
    switch (row.status) {
      case "ok": return row.expires ? pill(`Expires ${row.expires}`, "plain") : pill("No expiry", "plain");
      case "soon": return pill(row.days_left === 0 ? "Expires today" : `Expires in ${row.days_left} days`, "warn");
      case "expired": return pill(row.expiry?.kind === "refused" ? "Refused by issuer" : "Expired", "bad");
      case "missing": return "—";
      default: return pill("Unknown", "plain");
    }
  }

  function setSecret(ctx, s) {
    const value = input({ type: "password", mono: true, autocomplete: "off", spellcheck: "false", "aria-label": `Value for ${s.name}` });
    confirm({
      title: s.set ? "Replace secret" : "Set secret",
      target: [["Secret", code(s.name)], ["Owner", s.owner], ["Purpose", s.purpose || "—"]],
      form: field({ label: s.set ? "New value" : "Value", control: value,
        hint: "Your passkey is bound to this name and a hash of this value. The value is never shown or kept on this page." }),
      effect: s.set ? "The new value replaces the stored one at once." : null,
      action: "secret-set", confirmLabel: "Save with passkey",
      prepare: async () => {
        const v = value.value.trim();
        if (!v) throw new Error("Type the value first.");
        return { id: s.name, change: { value: v }, begin: { digest: await sha256(v) } };
      },
      onDone: refresh(ctx),
    });
  }

  const APPLE = [
    { action: "apple-adopt", title: "Adopt bundle id", label: "Adopt",
      what: "Registers the bundle id if Apple has none, and adds Push Notifications and Associated Domains where missing. Apple marks existing App Store profiles for it invalid when its capabilities change." },
    { action: "apple-identity", title: "Create signing identity", label: "Create",
      what: "Makes a key in pigeon, an Apple Distribution certificate for it and an App Store profile. The first run only makes APPLE_SEAL_KEY; run it again once its binding has landed." },
  ];
  const BUNDLE = "dev.pigeon.app";

  function appleSection(ctx) {
    const run = (a) => confirm({
      title: a.title, target: [["App", code(BUNDLE)], ["Effect", a.what]],
      action: a.action, id: BUNDLE, confirmLabel: "Run with passkey",
      onDone: refresh(ctx),
    });
    return el("section", { class: "section" },
      el("div", { class: "section-head" }, el("h2", { text: "Apple signing" }), el("span", { class: "muted", text: "Shown because APPLE_ASC_KEY is set. apple_status shows what Apple holds." })),
      dataTable({
        label: "Apple signing", noun: "Apple actions", signal: ctx.signal,
        columns: [
          { label: "Action", cell: (a) => el("strong", { text: a.title }) },
          { label: "What it does", cls: "wide", cell: (a) => el("span", { class: "ink2", text: a.what }) },
          { label: "", cls: "acts", cell: (a) => button(a.label, () => run(a), { size: "sm" }) },
        ],
        load: APPLE,
      }));
  }

  function secrets(ctx) {
    const extras = el("div", { class: "view" });
    const table = dataTable({
      label: "Secrets", noun: "secrets", filter: "Filter secrets", signal: ctx.signal,
      empty: "No capability declares a secret.",
      columns: [
        { label: "Name", cls: "mono", cell: (s) => el("strong", { text: s.name }) },
        { label: "Owner", cell: (s) => s.owner },
        { label: "Purpose", cls: "wide", cell: (s) => el("span", { class: "ink2", text: s.purpose || "—" }) },
        { label: "Status", cell: (s) => (s.set ? pill("Set", "ok") : pill("Missing", "bad")) },
        { label: "Changed", cls: "num", cell: (s) => (s.modified ? String(s.modified).slice(0, 10) : "—") },
        { label: "Expiry", cell: (s) => (s.set ? expiryPill(s.expiry) : "—") },
        { label: "", cls: "acts", cell: (s) => button(s.set ? "Replace" : "Set", () => setSecret(ctx, s), { size: "sm", variant: s.set ? null : "primary" }) },
      ],
      load: async () => {
        const [list, expiry] = await Promise.all([
          ctx.op("secrets_list"),
          ctx.op("credentials_expiry").catch(() => null),
        ]);
        const rows = Array.isArray(list) ? list : [];
        const expiries = (expiry?.credentials || []).filter((r) => r.kind === "secret");
        const declared = rows.filter((s) => s.owner).map((s) => ({ ...s, expiry: expiries.find((r) => r.name === s.name) }))
          .sort((a, b) => Number(a.set) - Number(b.set) || a.name.localeCompare(b.name));
        const others = rows.filter((s) => !s.owner);
        const apple = rows.some((s) => s.name === "APPLE_ASC_KEY" && s.set);
        extras.replaceChildren(
          others.length ? more(`${others.length} ${others.length === 1 ? "secret" : "secrets"} in the store that no capability declares`,
            dataTable({ label: "Undeclared secrets", noun: "secrets", signal: ctx.signal, load: others,
              columns: [
                { label: "Name", cls: "mono", cell: (s) => s.name },
                { label: "Changed", cls: "num", cell: (s) => (s.modified ? String(s.modified).slice(0, 10) : "—") },
                { label: "Note", cell: () => el("span", { class: "muted", text: "Listed only. No capability reads it." }) },
              ] })) : null,
          apple ? appleSection(ctx) : null);
        return declared;
      },
    });
    return el("div", { class: "view" },
      intro("Values are never shown. Missing secrets come first; setting one asks for your passkey, bound to that name and value."),
      table, extras);
  }

  // ---- Tokens: live proxy credentials, issued once (ShowOnce) and revoked; those that ended in
  // the last two weeks below.
  function tokenStatus(t) {
    const days = Math.floor((t.expires_at - Date.now()) / DAY);
    return days <= 0 ? pill("Ends today", "warn") : pill(`${days} ${days === 1 ? "day" : "days"} left`, days <= 7 ? "warn" : "ok");
  }

  function issueToken(ctx) {
    const label = input({ maxlength: "64", placeholder: "ci-gateway session", "aria-label": "Label" });
    const days = input({ type: "number", min: "1", max: "90", value: "30", step: "1", "aria-label": "Lifetime in days" });
    confirm({
      title: "Issue proxy token",
      form: el("div", { class: "section" },
        field({ label: "Label", control: label, hint: "Names the cloud session or machine that will hold it." }),
        field({ label: "Lifetime in days", control: days, hint: "1 to 90 days." })),
      effect: "The token is shown once, after your passkey. Put it in the cloud session’s environment then.",
      action: "token-issue", confirmLabel: "Issue with passkey",
      prepare: () => {
        const text = label.value.trim();
        const n = Number(days.value);
        if (!text) throw new Error("Give the token a label.");
        if (!Number.isInteger(n) || n < 1 || n > 90) throw new Error("Lifetime must be a whole number from 1 to 90 days.");
        return { id: text, change: { days: n } };
      },
      onDone: refresh(ctx),
    });
  }

  function revokeToken(ctx, t) {
    confirm({
      title: "Revoke proxy token", danger: true,
      target: [["Token", [el("strong", { text: t.label }), " ", code(t.id)]], ["Issued by", t.person || "someone removed"], ["Expires", day(t.expires_at)]],
      effect: "Sessions using it lose proxy access at once.",
      action: "token-revoke", id: t.id, confirmLabel: "Revoke with passkey",
      onDone: refresh(ctx),
    });
  }

  function tokens(ctx) {
    const ended = el("div", { class: "view" });
    const table = dataTable({
      label: "Proxy tokens", noun: "proxy tokens", signal: ctx.signal,
      empty: "No proxy tokens. Issue one for a cloud session that needs the proxies.",
      columns: [
        { label: "Label", cell: (t) => el("strong", { text: t.label }) },
        { label: "Id", cls: "mono", cell: (t) => t.id },
        { label: "Issued by", cell: (t) => t.person || "someone removed" },
        { label: "Issued", cls: "num", cell: (t) => day(t.created_at) },
        { label: "Expires", cls: "num", cell: (t) => day(t.expires_at) },
        { label: "Status", cell: tokenStatus },
        { label: "", cls: "acts", cell: (t) => button("Revoke", () => revokeToken(ctx, t), { size: "sm", variant: "danger" }) },
      ],
      load: async () => {
        const data = await ctx.op("app_proxy_tokens");
        const now = Date.now();
        const all = Array.isArray(data?.tokens) ? data.tokens : [];
        const gone = all.filter((t) => t.expires_at <= now);
        ended.replaceChildren(gone.length ? more(`Expired in the last 14 days (${gone.length})`,
          dataTable({ label: "Expired proxy tokens", noun: "tokens", signal: ctx.signal, load: gone,
            columns: [
              { label: "Label", cell: (t) => t.label },
              { label: "Id", cls: "mono", cell: (t) => t.id },
              { label: "Issued by", cell: (t) => t.person || "someone removed" },
              { label: "Status", cell: (t) => pill(`Expired ${day(t.expires_at)}`, "plain") },
            ] })) : "");
        return all.filter((t) => t.expires_at > now);
      },
    });
    return el("div", { class: "view" },
      intro("A proxy token lets a cloud session reach the platform’s git and Cloudflare proxies, and nothing else. Each is shown once, when issued.",
        button("Issue token", () => issueToken(ctx), { variant: "primary" })),
      table, ended);
  }

  // ---- People and their passkeys; invite with a one-time link (ShowOnce).
  function invitePerson(ctx) {
    const name = input({ maxlength: "64", placeholder: "Their name", "aria-label": "Name" });
    confirm({
      title: "Invite person",
      form: field({ label: "Name", control: name, hint: "They register their own passkey from the link, which works once, within 15 minutes." }),
      action: "invite", confirmLabel: "Create invite with passkey",
      prepare: () => {
        const text = name.value.trim();
        if (!text) throw new Error("Give them a name first.");
        return { id: text };
      },
      onDone: refresh(ctx),
    });
  }

  function removePasskey(ctx, person, key) {
    confirm({
      title: "Remove passkey", danger: true,
      target: [["Person", person.name], ["Passkey", key.name || "Unnamed passkey"], ["Added", day(key.created_at)]],
      effect: "That device can no longer sign in or approve.",
      action: "passkey-remove", id: key.id, confirmLabel: "Remove with passkey",
      onDone: refresh(ctx),
    });
  }

  // Make a person an admin, or a member again. Only an admin may; the last admin stays one.
  function setAdmin(ctx, person, make) {
    confirm({
      title: make ? "Make admin" : "Make member", danger: !make,
      target: [["Person", person.name], ["Now", person.admin ? "Admin" : "Member"]],
      effect: make
        ? "They can invite and remove people, remove anyone's passkeys, disconnect apps and make other admins."
        : "They keep their own passkeys and apps, and can no longer manage other people's.",
      action: "person-admin", id: person.id, change: { admin: make },
      confirmLabel: make ? "Make admin with passkey" : "Make member with passkey",
      onDone: refresh(ctx),
    });
  }

  // A person's role on one instance (docs/plans/membership.md): owner, operator, viewer, or
  // none to remove them. Only the instance's owner or an admin may; the server checks again.
  const ROLES = ["owner", "operator", "viewer", "none"];
  const ROLE_TEXT = {
    owner: "Owner: reads, writes, approves and manages who belongs",
    operator: "Operator: reads, writes and approves changes",
    viewer: "Viewer: reads only",
    none: "None: removed from this instance",
  };
  function setRole(ctx, person, instance, now) {
    const select = el("select", { class: "input", "aria-label": "Role" },
      ROLES.map((r) => el("option", { value: r, text: ROLE_TEXT[r], selected: r === (now || "none") })));
    confirm({
      title: "Change role",
      target: [["Person", person.name], ["Instance", instance], ["Now", now || "None"]],
      form: field({ label: "Role", control: select }),
      action: "member-set", id: instance,
      prepare: () => {
        if (select.value === (now || "none")) throw new Error("Pick another role first.");
        return { change: { person: person.id, role: select.value } };
      },
      confirmLabel: "Change with passkey",
      onDone: refresh(ctx),
    });
  }

  const lastUsed = (p) => Math.max(0, ...p.passkeys.map((k) => k.last_used_at || 0));
  const rolesText = (p) => Object.entries(p.roles || {}).map(([i, r]) => `${i}: ${r}`).join(", ") || "—";

  function people(ctx) {
    // Whether the one asking is an admin, from the read; the server checks every write again.
    let meAdmin = false;
    // The instances the one asking may see, and those whose members they may change.
    let instances = [];
    let manage = [];
    const actions = el("span");
    const appsLink = (n) => (n && ctx.nav ? el("a", { href: shell.href({ instance: ctx.instance.id, area: "access", view: "apps" }), text: String(n),
      on: { click: (e) => { if (e.button === 0 && !e.metaKey && !e.ctrlKey) { e.preventDefault(); ctx.nav({ instance: ctx.instance.id, area: "access", view: "apps" }); } } } }) : String(n || 0));
    const table = dataTable({
      label: "People", noun: "people", filter: "Filter people", signal: ctx.signal, empty: "No people yet.",
      columns: [
        { label: "Person", cell: (p) => el("span", { class: "who-cell" }, el("span", { class: "avatar", "aria-hidden": "true", text: p.initials || "?" }),
          el("strong", { text: p.name }), p.me ? pill("You", "accent") : null, p.admin ? pill("Admin", "plain") : null) },
        { label: "Passkeys", cls: "num", cell: (p) => String(p.passkeys.length) },
        { label: "Last sign-in", cls: "num", cell: (p) => (lastUsed(p) ? day(lastUsed(p)) : "never") },
        { label: "Apps", cls: "num", cell: (p) => appsLink(p.apps) },
        { label: "Instances", cell: (p) => (p.admin ? el("span", { class: "muted", text: "Owner of all, as admin" }) : rolesText(p)) },
        { label: "", cls: "acts", cell: (p) => (meAdmin && !p.me
          ? button(p.admin ? "Make member" : "Make admin", () => setAdmin(ctx, p, !p.admin), { size: "sm" })
          : null) },
      ],
      expand: (p) => el("div", {}, dataTable({
        label: `Passkeys of ${p.name}`, noun: "passkeys", empty: "No passkeys registered.", signal: ctx.signal, load: p.passkeys,
        columns: [
          { label: "Passkey", cell: (k) => k.name || "Unnamed passkey" },
          { label: "Added", cls: "num", cell: (k) => day(k.created_at) },
          { label: "Last used", cls: "num", cell: (k) => (k.last_used_at ? day(k.last_used_at) : "never") },
          { label: "", cls: "acts", cell: (k) => (p.passkeys.length <= 1
            ? el("span", { class: "muted", text: "Only passkey" })
            : (p.me || meAdmin)
              ? button("Remove", () => removePasskey(ctx, p, k), { size: "sm", variant: "danger" })
              : null) },
        ],
      }), dataTable({
        label: `Roles of ${p.name}`, noun: "instances", empty: "No instances you can see.", signal: ctx.signal,
        load: instances.map((i) => ({ id: i, role: (p.roles || {})[i] || null })),
        columns: [
          { label: "Instance", cls: "mono", cell: (r) => r.id },
          { label: "Role", cell: (r) => (p.admin ? pill("Owner (admin)", "plain") : r.role ? pill(r.role, "plain") : el("span", { class: "muted", text: "None" })) },
          { label: "", cls: "acts", cell: (r) => (!p.admin && manage.includes(r.id)
            ? button("Change", () => setRole(ctx, p, r.id, r.role), { size: "sm" })
            : null) },
        ],
      })),
      load: async () => {
        const data = await ctx.op("app_people");
        meAdmin = data?.me_admin === true;
        instances = Array.isArray(data?.instances) ? data.instances : [];
        manage = Array.isArray(data?.manage) ? data.manage : [];
        actions.replaceChildren(meAdmin ? button("Invite person", () => invitePerson(ctx), { variant: "primary" }) : "");
        const apps = data?.apps || [];
        return (data?.people || []).map((p) => ({ ...p, passkeys: p.passkeys || [], me: p.id === data.me,
          apps: apps.filter((a) => a.person === p.id).length }))
          .sort((a, b) => Number(b.me) - Number(a.me));
      },
    });
    return el("div", { class: "view" },
      intro("Who can sign in to the platform and approve with a passkey, and their role on each instance: owners manage who belongs, operators write and approve, viewers read. Expand a person for their passkeys and roles. Admins invite and remove people, manage anyone's passkeys and are owners everywhere.",
        actions),
      table);
  }

  // ---- Apps: OAuth apps and service users, by the person each acts for; disconnect.
  const KIND = { app: "OAuth app", service: "Service user" };

  function disconnect(ctx, a) {
    confirm({
      title: "Disconnect app", danger: true,
      target: [["App", el("strong", { text: a.name })], ["Kind", KIND[a.kind] || a.kind], ["Acting for", a.personName || "No one"], ["Live tokens", String(a.tokens ?? 0)]],
      effect: "Revokes its tokens and its link, so it acts for no one. It must be allowed again with a passkey to reconnect. Proxy tokens a person issued stay.",
      action: "app-disconnect", id: a.id, confirmLabel: "Disconnect with passkey",
      onDone: refresh(ctx),
    });
  }

  function apps(ctx) {
    let meAdmin = false;
    return el("div", { class: "view" },
      intro("OAuth apps (MCP clients) and service users (agents) that act for a person. Disconnecting revokes their tokens."),
      dataTable({
        label: "Apps", noun: "apps", filter: "Filter apps", signal: ctx.signal,
        empty: "No apps yet. Add pigeon as a connector in Claude, then allow it with your passkey.",
        columns: [
          { label: "App", cell: (a) => [el("strong", { text: a.name }), el("span", { class: "sub mono", text: a.id })] },
          { label: "Kind", cell: (a) => tag(KIND[a.kind] || a.kind) },
          { label: "Acting for", cell: (a) => (a.personName ? a.personName : pill("No one", "warn")) },
          { label: "Tokens", cls: "num", cell: (a) => String(a.tokens ?? 0) },
          { label: "Linked", cls: "num", cell: (a) => day(a.linked_at) },
          { label: "", cls: "acts", cell: (a) => (meAdmin ? button("Disconnect", () => disconnect(ctx, a), { size: "sm", variant: "danger" }) : null) },
        ],
        load: async () => {
          const data = await ctx.op("app_people");
          meAdmin = data?.me_admin === true;
          const names = new Map((data?.people || []).map((p) => [p.id, p.name]));
          const all = (data?.apps || []).map((a) => ({ ...a, personName: a.person ? names.get(a.person) || a.person : null }));
          const none = all.filter((a) => !a.person);
          const some = all.filter((a) => a.person).sort((a, b) => a.personName.localeCompare(b.personName) || a.name.localeCompare(b.name));
          return [
            ...(none.length ? [{ group: "Acting for no one" }, ...none] : []),
            ...(some.length ? [{ group: "Acting for a person" }, ...some] : []),
          ];
        },
      }));
  }

  register("access", "secrets", secrets);
  register("access", "tokens", tokens);
  register("access", "people", people);
  register("access", "apps", apps);
})();
