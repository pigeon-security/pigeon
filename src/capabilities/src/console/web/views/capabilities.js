/* The instance record's two views (docs/plans/console-redesign.md, M1, M2, C2):
 * - Overview: the record's facts, the Cloudflare link (Link, or Unlink through linked-unlink),
 *   and an editor for the fields that are not capabilities (title, zones, repository, Worker,
 *   sites), as a form or as JSON, which shows its diff and applies it through instance-put.
 * - Capabilities: the one place capabilities change. Enable, Configure, Upgrade and Disable each
 *   make one diff of the record, applied through instance-put with the passkey; Upgrade moves a
 *   pin (Q2), as capabilities_upgrade_plan suggests.
 * Both read the record fresh from instances_list, so the version they edit from is current. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, tag, code, button, facts, panel, segmented, input, field, diffBlock, dataTable, dialog, confirm, strip, toast, sha256 } = shell;

  const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "—");
  const show = (v) => (v === undefined || v === null ? "none" : JSON.stringify(v));
  const clone = (v) => JSON.parse(JSON.stringify(v));
  // The record's layout (docs/plans/record-reshape.md): each setting at a dotted path, such as
  // `site.publishing` or `data.email`. `setAt` makes the objects on the way; `delAt` removes
  // the value and every namespace it leaves empty (`site`, `network`, `cloud`, `instance`,
  // `repo` when only its `source` was left), as the platform never writes an empty one.
  const getAt = (r, path) => path.split(".").reduce((o, k) => (o == null ? undefined : o[k]), r);
  function setAt(r, path, value) {
    const keys = path.split(".");
    const last = keys.pop();
    let o = r;
    for (const k of keys) {
      if (!o[k] || typeof o[k] !== "object" || Array.isArray(o[k])) o[k] = {};
      o = o[k];
    }
    o[last] = value;
  }
  const NAMESPACES = new Set(["site", "network", "cloud", "instance", "capabilities"]);
  function delAt(r, path) {
    const keys = path.split(".");
    const chain = [r];
    for (const k of keys.slice(0, -1)) {
      const o = chain[chain.length - 1]?.[k];
      if (!o || typeof o !== "object") return;
      chain.push(o);
    }
    delete chain[chain.length - 1][keys[keys.length - 1]];
    for (let i = keys.length - 2; i >= 0; i--) {
      const parent = chain[i];
      const k = keys[i];
      const o = parent[k];
      if (!o || Object.keys(o).length) break;
      if (NAMESPACES.has(k) && i === 0) delete parent[k];
      else if (i > 0) delete parent[k];
    }
    if (r.repo && typeof r.repo === "object" && !Object.keys(r.repo).length) r.repo = null;
  }
  const go = (ctx, to, text) => el("a", { href: shell.href(to), text, on: { click: (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    ctx.nav(to);
  } } });
  async function recordOf(ctx) {
    const list = await ctx.op("instances_list");
    return (Array.isArray(list) ? list : []).find((r) => r.id === ctx.instance.id) || null;
  }

  // ---- The record diff: what the person approves, path by path, as the platform diffs it.
  const plain = (v) => v && typeof v === "object" && !Array.isArray(v);
  function changes(from, to, path = "") {
    if (plain(from) && plain(to)) {
      const keys = [...new Set([...Object.keys(from), ...Object.keys(to)])].filter((k) => path || k !== "version").sort();
      return keys.flatMap((k) => changes(from[k], to[k], path ? `${path}.${k}` : k));
    }
    // Absent, null, false and empty are the same to the record (serde's defaults), except a
    // capability's own entry: `data = {}` turns it on with every setting at its default, as
    // record::diff counts it.
    const entry = /^capabilities\.[^.]+$/.test(path) || ENTRY_PATHS.has(path);
    const none = (v) => v == null || v === false || (Array.isArray(v) && !v.length) || (plain(v) && !Object.keys(v).length && !entry);
    const a = none(from) ? null : from;
    const b = none(to) ? null : to;
    return JSON.stringify(a) === JSON.stringify(b) ? [] : [{ path, from: a, to: b }];
  }
  function diffLines(record, list) {
    return [{ op: "note", text: `record version ${record.version ?? 0} → ${(record.version ?? 0) + 1}` },
      ...list.flatMap((c) => [c.from !== null && { op: "del", text: `${c.path}: ${show(c.from)}` },
        c.to !== null && { op: "add", text: `${c.path}: ${show(c.to)}` }].filter(Boolean))];
  }
  // One record write: the exact diff, then the passkey, through instance-put.
  function putRecord(ctx, { record, next, title, target = [], effect, danger = false, confirmLabel, jobsAudit }) {
    const list = changes(record, next);
    if (!list.length) return toast("Nothing changes.", "info");
    confirm({
      title, danger, confirmLabel,
      target: [["Instance", code(record.id)], ...target],
      diff: diffLines(record, list), effect,
      action: "instance-put", id: record.id, change: { record: JSON.stringify(next), ...(jobsAudit ? { jobs_audit: jobsAudit } : {}) },
      onDone: () => setTimeout(ctx.refresh, 0),
    });
  }

  // ---- Instance capabilities: the record field each one is enabled by, and what it adds.
  const list = (s) => String(s || "").split(",").map((x) => x.trim()).filter(Boolean);
  // The catalogue capabilities' own entries (`data`, `data.email`, …), filled from the catalogue.
  const ENTRY_PATHS = new Set();
  // Where catalogue capability `name` sits: its entry's `field`, or `capabilities.<name>`.
  let FIELDS = {};
  const fieldOf = (name) => FIELDS[name] || `capabilities.${name}`;
  // The sites gated by passkeys: each one `site.passkeys` names, with its `allow`.
  const passkeySites = (r) => Object.entries(r.site?.passkeys || {}).map(([name, p]) => ({ name, allow: p?.allow || [] }));
  const on = (r, c) => (c.on ? c.on(r) : c.cap === "site_passkeys" ? passkeySites(r).length > 0 && on(r, CAPS[0]) : getAt(r, c.field)?.state === "enabled");
  const CAPS = [
    { cap: "publishing", field: "site.publishing", component: "sites", adds: ["Site › Sites", "Site › Deployments"],
      what: "Publish one of the instance’s sites from its repository",
      needs: (r) => ((r.sites || []).length ? null : "a site in the record"),
      fields: (r) => [{ key: "site", label: "Site", options: (r.sites || []).map((s) => s.name), hint: "The site that Publish and Roll back act on." }] },
    { cap: "site_passkeys", field: "site.passkeys", component: "site_passkeys", adds: ["Site › Visitors"],
      what: "Gate a site’s platform subdomains behind visitor passkeys",
      needs: (r) => (on(r, CAPS[0]) ? null : "publishing"),
      fields: (r) => [
        { key: "site", label: "Site", options: (r.sites || []).map((s) => s.name), hint: "Only a site on platform subdomains can be gated." },
        { key: "allow", label: "Who may sign in", list: true, hint: "Comma-separated user ids, or site for every visitor you invite." },
      ],
      current: (r) => { const s = passkeySites(r)[0]; return s ? { site: s.name, allow: s.allow } : { site: r.site?.publishing?.site, allow: ["site"] }; },
      apply: (r, v) => setAt(r, "site.passkeys", { [v.site]: { allow: v.allow } }),
      remove: (r) => delAt(r, "site.passkeys") },
    { cap: "data-read", field: "data.read", component: "data", adds: ["Storage › Buckets"],
      what: "Read the objects of one approved bucket, beside the data store",
      needs: (r) => (r.data ? null : "data"),
      fields: () => [
        { key: "store", label: "Storage account", mono: true, hint: "A declared account, such as scaleway:archive." },
        { key: "bucket", label: "Bucket", mono: true },
        { key: "prefix", label: "Prefix", mono: true, optional: true, hint: "Ends with /. Empty for the whole bucket." },
      ] },
    { cap: "source", field: "repo.source", component: "source", adds: ["Source › Repository"],
      what: "Read the record’s repository", needs: (r) => (r.repo?.kind ? null : "a repository in the record"), fields: () => [] },
    { cap: "ci", field: "ci", component: "ci", adds: ["CI › Jobs"],
      what: "See CI jobs of the record’s repository",
      needs: (r) => (r.account?.kind === "home" && r.repo?.kind === "artifacts" ? null : "a home-account Artifacts repository"),
      fields: () => [{ key: "jobs", label: "Job names", list: true, mono: true, hint: "Comma-separated, 1 to 64." }] },
    { cap: "observability", field: "observability", component: "observability", adds: ["Metrics › Workers"],
      what: "Read metrics of the instance’s Worker", needs: (r) => (r.worker ? null : "a Worker in the record"), fields: () => [] },
    // A list on the record: Configure adds the redirect for a host, or replaces the one it has.
    { cap: "redirects", field: "network.redirects", component: "instances", adds: [],
      what: "Answer a hostname in the instance’s zones with a redirect to another URL",
      effect: "Its plan attaches the host to the instance’s Worker, which answers it with the redirect.",
      on: (r) => (r.network?.redirects || []).length > 0,
      needs: (r) => (!(r.zones || []).length ? "a zone in the record" : r.worker ? null : "a Worker in the record"),
      fields: (r) => [
        { key: "host", label: "Host", mono: true, hint: `A hostname in ${(r.zones || []).join(", ") || "the instance’s zones"}. An existing host’s redirect is replaced.` },
        { key: "to", label: "Redirect to", mono: true, hint: "An https:// URL, with no query or fragment." },
        { key: "status", label: "Status", options: ["301", "302", "307", "308"], hint: "301 and 308 are permanent; 302 and 307 temporary." },
        { key: "keep_path", label: "Keep path", options: ["yes", "no"], hint: "Append the request’s path and query to the URL." },
      ],
      current: (r) => {
        const x = (r.network?.redirects || [])[0];
        return x ? { host: x.host, to: x.to, status: String(x.status ?? 301), keep_path: x.keep_path === false ? "no" : "yes" }
          : { host: (r.zones || [])[0] || "", to: "", status: "301", keep_path: "yes" };
      },
      apply: (r, v) => {
        const entry = { host: v.host.toLowerCase(), to: v.to };
        if (v.status !== "301") entry.status = Number(v.status);
        if (v.keep_path === "no") entry.keep_path = false;
        setAt(r, "network.redirects", [...(r.network?.redirects || []).filter((x) => x.host !== entry.host), entry]);
      },
      remove: (r) => delAt(r, "network.redirects") },
  ];
  // Catalogue capabilities, each at its entry's `field` (`data.email`, `cloud.gateway`, …; else
  // `capabilities.<name>`): linked entries on linked instances and opt-in entries on home.
  // Placement is stored in the capability settings. Disabling removes the entry and its
  // dependents.
  const CATALOGUE_ADDS = {
    architecture: { adds: ["Platform › Architecture"], what: "Show this instance’s own setup: its Worker, bucket, hostnames and tools" },
    "data-ingest": {
      adds: ["Ingest › Runs"], what: "Import approved source data into the instance's data store",
      effect: "Review the Spaces scope and estimated cost, then approve the ingest run with a passkey.",
    },
    "data-ingest-email": {
      what: "Enable read-only IMAP backfill",
      effect: "The next Data plan authorizes signed historical imports with a dedicated key.",
    },
    "mcp-federation": { what: "Share approved Jobs reads through the platform MCP connection" },
    jobs: {
      adds: ["Jobs › Runs"], what: "Inspect this instance’s job history",
      effect: "Shows instance-owned job history and exports terminal audit records to one private R2 bucket.",
    },
    cloud: { what: "Enable Cloud Gateway status for this instance", effect: "Adds cloud_status to the instance’s MCP endpoint." },
    digitalocean: {
      what: "Connect the instance to one DigitalOcean team",
      effect: "Approves the expected team for this instance. Set and verify the token separately with a passkey.",
    },
    data: {
      what: "Keep this instance’s data store (an R2 bucket, a D1 database and the Worker that runs it)",
      effect: "The next instance plan creates them; Separate Worker runs it as {instance}-data, Inline inside the instance’s own Worker.",
    },
    "data-email": {
      adds: ["Email › Mailboxes"], what: "Receive every address of the instance’s zone into its data store",
      effect: "The next instance plan shows each DNS record Email Routing adds or removes before your passkey.",
    },
    secrets: { adds: ["Access › Secrets"], what: "List the secrets in this instance’s own Cloudflare account (its Secrets Store)" },
  };
  function catalogueCaps(versions, home) {
    FIELDS = Object.fromEntries((versions?.catalogue || []).filter((e) => e.field).map((e) => [e.name, e.field]));
    for (const f of Object.values(FIELDS)) ENTRY_PATHS.add(f);
    // A capability's own settings, without the capabilities that sit inside it (`data.email`
    // inside `data`).
    const children = (name) => Object.values(FIELDS).filter((f) => f.startsWith(`${fieldOf(name)}.`)).map((f) => f.slice(fieldOf(name).length + 1));
    const own = (r, name) => {
      const s = { ...getAt(r, fieldOf(name)) };
      for (const k of children(name)) delete s[k];
      if (name === "data") delete s.read;
      return s;
    };
    return (versions?.catalogue || []).filter((e) => (home ? e.opt_in : e.linked) && !e.always && e.name !== "redirects").map((e) => {
      const digitalOcean = e.name === "digitalocean";
      const jobs = e.name === "jobs" && !home;
      // Email runs where the data store runs (docs/plans/linked-data.md): only `data` chooses.
      const configurable = !home && !["data-email", "data-ingest-email"].includes(e.name) && (e.placements || []).includes("own_worker");
      const choices = versions.linked_placement_choices?.[e.name] || ["inline"];
      // The data store's own Worker is its default on a linked instance; every other entry's is inline.
      const placementDefault = e.name === "data" ? "own_worker" : "inline";
      return {
        cap: e.name, field: fieldOf(e.name), component: e.name,
        adds: CATALOGUE_ADDS[e.name]?.adds || [], what: CATALOGUE_ADDS[e.name]?.what || "", effect: CATALOGUE_ADDS[e.name]?.effect,
        on: (r) => Boolean(getAt(r, fieldOf(e.name))),
        needs: (r) => { if (jobs && !r.account?.account_id) return "a linked Cloudflare account"; const lack = (e.requires || []).filter((q) => !getAt(r, fieldOf(q))); return lack.length ? lack.join(", ") : null; },
        current: (r) => {
          const settings = own(r, e.name);
          return { placement: placementDefault, ...settings, ...(digitalOcean ? {
            spaces_region: settings?.spaces?.region, spaces_bucket: settings?.spaces?.bucket,
            spaces_prefix: settings?.spaces?.prefix,
            droplet_health: settings?.droplet_health ? "yes" : "no",
          } : {}) };
        },
        fields: () => [
          ...(digitalOcean ? [{ key: "team_id", label: "DigitalOcean team ID", mono: true,
            hint: "Verification checks that the token belongs to this team.",
            validate: (value) => /^[A-Za-z0-9_-]{1,128}$/.test(value) },
          { key: "spaces_region", label: "Spaces region", optional: true, mono: true,
            hint: "For example, nyc3. Leave region and bucket blank to keep Spaces off.",
            validate: (value, values) => value ? /^[a-z]{2,8}[0-9]{1,2}$/.test(value) : !values.spaces_bucket },
          { key: "spaces_bucket", label: "Spaces bucket", optional: true, mono: true,
            validate: (value, values) => value ? /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/.test(value) : !(values.spaces_region || values.spaces_prefix) },
          { key: "spaces_prefix", label: "Spaces prefix", optional: true, mono: true,
            hint: "Leave blank to check the bucket root.",
            validate: (value) => new TextEncoder().encode(value).length <= 1024 && !/[\u0000-\u001f\u007f-\u009f]/u.test(value) }] : []),
          ...(digitalOcean ? [{ key: "droplet_health", label: "Droplet runner checks",
            options: ["no", "yes"], labels: { no: "Off", yes: "On" },
            hint: "Each runner check needs a separate passkey approval before compute starts." }] : []),
          ...(configurable ? [{
            key: "placement", label: "Worker placement", options: ["inline", "own_worker"],
            labels: { inline: "Inline", own_worker: "Separate Worker" },
            disabled: (option) => !choices.includes(option),
            hint: choices.includes("own_worker") ? undefined : "Separate Worker requires a linked-account build for this capability.",
          }] : []),
        ],
        apply: (r, v) => {
          // The whole entry, its children (`data.email` inside `data`) kept as they are.
          const settings = { ...getAt(r, fieldOf(e.name)) };
          if (jobs) settings.audit_bucket = v.audit_bucket;
          if (configurable) settings.placement = v.placement;
          if (digitalOcean) {
            settings.team_id = v.team_id;
            if (v.droplet_health === "yes") settings.droplet_health = true;
            else delete settings.droplet_health;
            if (v.spaces_region || v.spaces_bucket || v.spaces_prefix) {
              settings.spaces = { region: v.spaces_region, bucket: v.spaces_bucket, ...(v.spaces_prefix ? { prefix: v.spaces_prefix } : {}) };
            } else delete settings.spaces;
          }
          setAt(r, fieldOf(e.name), settings);
        },
        remove: (r) => {
          for (const other of versions.catalogue) if ((other.requires || []).includes(e.name)) delAt(r, fieldOf(other.name));
          delAt(r, fieldOf(e.name));
        },
      };
    });
  }
  const profileOf = (r, c) => (c.current ? c.current(r) : getAt(r, c.field) || {});
  // The record with capability `c` set to `values`, or without it (and what needs it).
  function withCap(r, c, values) {
    const next = clone(r);
    if (c.apply) c.apply(next, values);
    else setAt(next, c.field, { state: "enabled", ...Object.fromEntries(Object.entries(values).filter(([, v]) => v !== "" && !(Array.isArray(v) && !v.length))) });
    return next;
  }
  function withoutCap(r, c) {
    const next = clone(r);
    for (const d of [c, ...dependents(r, c)]) {
      if (d.remove) d.remove(next);
      else delAt(next, d.field);
    }
    return next;
  }
  const dependents = (r, c) => (c.cap === "publishing" && on(r, CAPS[1]) ? [CAPS[1]] : []);

  function ingestBucketConnections(ctx, record) {
    const content = el("div", { class: "stack" });
    const d = dialog({ title: "Bucket connections", body: [content] });
    const states = {
      configured: "Credential saved",
      scope_changed: "Scope changed. Replace the credential.",
      legacy_spaces_key: "Using the existing Spaces credential",
      missing: "Credential required",
    };
    ctx.op("data_ingest_connections", { instance: record.id }).then((result) => {
      if (!result.connections.length) {
        content.append(el("p", { text: "Add named connections under data.ingest.connections in the instance record." }));
        return;
      }
      for (const connection of result.connections) {
        const { name, binding, state, generation } = connection;
        const target = [
          ["Instance", code(record.id)],
          ["Connection", code(name)],
          ["Source", code([binding.provider, binding.region, binding.bucket, binding.prefix].filter(Boolean).join(" / "))],
        ];
        const verify = button("Verify", async () => {
          try {
            await ctx.op("data_ingest_connection_verify", { instance: record.id, connection: name });
            d.say(`Verified read access for ${name}.`);
          } catch (err) {
            d.say(err.message || String(err), true);
          }
        });
        verify.disabled = state !== "configured" && state !== "legacy_spaces_key";
        const set = button(state === "configured" ? "Rotate key" : "Set key", () => {
          const value = input({ type: "password", mono: true, autocomplete: "off", spellcheck: "false" });
          confirm({
            title: `Set key for ${name}`, target,
            form: field({ label: "Access key ID and secret", control: value,
              hint: "Enter ACCESS_KEY_ID:SECRET_ACCESS_KEY for a read-only key limited to the bucket." }),
            action: "data-ingest-connection-key-set", id: { instance: record.id, name, version: result.version, generation },
            confirmLabel: "Verify and save with passkey",
            prepare: async () => {
              const secret = value.value.trim();
              if (!secret) throw new Error("Enter the bucket credential.");
              return { change: { value: secret }, begin: { digest: await sha256(secret) } };
            },
            onDone: () => setTimeout(ctx.refresh, 0),
          });
        });
        content.append(facts(target), el("p", { class: "muted", text: states[state] || "Credential status unavailable" }), el("div", { class: "actions" }, [verify, set]));
      }
    }).catch((err) => d.say(err.message || String(err), true));
  }

  function digitalOceanConnection(ctx, record, spaces = false) {
    const args = { instance: record.id };
    const teamId = record.cloud.digitalocean.team_id;
    const target = [["Instance", code(record.id)], ["Expected team", code(teamId)]];
    const source = record.cloud.digitalocean.spaces;
    if (spaces) target.push(["Source", code(`${source.region} / ${source.bucket} / ${source.prefix || ""}`)]);
    const credential = spaces ? "Spaces key" : "token";
    const status = el("p", { class: "muted", role: "status", text: "Checking connection…" });
    const verify = button(`Verify ${credential}`, verifyToken);
    const token = button(`Set ${credential}`, () => {
      const value = input({ type: "password", mono: true, autocomplete: "off", spellcheck: "false", "aria-label": spaces ? "Spaces access key and secret" : "DigitalOcean API token" });
      confirm({
        title: spaces ? "Set Spaces key" : "Set DigitalOcean token", target,
        form: field({ label: spaces ? "Access key ID and secret" : "API token", control: value,
          hint: spaces ? "Enter ACCESS_KEY_ID:SECRET_ACCESS_KEY for a read-only key limited to this bucket."
            : "Use account:read for team verification. Add spaces_key:read if you will verify a Spaces key." }),
        effect: spaces ? "Pigeon verifies the key’s bucket permissions and checks a bounded listing before saving it."
          : "Pigeon verifies the token against the approved team before saving it.",
        action: spaces ? "digitalocean-spaces-key-set" : "digitalocean-token-set", id: { instance: record.id, team_id: teamId, version: record.version },
        confirmLabel: "Verify and save with passkey",
        prepare: async () => {
          const secret = value.value.trim();
          if (!secret) throw new Error(`Enter the ${spaces ? "Spaces access key and secret" : "DigitalOcean API token"}.`);
          return { change: { value: secret }, begin: { digest: await sha256(secret) } };
        },
        onDone: () => setTimeout(ctx.refresh, 0),
      });
    });
    verify.disabled = token.disabled = true;
    const d = dialog({
      title: spaces ? "DigitalOcean Spaces" : "DigitalOcean connection",
      body: [facts(target), status],
      actions: [verify, token],
    });
    const showError = (err) => d.say(err.message || String(err), true);
    ctx.op(spaces ? "digitalocean_spaces_status" : "digitalocean_status", args).then((result) => {
      status.textContent = result.state === "connected" ? `Saved for the approved ${spaces ? "bucket" : "team"}.`
        : result.state === "unverified" ? `Replace the saved ${credential} to verify this ${spaces ? "source" : "team"}.` : `No ${credential} is saved for the approved ${spaces ? "bucket" : "team"}.`;
      token.textContent = `${result.state === "missing" ? "Set" : "Replace"} ${credential}`;
      verify.disabled = result.state !== "connected";
      token.disabled = false;
    }).catch((err) => {
      status.textContent = "Connection status unavailable.";
      showError(err);
    });
    async function verifyToken() {
      verify.disabled = token.disabled = true;
      d.say("Verifying…");
      try {
        const result = await ctx.op(spaces ? "digitalocean_spaces_verify" : "digitalocean_verify", args);
        if (result.verified !== true) throw new Error("DigitalOcean verification did not complete.");
        d.say(spaces ? "Verified read access to the approved Spaces source." : "Verified against the approved DigitalOcean team.");
      } catch (err) {
        showError(err);
      } finally {
        verify.disabled = token.disabled = false;
      }
    }
  }

  function digitalOceanDroplet(ctx, record) {
    const team = record.cloud.digitalocean.team_id;
    const id = { instance: record.id, team_id: team, version: record.version };
    const target = [["Instance", code(record.id)], ["Expected team", code(team)]];
    const details = el("div", { role: "status" });
    let plan = null;
    let run = null;
    const setToken = button("Set compute token", () => {
      const value = input({ type: "password", mono: true, autocomplete: "off", spellcheck: "false" });
      confirm({
        title: "Set DigitalOcean compute token", target,
        form: field({ label: "Compute token", control: value, hint: "Use a separate API token with account:read, droplet:create, droplet:read, droplet:delete, and tag:create." }),
        effect: "Pigeon verifies the token against the approved team before saving it.",
        action: "digitalocean-droplet-token-set", id, confirmLabel: "Verify and save with passkey",
        prepare: async () => {
          const secret = value.value.trim();
          if (!secret) throw new Error("Enter the DigitalOcean compute token.");
          return { change: { value: secret }, begin: { digest: await sha256(secret) } };
        },
        onDone: () => refresh(),
      });
    });
    const start = button("Review runner check", () => {
      if (!plan?.fingerprint) return;
      confirm({
        title: "Start temporary Droplet health check",
        target: [...target, ...planFacts(plan)],
        effect: "Creates one billable Droplet and authorizes its cleanup. The runner has no Spaces or R2 access.",
        action: "digitalocean-droplet-start", id, change: { plan: plan.fingerprint }, confirmLabel: "Start with passkey",
        onDone: () => refresh(),
      });
    }, { variant: "primary" });
    const cancel = button("Cancel runner", () => {
      if (!run || !["creating", "waiting"].includes(run.state)) return;
      confirm({
        title: "Cancel DigitalOcean runner check",
        target: [...target, ["Run", code(run.id)]],
        effect: "Stops this runner check and requests cleanup of its Droplet.",
        action: "digitalocean-droplet-cancel", id, change: { run: run.id },
        confirmLabel: "Cancel runner with passkey", onDone: () => refresh(),
      });
    });
    const refreshButton = button("Refresh", () => refresh());
    const d = dialog({
      title: "DigitalOcean runner check",
      body: [facts(target), details],
      actions: [refreshButton, setToken, cancel, start],
    });
    function planFacts(p) {
      return [["Region", code(p.region)], ["Size", code(p.size)], ["Droplet image", code(p.image)],
        ["Runner commit", code(p.commit)], ["Runtime limit", `${p.max_minutes} minutes`],
        ["Estimated compute cost", `$${p.estimated_usd} USD`]];
    }
    async function refresh() {
      refreshButton.disabled = setToken.disabled = cancel.disabled = start.disabled = true;
      d.say("Checking runner status…");
      try {
        const result = await ctx.op("digitalocean_droplet_status", { instance: record.id });
        plan = result.plan || null;
        run = result.run;
        details.replaceChildren(
          el("p", { class: "muted", text: result.credential === "configured"
            ? "Compute token saved for this team." : "Set a compute token for the approved team." }),
          ...(plan ? [facts(planFacts(plan))] : []),
          ...(run ? [facts([["Run", code(run.id)], ["State", run.state],
            ...(run.health ? [["Health", run.health]] : []),
            ...(run.error ? [["Detail", run.error]] : [])])] : []),
        );
        setToken.disabled = false;
        cancel.disabled = !run || !["creating", "waiting"].includes(run.state);
        start.disabled = !plan || result.can_start !== true;
        d.say("");
      } catch (err) {
        d.say(err.message || String(err), true);
      } finally {
        refreshButton.disabled = false;
      }
    }
    refresh();
  }

  // ---- Capabilities (M2).
  function capabilities(ctx) {
    const inst = ctx.instance;
    const gated = new Map();
    for (const a of ctx.manifest.areas || []) {
      for (const v of a.views || []) if (v.upgrade && !gated.has(v.capability)) gated.set(v.capability, v);
    }
    let state = null;
    // Internal plumbing (catalogue::PLUMBING: dispatch, the console itself, OAuth…) stays folded
    // under its heading until Show; `redraw` re-renders from `state` without reading again.
    let internal = false;
    let redraw = false;
    const table = dataTable({
      label: "Capabilities", noun: "capabilities", signal: ctx.signal,
      empty: "This instance has no record to change.",
      columns: [
        { label: "Capability", cell: (row) => [el("strong", { class: "mono", text: row.cap }), el("span", { class: "sub", text: row.what })] },
        { label: "Adds to the console", cell: (row) => (row.adds?.length ? row.adds.map((t) => tag(t)) : el("span", { class: "muted", text: "—" })) },
        { label: "Runs", cell: (row) => row.runs || el("span", { class: "muted", text: "—" }) },
        { label: "Record field", cls: "mono", cell: (row) => row.field || "—" },
        { label: "Pin → selected", cell: (row) => row.pinText },
        { label: "Latest", cls: "mono", cell: (row) => row.latest || "—" },
        { label: "Status", cell: (row) => row.status },
        { label: "", cls: "acts", cell: (row) => row.acts },
      ],
      load: async () => {
        if (redraw && state) {
          redraw = false;
          return rows(state);
        }
        const [record, versions, plan] = await Promise.all([
          recordOf(ctx),
          ctx.op("capabilities_versions", { instance: inst.id }).catch(() => null),
          ctx.op("capabilities_upgrade_plan", { instance: inst.id }).catch(() => null),
        ]);
        if (!record) return [];
        state = { record, versions, plan };
        return rows(state);
      },
    });

    function rows({ record, versions, plan }) {
      const comps = new Map((versions?.components || []).map((c) => [c.name, c]));
      // Where each catalogue capability runs here (catalogue::placements, M3): on home what is
      // deployed, read-only; on a linked instance inline, in the instance's own Worker.
      const placed = (versions?.instances || []).find((i) => i.id === record.id)?.placements || {};
      const runs = (name) => {
        const p = placed[name];
        if (!p) return null;
        const where = p.placement === "own_worker" ? "Own Worker" : "Inline";
        return [pill(where, p.placement === "own_worker" ? "info" : "plain"), p.worker ? el("span", { class: "sub mono", text: p.worker }) : null,
          p.read_only ? el("span", { class: "sub muted", text: "read-only: moves land through code" }) : null];
      };
      const pins = new Map(((versions?.instances || []).find((i) => i.id === record.id)?.pins || []).map((p) => [p.component, p]));
      const steps = new Map(((plan?.instances || []).find((i) => i.id === record.id)?.steps || []).map((s) => [s.component, s]));
      const row = (c) => {
        const enabled = on(record, c);
        const latest = comps.get(c.component)?.version || null;
        const pin = pins.get(c.component);
        const step = steps.get(c.component);
        const view = gated.get(c.cap);
        const sm = { size: "sm" };
        let status;
        let acts;
        if (enabled) {
          const major = (v) => Number(String(v || "0").split(".")[0]);
          status = view ? pill(`Upgrade to see ${view.title}`, "warn")
            : pin?.state === "held" ? pill("Held by its pin", "bad")
              : step ? pill(major(step.to) > major(step.from) ? "Major upgrade" : "Update available", "info") : pill("Current", "ok");
          if (c.cap === "jobs" && !record.jobs.audit_bucket) status = pill("Audit setup required", "warn");
          acts = [
            c.cap === "jobs" && !record.jobs.audit_bucket
              ? button("Set up audit storage", () => configureJobs(c, true), sm) : null,
            step ? button("Upgrade", () => upgrade(c, step), { ...sm, variant: view ? "primary" : undefined }) : null,
            c.fields(record).length || c.cap === "mcp-federation"
              ? button(c.cap === "mcp-federation" ? "Manage trust" : "Configure", () => configure(c, true), { ...sm, variant: "ghost" }) : null,
            c.cap === "data-ingest" ? button("Bucket connections", () => ingestBucketConnections(ctx, record), sm) : null,
            c.cap === "digitalocean" ? button("Connection", () => digitalOceanConnection(ctx, record), sm) : null,
            c.cap === "digitalocean" && record.cloud.digitalocean.droplet_health
              ? button("Runner check", () => digitalOceanDroplet(ctx, record), sm) : null,
            c.cap === "digitalocean" && record.cloud.digitalocean.spaces
              ? button("Spaces", () => digitalOceanConnection(ctx, record, true), sm) : null,
            button("Disable", () => disable(c), { ...sm, variant: "danger" }),
          ];
        } else {
          const need = c.needs?.(record);
          status = pill(record[c.field]?.state === "disabled" ? "Disabled" : "Off", "plain");
          acts = need ? el("span", { class: "muted", text: `Needs ${need}` }) : button("Enable", () => configure(c, false), sm);
        }
        const pinText = !enabled ? el("span", { class: "muted", text: "—" })
          : pin ? [code(pin.requirement), " → ", code(pin.selected || "none")]
            : [el("span", { class: "muted", text: "unpinned → " }), code(latest || "newest")];
        return { ...c, enabled, latest, status, acts, pinText, runs: enabled ? runs(c.cap) : null };
      };
      const offered = [...CAPS, ...catalogueCaps(versions, inst.home)];
      const all = offered.map(row);
      const yes = all.filter((r) => r.enabled);
      const no = all.filter((r) => !r.enabled);
      const out = [{ group: `Enabled on ${inst.title} (${yes.length})` }, ...yes, { group: `Available (${no.length})` }, ...no];
      // The catalogue's built-in capabilities (docs/plans/platform-capabilities.md): those not
      // offered above, each with its `default`. On home they are on and read-only: `always` ones
      // (home-only platform capabilities and `instances`) are never listed in a record, `on`
      // ones need no listing there. On a linked instance the home-only ones are named, so where
      // each capability may run is plain.
      const shown = new Set(offered.flatMap((c) => [c.cap, c.component]));
      const builtIn = (versions?.catalogue || []).filter((e) => !shown.has(e.name));
      const builtInRow = (e, status, note) => ({
        cap: e.name, what: e.about || "", adds: [], field: e.field || (e.default === "always" ? "— (never listed)" : null),
        latest: comps.get(e.name)?.version || null, runs: runs(e.name),
        pinText: comps.get(e.name) ? code(comps.get(e.name).version) : el("span", { class: "muted", text: "—" }),
        status, acts: el("span", { class: "muted", text: note }),
      });
      if (inst.home) {
        const always = builtIn.filter((e) => e.default === "always" && placed[e.name]);
        const byDefault = builtIn.filter((e) => e.default === "on" && placed[e.name]);
        if (always.length) {
          out.push({ group: `Always on · the platform’s own (${always.length})` },
            ...always.map((e) => builtInRow(e, pill("Always on", "ok"), e.availability === "home_only" ? "Home only · changes land through code" : "Every instance · changes land through code")));
        }
        if (byDefault.length) {
          out.push({ group: `On by default (${byDefault.length})` },
            ...byDefault.map((e) => builtInRow(e, pill("On by default", "ok"), "Changes land through code")));
        }
      } else {
        const homeOnly = builtIn.filter((e) => e.availability === "home_only");
        if (homeOnly.length) {
          out.push({ group: `Home only (${homeOnly.length})` },
            ...homeOnly.map((e) => ({ ...builtInRow(e, pill("Home only", "plain"), "Only the home instance has it"), runs: null, pinText: el("span", { class: "muted", text: "—" }) })));
        }
      }
      if (inst.home) {
        const ours = new Set([...shown, ...builtIn.map((e) => e.name)]);
        const deployed = (versions?.components || []).filter((c) => c.kind === "capability" && !ours.has(c.name));
        const platformRow = (c) => ({
          cap: c.name, what: c.worker ? `Served by ${c.worker}` : "", adds: [], field: null, latest: c.version, runs: runs(c.name),
          pinText: code(c.version), status: pill("Deployed", "ok"), acts: el("span", { class: "muted", text: "Changes land through code" }),
        });
        const platform = deployed.filter((c) => !c.plumbing);
        const plumbing = deployed.filter((c) => c.plumbing);
        if (platform.length) out.push({ group: "Platform capabilities · deployed with the platform" }, ...platform.map(platformRow));
        if (plumbing.length) {
          const flip = () => { internal = !internal; redraw = true; table.reload(); };
          out.push({ group: [`Internal · platform plumbing (${plumbing.length}) `,
            button(internal ? "Hide" : "Show", flip, { size: "sm", variant: "ghost", "aria-expanded": String(internal) })] });
          if (internal) out.push(...plumbing.map(platformRow));
        }
      }
      return out;
    }

    function configureJobs(c, editing) {
      const record = state.record;
      const title = editing ? `Set up Jobs audit storage on ${inst.title}` : `Enable Jobs on ${inst.title}`;
      const review = button("Review bucket", load, { variant: "primary" });
      const d = dialog({
        title,
        body: [el("p", { text: "All jobs on this instance share a private R2 audit bucket in its linked Cloudflare account." })],
        actions: [button("Cancel", () => d.close()), review],
      });
      async function load() {
        review.disabled = true;
        d.say("Checking the audit bucket…");
        try {
          const plan = await ctx.op("app_jobs_audit_plan", { instance: record.id });
          if (!d.content.isConnected) return;
          if (!plan?.bucket || !["create", "reuse"].includes(plan.action)) {
            throw new Error("Pigeon couldn’t determine which audit bucket action to review.");
          }
          const create = plan.action === "create";
          d.close();
          putRecord(ctx, {
            record, next: withCap(record, c, { audit_bucket: plan.bucket }), title, jobsAudit: plan.action,
            target: [["R2 audit bucket", code(plan.bucket)], ["Cloudflare account", plan.account_name || "This instance’s linked account"],
              ["Bucket action", create ? "Create a private bucket" : "Reuse the existing private bucket"]],
            confirmLabel: editing ? "Set up with passkey" : "Enable with passkey",
            effect: "Pigeon verifies write access before saving Jobs. When a run finishes, Pigeon exports its audit record to this bucket.",
          });
        } catch (err) {
          review.disabled = false;
          d.say(err.message || String(err), true);
        }
      }
      load();
    }

    function configureFederation(editing) {
      const record = state.record;
      const name = "mcp-federation";
      const grants = getAt(record, fieldOf(name))?.federation || [];
      const tools = [
        ["digitalocean_droplet_jobs", "List runner checks"],
        ["digitalocean_droplet_job", "Read a runner check"],
        ["digitalocean_droplet_status", "Read runner status"],
      ];
      const option = (value, text) => el("option", { value, text });
      const peer = el("select", { class: "input", name: "peer" }, option("", "Loading instances…"));
      const person = el("select", { class: "input", name: "person" }, option("", "Loading people…"));
      const app = input({ name: "app", mono: true, spellcheck: "false" });
      const appField = field({ label: "OAuth app service user ID",
        hint: "Expand the person’s connected app under Access → People to see its service user ID.", control: app });
      const appScope = el("select", { class: "input", name: "app_scope",
        on: { change: () => { appField.hidden = appScope.value === "all"; } } }, [
        option("specific", "Specific OAuth app"),
        option("all", "All current and future OAuth apps"),
      ]);
      const choices = tools.map(([value, label]) => ({ value, label, control: el("input", { type: "checkbox", checked: true }) }));
      const title = editing ? "Manage MCP Federation trust" : "Enable MCP Federation";
      function review(nextGrants, removing) {
        const next = clone(record);
        const settings = { ...getAt(next, fieldOf(name)), federation: nextGrants };
        if (!nextGrants.length) delete settings.federation;
        setAt(next, fieldOf(name), settings);
        d.close();
        putRecord(ctx, {
          record, next, title: removing ? "Revoke MCP Federation trust" : title,
          target: [["Capability", code(name)]], danger: removing,
          confirmLabel: removing ? "Revoke with passkey" : "Approve with passkey",
          effect: removing ? "Blocks this federation grant on the next call."
            : nextGrants.at(-1)?.all_apps
              ? "This grant covers all current and future OAuth apps connected to this person. The peer instance must also approve access."
              : "The peer instance owner must also approve this person, app and tool access.",
        });
      }
      const add = () => {
        const scope = appScope.value === "all" ? { all_apps: true } : { app: app.value.trim() };
        const grant = { peer: peer.value, person: person.value, ...scope,
          tools: choices.filter(({ control }) => control.checked).map(({ value }) => value) };
        if (!grant.peer || !grant.person || !(grant.all_apps || grant.app) || !grant.tools.length)
          return d.say("Select a peer, a person, an app scope, and at least one tool. Enter the app ID for a specific app.", true);
        if (grants.some((g) => g.peer === grant.peer && g.person === grant.person && g.app === grant.app))
          return d.say("Revoke the existing grant for this peer, person and app before replacing it.", true);
        if (grants.length >= 16) return d.say("This instance already has 16 grants.", true);
        review([...grants, grant], false);
      };
      const d = dialog({
        title,
        body: [
          ...grants.map((g, index) => el("div", { class: "section" },
            facts([["Peer instance", code(g.peer)], ["Person ID", code(g.person)], ["OAuth app scope", g.all_apps ? "All current and future OAuth apps" : code(g.app)],
              ["Read tools", g.tools.map((tool) => tools.find(([value]) => value === tool)?.[1] || tool).join(", ")]]),
            button("Revoke grant", () => review(grants.filter((_, i) => i !== index), true), { variant: "danger" }))),
          field({ label: "Peer instance", control: peer }),
          field({ label: "Person", control: person }),
          field({ label: "OAuth app access", control: appScope }),
          appField,
          ...choices.map(({ control, label }) => el("label", {}, [control, " ", label])),
        ],
        actions: [button("Cancel", () => d.close()), button("Review grant", add, { variant: "primary" })],
      });
      const home = record.account?.kind === "home";
      ctx.op("instances_list").then((instances) => {
        const peers = instances.filter((instance) => instance.id !== record.id
          && (home ? instance.account?.kind === "linked" : instance.account?.kind === "home"));
        peer.replaceChildren(option("", peers.length ? "Select a peer instance" : "No eligible peer instances"),
          ...peers.map((instance) => option(instance.id, instance.title && instance.title !== instance.id
            ? `${instance.title} (${instance.id})` : instance.id)));
      }).catch((error) => {
        peer.replaceChildren(option("", "Could not load instances"));
        d.say(error.message, true);
      });
      ctx.op("app_federation_people", { instance: record.id }).then((people) => {
        person.replaceChildren(option("", people.length ? "Select a person" : "No instance members"),
          ...people.map((member) => option(member.id, member.name || member.id)));
      }).catch((error) => {
        person.replaceChildren(option("", "Could not load people"));
        d.say(error.message, true);
      });
    }

    // Enable or Configure: the capability's settings, its diff as they are typed, then the passkey.
    function configure(c, editing) {
      if (c.cap === "jobs") return configureJobs(c, editing);
      if (c.cap === "mcp-federation") return configureFederation(editing);
      const r = state.record;
      const now = profileOf(r, c);
      const fields = c.fields(r);
      const controls = fields.map((f) => {
        const value = now[f.key] ?? (f.key === "site" ? r.site?.publishing?.site : "");
        const control = f.options
          ? el("select", { class: "input", name: f.key }, f.options.map((o) => el("option", { value: o, text: f.labels?.[o] || o, selected: o === value, disabled: f.disabled?.(o) || false })))
          : input({ name: f.key, mono: f.mono, autocomplete: "off", spellcheck: "false", value: Array.isArray(value) ? value.join(", ") : String(value ?? "") });
        return { f, control };
      });
      const read = () => Object.fromEntries(controls.map(({ f, control }) => [f.key, f.list ? list(control.value) : control.value.trim()]));
      const verb = editing ? "Configure" : "Enable";
      const preview = el("div", {});
      const update = () => preview.replaceChildren(diffBlock(diffLines(r, changes(r, withCap(r, c, read())))));
      const review = () => {
        const values = read();
        const missing = controls.find(({ f }) => !f.optional && (f.list ? !values[f.key].length : !values[f.key]));
        if (missing) { missing.control.focus(); return d.say(`Fill in ${missing.f.label.toLowerCase()}.`, true); }
        const invalid = controls.find(({ f }) => f.validate && !f.validate(values[f.key], values));
        if (invalid) { invalid.control.focus(); return d.say(`Enter a valid ${invalid.f.label}.`, true); }
        putRecord(ctx, {
          record: r, next: withCap(r, c, values), title: `${verb} ${c.cap} on ${inst.title}`,
          target: [["Capability", code(c.cap)]], confirmLabel: `${verb} with passkey`,
          effect: c.effect ? c.effect : editing ? `Changes how ${c.cap} is scoped. The console sections stay the same.`
            : `Adds ${c.adds.join(", ")} to this instance’s console, and ${c.cap}’s tools to its MCP endpoint.`,
        });
      };
      if (!fields.length) return review();
      update();
      const d = dialog({
        title: `${verb} ${c.cap} on ${inst.title}`,
        body: [...controls.map(({ f, control }) => field({ label: f.label, hint: f.hint, control })),
          el("div", { class: "section" }, el("div", { class: "label", text: "Record diff" }), preview),
          el("p", { class: "muted", text: "An agent proposing this with instance_propose would wait in Inbox with the same diff." })],
        actions: [button("Cancel", () => d.close()), button("Review", review, { variant: "primary" })],
      });
      d.content.addEventListener("input", update);
      d.content.addEventListener("change", update);
    }

    function upgrade(c, step) {
      const r = state.record;
      const next = clone(r);
      next.pins = { ...(next.pins || {}), [c.component]: step.suggested };
      const work = (step.work || []).map((w) => `${w.version}: ${w.summary}${w.migration ? ` Migration: ${w.migration}` : ""}`);
      const view = gated.get(c.cap);
      putRecord(ctx, {
        record: r, next, title: `Upgrade ${c.cap} on ${inst.title}`, confirmLabel: "Upgrade with passkey",
        target: [["Capability", code(c.cap)], ["Version", `${step.from || "held"} → ${step.to}`],
          work.length && ["Breaking on the way", el("ol", { class: "steps" }, work.map((w) => el("li", { text: w })))]].filter(Boolean),
        effect: `Moves the pin on ${c.component} to ${step.suggested}, which selects ${step.to}.${view ? ` It unlocks ${view.title}.` : ""}`,
      });
    }

    function disable(c) {
      const r = state.record;
      const also = dependents(r, c);
      putRecord(ctx, {
        record: r, next: withoutCap(r, c), title: `Disable ${c.cap} on ${inst.title}`, danger: true, confirmLabel: "Disable with passkey",
        target: [["Capability", code(c.cap)], also.length && ["Also disables", also.map((d) => d.cap).join(", ")]].filter(Boolean),
        effect: c.cap === "redirects" ? "Removes every redirect from the record. Their hosts stay attached to the instance’s Worker: pigeon never detaches them." : `Removes ${[c, ...also].flatMap((d) => d.adds).join(", ")} from this instance’s console and ${[c, ...also].map((d) => d.cap).join(" and ")} from its MCP tools. Nothing in the account is deleted.`,
      });
    }

    return el("div", { class: "view" },
      strip("neutral", el("span", { class: "grow" }, "This is the only place capabilities change. Enable, Configure, Upgrade and Disable each make one diff of the instance record, which you approve with your passkey. Agents propose the same diff with ",
        code("instance_propose"), ", and it waits in ", go(ctx, { instance: inst.id, area: "inbox" }, "Inbox"), ".")),
      table);
  }

  // ---- Overview (C2): facts, the link, and the record's other fields as a form or JSON.
  // The record's capability fields (docs/plans/record-reshape.md), and `repo.source` beside the
  // repository: changed in Capabilities, never here.
  const CAP_FIELDS = new Set(["site", "ci", "observability", "network", "instance", "data", "cloud", "jobs", "pins", "capabilities"]);
  const FIXED = new Set(["id", "account", "version"]);
  const bareRepo = (repo) => { if (!repo || typeof repo !== "object") return repo ?? null; const { source, ...rest } = repo; return Object.keys(rest).length ? rest : null; };
  const editable = (r) => Object.fromEntries(Object.entries(r).filter(([k]) => !CAP_FIELDS.has(k) && !FIXED.has(k)).map(([k, v]) => [k, k === "repo" ? bareRepo(v) : v]));
  // `repo` as edited, with the record's `repo.source` put back beside it.
  const withSource = (repo, was) => (was?.source ? { ...(repo || {}), source: was.source } : repo);
  const repoText = (repo) => (!repo?.kind ? "" : repo.kind === "github" ? `github:${repo.full_name}` : repo.kind === "artifacts" ? `artifacts:${repo.namespace}/${repo.repo}` : JSON.stringify(repo));
  function repoFrom(text, was) {
    const t = text.trim();
    if (!t) return null;
    const [kind, rest = ""] = t.split(/:(.*)/s);
    if (kind === "github" && /^[\w.-]+\/[\w.-]+$/.test(rest)) {
      return { kind: "github", full_name: rest, installation: was?.kind === "github" && was.full_name === rest ? was.installation ?? null : null };
    }
    const parts = rest.split("/");
    if (kind === "artifacts" && parts.length === 2 && parts.every(Boolean)) return { kind: "artifacts", namespace: parts[0], repo: parts[1] };
    throw new Error("A repository is github:owner/name or artifacts:namespace/repo, or empty for none.");
  }

  function recordEditor(ctx, r) {
    let mode = "form";
    const title = input({ name: "title", value: r.title || "" });
    const zones = input({ name: "zones", mono: true, value: (r.zones || []).join(", "), spellcheck: "false" });
    const repo = input({ name: "repo", mono: true, value: repoText(r.repo), spellcheck: "false", placeholder: "github:owner/name" });
    const worker = input({ name: "worker", mono: true, value: r.worker || "", spellcheck: "false" });
    const override = el("input", { type: "checkbox", name: "worker_override", checked: Boolean(r.worker_override) });
    const form = el("div", { class: "panel" },
      field({ label: "Title", control: title }),
      field({ label: "Zones", hint: "Comma-separated DNS zones this instance may serve hostnames in.", control: zones }),
      field({ label: "Repository", hint: "github:owner/name or artifacts:namespace/repo; empty for none.", control: repo }),
      field({ label: "Worker", hint: "The one Worker that serves the instance’s sites; empty for the default.", control: worker }),
      el("label", { class: "toolbar" }, override, el("span", { class: "ink2", text: "The Worker’s name may differ from the default" })),
      el("p", { class: "muted", text: `Sites (${(r.sites || []).length}) are edited as JSON.` }));
    const json = el("textarea", { class: "input", rows: "14", spellcheck: "false", "aria-label": "Instance record JSON, without its capabilities" });
    json.value = JSON.stringify(editable(r), null, 2);
    json.hidden = true;
    const preview = el("div", {});
    const problem = el("p", { class: "bad-text", role: "status" });
    function next() {
      if (mode === "json") {
        let edited;
        try { edited = JSON.parse(json.value); } catch (err) { throw new Error(`The JSON doesn’t parse: ${err.message}`); }
        if (!plain(edited)) throw new Error("The JSON is an object of the record’s fields.");
        const bad = Object.keys(edited).find((k) => CAP_FIELDS.has(k) || FIXED.has(k)) || (edited.repo?.source && "repo.source");
        if (bad) throw new Error(`${bad} is not edited here: ${FIXED.has(bad) ? "it can’t change" : "change it in Capabilities"}.`);
        const out = clone(r);
        for (const k of Object.keys(editable(r))) delete out[k];
        return { ...out, ...edited, repo: withSource(edited.repo, r.repo) };
      }
      return { ...clone(r), title: title.value.trim(), zones: list(zones.value), repo: withSource(repoFrom(repo.value, r.repo), r.repo),
        worker: worker.value.trim() || null, worker_override: override.checked };
    }
    function update() {
      try {
        const found = changes(r, next());
        problem.textContent = "";
        preview.replaceChildren(found.length ? diffBlock(diffLines(r, found)) : el("p", { class: "muted", text: "No changes yet." }));
      } catch (err) {
        problem.textContent = err.message;
        preview.replaceChildren();
      }
    }
    const review = button("Review changes", () => {
      let to;
      try { to = next(); } catch (err) { problem.textContent = err.message; return; }
      putRecord(ctx, {
        record: r, next: to, title: `Save ${r.title || r.id}`, confirmLabel: "Save with passkey",
        effect: "Saves the record a version on. Capabilities and pins stay as they are.",
      });
    }, { variant: "primary" });
    const switcher = segmented({ label: "Editor", value: mode, options: [{ id: "form", title: "Form" }, { id: "json", title: "JSON" }],
      onChange: (m) => {
        // Carry what was typed across: the form's edits into the JSON, and back.
        try {
          const now = next();
          if (m === "json") json.value = JSON.stringify(editable(now), null, 2);
          else {
            title.value = now.title || "";
            zones.value = (now.zones || []).join(", ");
            repo.value = repoText(now.repo);
            worker.value = now.worker || "";
            override.checked = Boolean(now.worker_override);
          }
        } catch { /* Kept as typed; the problem shows below. */ }
        mode = m;
        form.hidden = m !== "form";
        json.hidden = m !== "json";
        update();
      } });
    const node = el("section", { class: "section" },
      el("div", { class: "section-head" }, el("h2", { text: "Record" }),
        el("span", { class: "muted" }, "Everything but capabilities, which change in ", go(ctx, { instance: r.id, area: "capabilities" }, "Capabilities"), "."),
        el("span", { class: "grow" }), switcher),
      form, json,
      el("div", { class: "label", text: "Diff" }), preview, problem,
      el("div", { class: "toolbar" }, review));
    node.addEventListener("input", update);
    node.addEventListener("change", update);
    update();
    return node;
  }

  function linkPanel(ctx, inst, d) {
    if (inst.home) return panel("Cloudflare link", el("p", { class: "muted", text: "The home account runs the platform itself. It needs no link." }));
    const l = d?.link || {};
    const tone = l.status === "linked" ? "ok" : l.status === "not linked" ? "warn" : "bad";
    const linked = l.status && l.status !== "not linked" && !l.error;
    const status = el("p", { class: "muted", role: "status" });
    const link = button(linked ? "Link again" : "Link Cloudflare account", async () => {
      if (shell.fixtures) return toast("Link opens Cloudflare’s consent screen (fixture; nothing was sent).", "info");
      link.disabled = true;
      status.textContent = "Opening Cloudflare…";
      try {
        const begin = await window.pigeonPasskey.post("/linked/begin", { instance: inst.id });
        location.assign(begin.url);
      } catch (err) {
        status.textContent = err.message;
        link.disabled = false;
      }
    }, { variant: l.status === "linked" ? undefined : "primary" });
    const unlink = linked ? button("Unlink", () => confirm({
      title: `Unlink ${inst.title}`,
      target: [["Instance", code(inst.id)], l.account_name && ["Account", l.account_name], l.account_id && ["Account id", code(l.account_id)]].filter(Boolean),
      diff: [{ op: "del", text: `link: ${l.account_name || "account"} ${l.account_id || ""}`.trim() }],
      effect: "The platform forgets this account’s access. Nothing in the Cloudflare account is deleted, and its sites keep serving; the platform can’t deploy to it or read it until you link again.",
      action: "linked-unlink", id: inst.id, danger: true, confirmLabel: "Unlink with passkey",
      onDone: () => setTimeout(ctx.refresh, 0),
    }), { variant: "danger" }) : null;
    return el("section", { class: "panel" },
      el("div", { class: "section-head" }, el("h2", { text: "Cloudflare link" }), el("span", { class: "grow" }), pill(l.status || "unknown", tone)),
      facts([
        l.account_name && ["Account", l.account_name],
        l.account_id && ["Account id", code(l.account_id)],
        l.linked_ms && ["Linked", when(l.linked_ms)],
        l.access_expires_ms && ["Access expires", when(l.access_expires_ms)],
        l.scope && ["Scopes", code(l.scope)],
        d?.worker && ["Worker", code(d.worker)],
      ]),
      l.status === "refresh failed" ? el("p", { class: "bad-text", text: "The platform couldn’t refresh this account’s access. Link again to restore it." }) : null,
      l.error ? el("p", { class: "bad-text", text: l.error }) : null,
      el("div", { class: "toolbar" }, link, unlink), status);
  }

  function overview(ctx) {
    const inst = ctx.instance;
    const out = el("div", { class: "view" }, el("p", { class: "muted", role: "status", text: "Loading the record…" }));
    const sections = (ctx.manifest.areas || []).filter((a) => a.group === "instance" && !["overview", "capabilities"].includes(a.id)).map((a) => a.title);
    const caps = { instance: inst.id, area: "capabilities" };
    const load = async () => {
      try {
        const [r, d] = await Promise.all([
          recordOf(ctx),
          inst.home ? null : ctx.op("app_linked_details", { instance: inst.id }).catch((err) => ({ link: { status: "unknown", error: `Couldn’t read the link: ${err.message}` } })),
        ]);
        if (ctx.signal.aborted) return;
        if (!r) {
          out.replaceChildren(el("div", { class: "notice" }, el("strong", { text: `No record for ${inst.title}.` }),
            el("p", { class: "ink2", text: "The instance list doesn’t include it. It may have been retired." })));
          return;
        }
        const enabled = CAPS.filter((c) => on(r, c));
        const facts1 = panel("Instance", facts([
          ["Id", code(r.id)],
          ["Account", inst.home ? "Home account" : "Linked account"],
          ["Host", code(inst.host || "—")],
          ["Zones", (r.zones || []).length ? (r.zones || []).map((z) => code(z)).flatMap((z, i) => (i ? [", ", z] : [z])) : "none"],
          ["Repository", r.repo?.kind ? code(repoText(r.repo)) : "none"],
          ["Worker", r.worker ? code(r.worker) : "none (no sites yet)"],
          ["Record", `v${r.version ?? 0}`],
          ["Capabilities", [enabled.length ? enabled.flatMap((c, i) => (i ? [", ", code(c.cap)] : [code(c.cap)])) : "none", " · ", go(ctx, caps, enabled.length ? "Manage" : "Enable one")]],
          ["Console sections", sections.length ? sections.join(", ") : "Only Overview and Capabilities"],
        ]));
        out.replaceChildren(el("div", { class: "cols" }, facts1, linkPanel(ctx, inst, d)), recordEditor(ctx, r));
      } catch (err) {
        if (ctx.signal.aborted) return;
        out.replaceChildren(strip("bad", el("span", { class: "grow", text: `Couldn’t load the record: ${err.message || err}` }),
          button("Retry", () => { out.replaceChildren(el("p", { class: "muted", role: "status", text: "Loading the record…" })); load(); }, { size: "sm" })));
      }
    };
    load();
    return out;
  }

  register("overview", null, overview);
  register("capabilities", null, capabilities);
})();
