/* Site: the views publishing and site_passkeys add to an instance (docs/plans/console-redesign.md).
 * - Sites: the sites on the approved record, read from instances_list.
 * - Deployments (Q1): the immutable releases from site_releases, live first; Publish is the
 *   site-deploy write and Rollback the site-rollback write, a destructive confirm.
 * - Visitors (P7): each passkey-protected hostname (app_site_hosts) with its visitors
 *   (app_site_visitors), loaded with the view; invite is visitor-invite, whose link is shown
 *   once, and Remove is visitor-revoke.
 * Every write goes through the shell's confirm dialog; every read through ctx.op. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, code, button, facts, field, input, dataTable, dialog, confirm, strip } = shell;

  const short = (s, n = 12) => String(s || "").slice(0, n);
  // A link to another view of the console, as the shell's own links behave.
  const go = (ctx, to, text) => el("a", { href: shell.href(to), text, on: { click: (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    ctx.nav(to);
  } } });
  // The zone of the platform's own subdomains: the console's host less its first label.
  const platformZone = () => (shell.fixtures ? "example.dev" : location.hostname.split(".").slice(1).join("."));
  async function recordOf(ctx) {
    const list = await ctx.op("instances_list");
    return (Array.isArray(list) ? list : []).find((r) => r.id === ctx.instance.id) || null;
  }
  const publishing = (r) => (r?.publishing?.state === "enabled" ? r.publishing.site : null);

  // ---- Site › Sites: the record's sites, the one publishing names first.
  function targetText(t) {
    if (t.kind === "subdomain") return `${t.label}.${platformZone()}`;
    if (t.kind === "preview") return "Preview";
    return t.hostname || "—";
  }
  function sites(ctx) {
    return el("div", { class: "view" },
      el("p", { class: "muted" }, "The sites on ", ctx.instance.title, "’s approved record. They change with the record, in ",
        go(ctx, { instance: ctx.instance.id, area: "overview" }, "Overview"), "; publish and roll back in ",
        go(ctx, { instance: ctx.instance.id, area: "site", view: "deployments" }, "Deployments"), "."),
      dataTable({
        label: "Sites", noun: "sites", signal: ctx.signal,
        empty: "This instance’s record has no sites yet.",
        columns: [
          { label: "Site", cell: (s) => [el("strong", { text: s.name }), s.published ? [" ", pill("Published", "accent")] : null,
            s.build ? el("span", { class: "sub mono", text: s.build }) : null] },
          { label: "Bucket", cell: (s) => [code(s.bucket), s.jurisdiction ? el("span", { class: "sub", text: `Jurisdiction ${s.jurisdiction}` }) : null] },
          { label: "Targets", cell: (s) => (s.targets?.length ? s.targets.map((t) => el("div", { class: "mono" }, targetText(t),
            t.kind === "zone" ? [" ", pill(t.enabled ? "Route on" : "Route off", t.enabled ? "ok" : "warn")] : null))
            : el("span", { class: "muted", text: "None" })) },
          { label: "Visitors", cell: (s) => (s.auth === "passkey"
            ? [pill("Passkey", "info"), el("span", { class: "sub", text: (s.allow || []).includes("site") ? "Invited visitors" : `${(s.allow || []).length} allowed` })]
            : pill("Public", "plain")) },
        ],
        load: async () => {
          const r = await recordOf(ctx);
          const live = publishing(r);
          return (r?.sites || []).map((s) => ({ ...s, published: s.name === live }))
            .sort((a, b) => Number(b.published) - Number(a.published));
        },
      }));
  }

  // ---- Site › Deployments (Q1): immutable releases, live first; Publish and Rollback.
  function deployments(ctx) {
    const id = ctx.instance.id;
    let site = null;
    let liveRelease = null;
    const about = el("p", { class: "muted", text: "Immutable releases of this instance’s site, live first." });
    const publish = button("Publish", () => publishDialog(), { variant: "primary" });
    const table = dataTable({
      label: "Releases", noun: "releases", filter: "Filter releases", signal: ctx.signal,
      empty: "No releases yet. Publish makes the first one.",
      columns: [
        { label: "Release", cell: (r) => [code(short(r.release, 20)), r.live ? [" ", pill("Live", "ok")] : null] },
        { label: "Commit", cls: "mono", cell: (r) => short(r.commit) || "—" },
        { label: "Template", cell: (r) => (r.template ? [code(r.template), r.template_version ? el("span", { class: "sub mono", text: r.template_version }) : null] : "—") },
        { label: "", cls: "acts", cell: (r) => (r.live ? el("span", { class: "muted", text: "Serving now" })
          : button("Roll back", () => rollback(r), { variant: "danger", size: "sm" })) },
      ],
      expand: (r) => facts([
        ["Release", code(r.release)],
        ["Commit", code(r.commit || "—")],
        r.template && ["Template", `${r.template} ${r.template_version || ""}`.trim()],
        r.base_url && ["Address", el("a", { href: r.base_url, text: r.base_url })],
      ]),
      load: async () => {
        const data = await ctx.op("site_releases", { instance: id });
        site = data?.site || null;
        liveRelease = data?.live || null;
        const rows = Array.isArray(data?.releases) ? data.releases : [];
        about.textContent = site
          ? `Immutable releases of ${site}, live first${data.total > rows.length ? ` (the newest ${rows.length} of ${data.total})` : ""}.`
          : "Immutable releases of this instance’s site, live first.";
        return rows;
      },
    });
    const after = () => setTimeout(() => table.reload(), 0);

    function rollback(r) {
      confirm({
        title: `Roll back ${site || "the site"}`,
        target: [["Instance", code(id)], ["Site", site || "—"], ["Release", code(r.release)], ["Commit", code(r.commit || "—")]],
        diff: [liveRelease && { op: "del", text: `live: ${liveRelease}` }, { op: "add", text: `live: ${r.release}` }].filter(Boolean),
        effect: "Visitors get this release as soon as it is live. Nothing is deleted: the release live now stays here, and you can roll forward to it the same way.",
        action: "site-rollback", id, change: { release: r.release }, danger: true,
        confirmLabel: "Roll back with passkey", onDone: after,
      });
    }
    function publishDialog() {
      const rev = input({ value: "main", mono: true, name: "rev", maxlength: 200, autocomplete: "off", spellcheck: "false" });
      const next = button("Review", () => review(), { variant: "primary" });
      const d = dialog({
        title: `Publish ${site || "the site"}`,
        body: [field({ label: "Revision", hint: "A commit or a branch. Publishing builds it as it is now.", control: rev })],
        actions: [button("Cancel", () => d.close()), next],
      });
      rev.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); review(); } });
      function review() {
        const value = rev.value.trim() || "main";
        if (!/^[A-Za-z0-9._/-]{1,200}$/.test(value)) return d.say("A revision is a commit or a branch name.", true);
        confirm({
          title: `Publish ${site || "the site"}`,
          target: [["Instance", code(id)], ["Site", site || "—"], ["Revision", code(value)]],
          diff: [{ op: "add", text: `new release built from ${value}` }, { op: "note", text: "live moves to the new release" }],
          effect: "Builds the site at this revision, keeps it as a new immutable release and makes it live. The release live now stays available to roll back to.",
          action: "site-deploy", id, change: { rev: value },
          confirmLabel: "Publish with passkey", onDone: after,
        });
      }
    }

    return el("div", { class: "view" }, el("div", { class: "toolbar" }, el("div", { class: "grow" }, about), publish), table);
  }

  // ---- Site › Visitors (P7): each protected hostname and its visitors, loaded with the view.
  function hostSection(ctx, { host, site }) {
    const target = { instance: ctx.instance.id, site, host };
    const closed = el("p", { class: "muted", hidden: true, text: "This hostname’s allowlist does not admit new visitors, so none can be invited." });
    const invite = button("Invite visitor", () => inviteDialog(), { size: "sm", disabled: true });
    const table = dataTable({
      label: `Visitors of ${host}`, noun: "visitors", signal: ctx.signal,
      empty: "No visitors invited for this hostname.",
      columns: [
        { label: "Visitor", cell: (v) => [el("strong", { text: v.name }), el("span", { class: "sub mono", text: v.id })] },
        { label: "Status", cell: (v) => (Number(v.enrolled) ? pill("Passkey registered", "ok") : pill("Invite pending", "warn")) },
        { label: "", cls: "acts", cell: (v) => button("Remove", () => revoke(v), { variant: "danger", size: "sm" }) },
      ],
      load: async () => {
        const data = await ctx.op("app_site_visitors", target);
        invite.disabled = !data?.can_invite;
        closed.hidden = Boolean(data?.can_invite);
        return Array.isArray(data?.visitors) ? data.visitors : [];
      },
    });
    const after = () => setTimeout(() => table.reload(), 0);
    const where = [["Hostname", code(host)], ["Site", site], ["Instance", code(ctx.instance.id)]];

    function revoke(v) {
      confirm({
        title: `Remove ${v.name}`,
        target: [["Visitor", v.name], ["Id", code(v.id)], ...where],
        diff: [{ op: "del", text: `visitor ${v.name} and their passkeys` }],
        effect: `${v.name} can no longer sign in to ${host}. To let them back, invite them again.`,
        action: "visitor-revoke", id: target, change: { user: v.id }, danger: true,
        confirmLabel: "Remove with passkey", onDone: after,
      });
    }
    function inviteDialog() {
      const name = input({ name: "visitor", maxlength: 64, autocomplete: "off", placeholder: "Their name" });
      const d = dialog({
        title: `Invite a visitor to ${host}`,
        body: [facts(where), field({ label: "Visitor name", hint: "Shown to you here; the visitor registers their own passkey.", control: name })],
        actions: [button("Cancel", () => d.close()), button("Review", () => review(), { variant: "primary" })],
      });
      name.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); review(); } });
      function review() {
        const who = name.value.trim();
        if (!who) { name.focus(); return d.say("Give the visitor a name.", true); }
        confirm({
          title: `Invite ${who}`,
          target: [["Visitor", who], ...where],
          diff: [{ op: "add", text: `visitor ${who}, invited` }],
          effect: "Makes a one-time link for them to register a passkey for this hostname. It is shown once, next.",
          action: "visitor-invite", id: target, change: { name: who },
          confirmLabel: "Create invite with passkey", onDone: after,
        });
      }
    }

    return el("section", { class: "section" },
      el("div", { class: "section-head" }, el("h2", { class: "mono", text: host }), el("span", { class: "muted", text: site }),
        el("span", { class: "grow" }), invite),
      closed, table);
  }

  function visitors(ctx) {
    const out = el("div", { class: "view" }, el("p", { class: "muted", role: "status", text: "Loading protected hostnames…" }));
    const capabilities = { instance: ctx.instance.id, area: "capabilities" };
    const load = async () => {
      try {
        const data = await ctx.op("app_site_hosts", { instance: ctx.instance.id });
        if (ctx.signal.aborted) return;
        const hosts = Array.isArray(data?.hosts) ? data.hosts : [];
        out.replaceChildren(...(hosts.length ? hosts.map((h) => hostSection(ctx, h))
          : [el("div", { class: "notice" }, el("strong", { text: "No protected hostnames yet." }),
            el("p", { class: "ink2", text: "A hostname appears here once a site gated by visitor passkeys is published on a platform subdomain." }))]),
        el("p", { class: "muted" }, "Visitor passkeys come from the site_passkeys capability; change it in ", go(ctx, capabilities, "Capabilities"), "."));
      } catch (err) {
        if (ctx.signal.aborted) return;
        out.replaceChildren(strip("bad", el("span", { class: "grow", text: `Couldn’t load the protected hostnames: ${err.message || err}` }),
          button("Retry", () => { out.replaceChildren(el("p", { class: "muted", role: "status", text: "Loading protected hostnames…" })); load(); }, { size: "sm" })));
      }
    };
    load();
    return out;
  }

  register("site", "sites", sites);
  register("site", "deployments", deployments);
  register("site", "visitors", visitors);
})();
