/* The console's read-only data views of one instance: Storage › Buckets, Source › Repository,
 * CI › Jobs, Metrics › Workers, Jobs › Runs, Ingest › Runs and Email › Mailboxes. Each reads only through /app/op with the
 * selected instance, which binds the call to that instance's approved record exactly as its MCP
 * endpoint does (console/web/ops.rs), so no view can name another bucket,
 * repository, Worker or job. None of them writes. */
(() => {
  "use strict";
  const shell = window.pigeonShell;
  if (!shell) return;
  const { register, el, pill, tag, code, button, facts, panel, strip, segmented, input, field, dataTable, icon, mailFrame, saveFile } = shell;

  // ---- Formatting.
  const minutes = (ms) => Math.max(0, Math.round(ms / 60000));
  function ago(ms) {
    if (!ms) return "—";
    const m = minutes(Date.now() - ms);
    return m < 1 ? "just now" : m < 60 ? `${m} min ago` : m < 2880 ? `${Math.floor(m / 60)} h ago` : `${Math.floor(m / 1440)} d ago`;
  }
  const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "—");
  const iso = (s) => (s ? String(s).replace("T", " ").replace(/\.\d+Z?$|Z$/, "").slice(0, 16) + " UTC" : "—");
  const short = (s, n = 12) => String(s || "").slice(0, n);
  const count = (n) => (Number.isFinite(Number(n)) ? Math.round(Number(n)).toLocaleString("en-US") : "—");
  function bytes(n) {
    n = Number(n);
    if (!Number.isFinite(n)) return "—";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1000 && i < units.length - 1) { n /= 1000; i += 1; }
    return `${i ? n.toFixed(n < 10 ? 1 : 0) : n} ${units[i]}`;
  }
  function duration(ms) {
    if (ms == null || !Number.isFinite(Number(ms))) return "—";
    const s = Math.round(Number(ms) / 1000);
    return s < 60 ? `${s} s` : s < 3600 ? `${Math.floor(s / 60)} min ${s % 60} s` : `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
  }
  const ms = (v) => (Number.isFinite(Number(v)) ? `${Number(v).toFixed(Number(v) < 10 ? 1 : 0)} ms` : "—");
  const percent = (r) => (Number.isFinite(Number(r)) ? `${(Number(r) * 100).toFixed(Number(r) < 0.01 ? 2 : 1)}%` : "—");
  const repoName = (r) => (typeof r === "string" ? r : r?.full_name || (r?.namespace ? `${r.namespace}/${r.repo}` : null));

  // A part of a view that loads on its own: a status line while it waits, an error with Retry.
  function loadInto(node, load, show, noun) {
    let turn = 0;
    async function run() {
      const mine = (turn += 1);
      node.replaceChildren(el("p", { class: "muted", role: "status", text: `Loading ${noun}…` }));
      try {
        const data = await load();
        if (mine === turn) node.replaceChildren(...[].concat(show(data)).filter(Boolean));
      } catch (err) {
        if (mine !== turn || err?.name === "AbortError") return;
        node.replaceChildren(strip("bad", el("span", { class: "grow", text: `Couldn’t load ${noun}: ${err.message || err}` }),
          button("Retry", run, { size: "sm" })));
      }
    }
    run();
    return run;
  }
  const readOnly = (text) => el("p", { class: "muted", text });

  // ---- Storage › Buckets: the instance's one approved location (store, bucket, prefix), browsed
  // by prefix a page at a time. storage_list_objects with `instance` answers keys relative to the
  // approved prefix and refuses a raw account or bucket; storage_list_buckets and
  // storage_head_object take those, so this view does not use them.
  // With the instance's data store (the record's `data`, docs/plans/email.md) the view can
  // also browse its bucket's `blobs/` and `derived/` through the data Worker (`source: "data"`),
  // beside the approved S3 location (`data.read`), which reads as before.
  function storage(ctx) {
    const id = ctx.instance.id;
    const s = ctx.record?.data?.read || {};
    const hasS3 = s.state === "enabled" || Boolean(s.bucket);
    const hasData = Boolean(ctx.record?.data);
    let source = hasS3 ? "s3" : "data";
    const dataName = ctx.record?.account?.kind === "home" ? "pigeon-data" : `${String(id).replace(/[/.]/g, "-")}-data`;
    const locationOf = () => (source === "data" ? dataName : [s.bucket, s.prefix].filter(Boolean).join("/").replace(/\/+$/, ""));
    let location = locationOf();
    let prefix = "";
    let rows = [];
    let next = null;
    let more = false;
    const crumbs = el("nav", { class: "toolbar", "aria-label": "Prefix" });
    const moreButton = button("Load more", () => { more = true; table.reload(); }, { size: "sm", hidden: true });
    function open(to) {
      prefix = to;
      rows = [];
      next = null;
      more = false;
      showCrumbs();
      table.reload();
    }
    function showCrumbs() {
      const parts = prefix.split("/").filter(Boolean);
      const nodes = [button(location || "Root", () => open(""), { variant: "ghost", size: "sm", "aria-current": parts.length ? null : "location" })];
      parts.forEach((part, i) => {
        const to = `${parts.slice(0, i + 1).join("/")}/`;
        nodes.push(el("span", { class: "muted", "aria-hidden": "true", text: "/" }));
        nodes.push(i === parts.length - 1 ? el("strong", { "aria-current": "location", text: part })
          : button(part, () => open(to), { variant: "ghost", size: "sm" }));
      });
      crumbs.replaceChildren(el("span", { class: "label", text: "Prefix" }), ...nodes);
    }
    const name = (key) => (key.startsWith(prefix) ? key.slice(prefix.length) : key) || key;
    const table = dataTable({
      label: "Objects", noun: "objects", filter: "Filter this page", signal: ctx.signal,
      empty: "Nothing is stored under this prefix.",
      columns: [
        { label: "Name", cell: (r) => (r.dir
          ? button(name(r.key), () => open(r.key), { variant: "ghost", size: "sm", icon: "storage", title: `Open ${r.key}` })
          : code(name(r.key))) },
        { label: "Size", cls: "num", cell: (r) => (r.dir ? "—" : el("span", { title: `${count(r.size)} bytes`, text: bytes(r.size) })) },
        { label: "Modified", cls: "num", cell: (r) => (r.dir ? "—" : iso(r.last_modified)) },
        { label: "ETag", cls: "mono", cell: (r) => (r.dir ? tag("prefix") : short(r.etag, 10) || "—") },
      ],
      load: async () => {
        const args = { instance: id, prefix, delimiter: "/", max_keys: 100 };
        if (hasS3 && hasData) args.source = source;
        if (more && next) args.token = next;
        const data = await ctx.op("storage_list_objects", args);
        const page = [
          ...(data.prefixes || []).map((key) => ({ dir: true, key })),
          ...(data.objects || []).map((o) => ({ ...o, dir: false })),
        ];
        rows = more ? rows.concat(page) : page;
        next = data.next || null;
        more = false;
        moreButton.hidden = !next;
        return rows;
      },
    });
    showCrumbs();
    const sources = hasS3 && hasData ? segmented({ label: "Source", value: source,
      options: [{ id: "s3", title: "Approved location" }, { id: "data", title: "Data store" }],
      onChange: (to) => { source = to; location = locationOf(); open(""); } }) : null;
    return el("div", { class: "view" },
      el("div", { class: "section-head" }, el("h2", { text: hasS3 ? "Approved location" : "Data store" }),
        readOnly(hasS3 ? "Read-only. Only this instance’s approved bucket and prefix."
          : "Read-only. The instance’s data store: originals under blobs/, derived files under derived/.")),
      sources ? el("div", { class: "toolbar" }, el("span", { class: "label", text: "Source" }), sources) : null,
      hasS3 && (s.store || s.bucket) ? facts([
        s.store && ["Store", code(s.store)],
        s.bucket && ["Bucket", code(s.bucket)],
        s.prefix && ["Prefix", code(s.prefix)],
      ]) : null,
      crumbs, table, el("div", { class: "toolbar" }, moreButton));
  }

  // ---- Source › Repository: resolve a revision of the approved repository and read its files.
  // The tools read one regular file at a time; they list no directories.
  function source(ctx) {
    const id = ctx.instance.id;
    const repo = repoName(ctx.record?.repo);
    let commit = null;
    const recent = [];
    const revision = input({ mono: true, value: "HEAD", name: "revision", spellcheck: "false", autocomplete: "off" });
    const path = input({ mono: true, name: "path", placeholder: "README.md", spellcheck: "false", autocomplete: "off" });
    const revisionFacts = el("div", { class: "section" });
    const file = el("div", { class: "section" });
    const recentNode = el("div", { class: "toolbar" });
    let resolved = null;

    // Files are read at the resolved commit, so what is shown matches the facts above it.
    function resolve() {
      const asked = revision.value.trim() || "HEAD";
      commit = null;
      loadInto(revisionFacts, () => {
        resolved = ctx.op("source_resolve_revision", { instance: id, revision: asked });
        resolved.catch(() => {});
        return resolved;
      }, (d) => {
        commit = d.commit;
        return facts([repo && ["Repository", code(repo)], ["Revision", code(d.revision || asked)], ["Commit", code(d.commit)]]);
      }, "the revision");
    }
    function remember(p) {
      const at = recent.indexOf(p);
      if (at >= 0) recent.splice(at, 1);
      recent.unshift(p);
      recent.length = Math.min(recent.length, 6);
      recentNode.replaceChildren(...(recent.length ? [el("span", { class: "label", text: "Recent" }),
        ...recent.map((r) => button(r, () => { path.value = r; read(); }, { variant: "ghost", size: "sm" }))] : []));
    }
    function read(quiet = false) {
      const p = path.value.trim().replace(/^\/+/, "");
      if (!p) { path.focus(); return; }
      loadInto(file, async () => {
        const at = commit || (await resolved.then((d) => d.commit, () => null));
        if (!at) throw new Error("resolve a revision first");
        return ctx.op("source_read_file", { instance: id, revision: at, path: p });
      }, (d) => {
        remember(p);
        const head = el("div", { class: "section-head" }, el("h3", {}, code(d.path || p)),
          el("span", { class: "muted", text: `${bytes(d.size)} at ${short(d.commit, 12)}` }));
        if (d.encoding !== "utf-8") return [head, readOnly(`A binary file (${count(d.size)} bytes). It is not shown here.`)];
        return [head, el("pre", { class: "code", tabindex: "0", "aria-label": `Contents of ${d.path || p}`, text: d.content || "" })];
      }, quiet ? "README.md" : p);
    }
    const revisionForm = el("form", { class: "toolbar", on: { submit: (e) => { e.preventDefault(); resolve(); } } },
      field({ label: "Revision", hint: "A branch, tag or commit.", control: revision }),
      button("Resolve", null, { type: "submit" }));
    const pathForm = el("form", { class: "toolbar", on: { submit: (e) => { e.preventDefault(); read(); } } },
      field({ label: "File", hint: "A path from the repository root.", control: path }),
      button("Open", null, { type: "submit", variant: "primary" }));
    resolve();
    path.value = "README.md";
    read(true);
    return el("div", { class: "view" },
      readOnly(repo ? `Read-only. ${repo}, the instance’s approved repository.` : "Read-only. The instance’s approved repository."),
      el("div", { class: "cols" }, panel("Revision", revisionForm, revisionFacts), panel("Read a file", pathForm, recentNode)),
      file);
  }

  // ---- CI › Jobs: the instance's jobs (its repository and approved job names only), newest
  // first; a row expands into the job's status, its changes and its output.
  const CI_STATES = ["queued", "dispatched", "running", "succeeded", "failed", "cancelled", "timed_out", "error"];
  const ciTone = (s) => ({ succeeded: "ok", failed: "bad", error: "bad", timed_out: "bad", running: "info", dispatched: "info", queued: "warn" })[s] || "plain";
  const ciState = (s) => pill(String(s || "unknown").replace(/_/g, " "), ciTone(s));
  function ciJob(ctx, row) {
    const node = el("div", { class: "section" });
    loadInto(node, () => ctx.op("ci_status", { instance: ctx.instance.id, id: row.id }), (j) => {
      const link = typeof j.url === "string" && j.url.startsWith("https://") ? el("a", { href: j.url, text: j.connector || "Open" }) : j.connector;
      const out = j.output == null || j.output === "" ? null : typeof j.output === "string" ? j.output : JSON.stringify(j.output, null, 2);
      return [
        el("div", { class: "cols" }, facts([
          ["Job", code(j.job)], ["Id", code(j.id)], ["State", ciState(j.state)],
          ["Commit", code(j.commit)], j.task && ["Task", j.task], ["Attempt", String(j.attempt ?? "—")],
          ["Connector", link || "—"], j.image && ["Image", code(j.image)],
        ]), facts([
          ["Created", when(j.created_at)], j.started_at && ["Started", when(j.started_at)],
          j.finished_at && ["Finished", when(j.finished_at)], ["Duration", duration(j.duration_ms)],
          j.exit_code != null && ["Exit code", code(j.exit_code)], j.detail && ["Detail", j.detail],
          j.excluded?.length && ["Excluded connectors", j.excluded.join(", ")],
        ])),
        j.events?.length ? dataTable({
          label: "Changes", noun: "changes", load: j.events,
          columns: [{ label: "At", cell: (e) => when(e.at) }, { label: "State", cell: (e) => ciState(e.state) },
            { label: "Detail", cell: (e) => e.detail || "—" }],
        }) : null,
        out ? el("pre", { class: "code", tabindex: "0", "aria-label": "Output", text: out }) : null,
      ];
    }, "the job");
    return node;
  }
  function ci(ctx) {
    let state = "";
    const select = el("select", { class: "input", "aria-label": "State", on: { change: () => { state = select.value; table.reload(); } } },
      el("option", { value: "", text: "Every state" }), CI_STATES.map((s) => el("option", { value: s, text: s.replace(/_/g, " ") })));
    const table = dataTable({
      label: "CI jobs", noun: "CI jobs", filter: "Filter jobs", signal: ctx.signal,
      empty: "No jobs for this instance yet.",
      expand: (row) => ciJob(ctx, row),
      columns: [
        { label: "Job", cell: (j) => [code(j.job), el("span", { class: "sub mono", text: short(j.id, 16) })] },
        { label: "State", cell: (j) => ciState(j.state) },
        { label: "Commit", cls: "mono", cell: (j) => short(j.commit, 9) },
        { label: "Task", cell: (j) => j.task || "—" },
        { label: "Caller", cell: (j) => j.caller_name || j.caller || "—" },
        { label: "Created", cls: "num", cell: (j) => ago(j.created_at) },
        { label: "Took", cls: "num", cell: (j) => duration(j.duration_ms) },
      ],
      load: async () => {
        const args = { instance: ctx.instance.id, limit: 50 };
        if (state) args.state = state;
        const data = await ctx.op("ci_jobs", args);
        return data.jobs || [];
      },
    });
    return el("div", { class: "view" },
      el("div", { class: "toolbar" }, readOnly("Read-only. Jobs of this instance’s repository and approved job names, newest first."),
        el("span", { class: "grow" }), select),
      table);
  }

  // ---- Metrics › Workers: the instance's approved Worker over a window of hours; on the home
  // instance every platform Worker, in total and one row each (`workers`).
  const dollars = (v) => (Number.isFinite(Number(v)) ? `$${Number(v).toFixed(Number(v) < 1 ? 4 : 2)}` : "—");
  const errorRate = (r) => (r >= 0.01 ? pill(percent(r), "bad") : r >= 0.001 ? pill(percent(r), "warn") : percent(r));
  const WINDOWS = [{ id: "1", title: "1 h" }, { id: "6", title: "6 h" }, { id: "24", title: "24 h" }, { id: "168", title: "7 d" }];
  function metrics(ctx) {
    let hours = "24";
    const body = el("div", { class: "view" });
    const show = (d) => {
      const m = d.metrics || {};
      const rate = Number(m.error_rate) || 0;
      const statuses = Object.entries(m.by_status || {}).sort((a, b) => b[1] - a[1]);
      const total = statuses.reduce((n, [, v]) => n + Number(v || 0), 0);
      const usd = Number(d.estimated_cost?.usd);
      const workers = Array.isArray(d.workers) ? d.workers : [];
      const several = workers.length > 1;
      const perWorker = several ? el("div", { class: "section" }, el("h2", { text: "By Worker" }), dataTable({
        label: "Metrics by Worker", noun: "Workers", empty: "No Workers in this window.",
        load: [...workers].sort((a, b) => Number(b.metrics?.requests || 0) - Number(a.metrics?.requests || 0)),
        columns: [
          { label: "Worker", cell: (w) => code(w.worker || "—") },
          { label: "Requests", cls: "num", cell: (w) => count(w.metrics?.requests) },
          { label: "Errors", cls: "num", cell: (w) => count(w.metrics?.errors) },
          { label: "Error rate", cls: "num", cell: (w) => errorRate(Number(w.metrics?.error_rate) || 0) },
          { label: "CPU p50", cls: "num", cell: (w) => ms(w.metrics?.cpu_ms_p50) },
          { label: "Wall p99", cls: "num", cell: (w) => ms(w.metrics?.wall_ms_p99) },
          { label: "Estimated cost", cls: "num", cell: (w) => dollars(w.estimated_cost?.usd) },
        ],
      })) : null;
      return [
        several
          ? el("p", { class: "muted" }, `All ${workers.length} platform Workers, in total and each · ${iso(d.from)} to ${iso(d.to)}`)
          : el("p", { class: "muted" }, "Worker ", code(d.worker || ctx.record?.worker || "—"), ` · ${iso(d.from)} to ${iso(d.to)}`),
        el("div", { class: "cols" },
          panel("Traffic", facts([
            ["Requests", count(m.requests)],
            ["Errors", count(m.errors)],
            ["Error rate", errorRate(rate)],
            ["Estimated cost", dollars(usd)],
          ]), d.estimated_cost?.basis ? el("p", { class: "muted", text: d.estimated_cost.basis }) : null),
          panel("Time per request", facts([
            ["CPU p50", ms(m.cpu_ms_p50)], ["CPU p99", ms(m.cpu_ms_p99)],
            ["Wall p50", ms(m.wall_ms_p50)], ["Wall p99", ms(m.wall_ms_p99)],
          ]))),
        perWorker,
        el("div", { class: "section" }, el("h2", { text: "By status" }), dataTable({
          label: "Requests by status", noun: "statuses", empty: "No requests in this window.",
          load: statuses.map(([status, n]) => ({ status, n })),
          columns: [
            { label: "Status", cell: (r) => (r.status === "success" ? pill(r.status, "ok") : tag(r.status)) },
            { label: "Requests", cls: "num", cell: (r) => count(r.n) },
            { label: "Share", cls: "num", cell: (r) => (total ? percent(r.n / total) : "—") },
          ],
        })),
      ];
    };
    const reload = () => loadInto(body, () => ctx.op("observability_worker_metrics", { instance: ctx.instance.id, hours: Number(hours) }), show, "the metrics");
    const windows = segmented({ label: "Window", options: WINDOWS, value: hours, onChange: (id) => { hours = id; reload(); } });
    reload();
    return el("div", { class: "view" },
      el("div", { class: "toolbar" }, readOnly(ctx.record?.account?.kind === "home"
        ? "Read-only. Request counts, errors and time for every platform Worker."
        : "Read-only. Request counts, errors and time for the instance’s approved Worker."),
        el("span", { class: "grow" }), el("span", { class: "label", text: "Window" }), windows),
      body);
  }

  // ---- Ingest › Runs: the instance's runs (app_ingest_runs, which never deletes a VM, unlike
  // ingest_status) and, per run, its report once it has finished.
  const runTone = (s) => ({ done: "ok", failed: "bad", stopped: "warn", running: "info", listing: "info", starting: "info" })[s] || "plain";
  function ingestReport(ctx, run) {
    const node = el("div", { class: "section" });
    loadInto(node, () => ctx.op("ingest_report", { instance: ctx.instance.id, run: run.run }), (r) => {
      if (!r || r.report === null || !r.counters) return readOnly(r?.note || "There is no report yet: the run has not finished.");
      const c = r.counters;
      const lines = (rows, label) => (rows?.length ? el("div", { class: "section" }, el("h3", { text: label }), dataTable({
        label, noun: label.toLowerCase(), load: rows,
        columns: [
          { label: "Key", cell: (l) => [code(l.origin?.key || l.norm_name || l.id), l.member?.path ? el("span", { class: "sub mono", text: l.member.path }) : null] },
          { label: "Size", cls: "num", cell: (l) => bytes(l.size) },
          { label: "Why", cell: (l) => l.error || String(l.reason || "—").replace(/_/g, " ") },
        ],
      })) : null);
      const truncated = Object.entries(r.truncated_from || {}).map(([k, n]) => `${k.replace(/_/g, " ")} (${count(n)})`);
      return [
        el("div", { class: "toolbar" }, pill(r.state, runTone(r.state)), r.dry_run ? tag("Dry run") : null,
          el("span", { class: "muted", text: `${when(r.started_ms)} to ${when(r.finished_ms)}` })),
        el("div", { class: "cols" }, facts([
          ["Listed", `${count(c.listed)} · ${bytes(c.listed_bytes)}`],
          ["Read", bytes(c.bytes_read)],
          r.dry_run ? ["Would copy", `${count(c.would_upload)} · ${bytes(c.would_upload_bytes)}`]
            : ["Copied", `${count(c.uploaded)} · ${bytes(c.uploaded_bytes)}`],
          ["Already in R2", `${count(c.duplicates)} · ${bytes(c.duplicate_bytes)}`],
        ]), facts([
          ["Conflicts", c.conflicts ? pill(count(c.conflicts), "warn") : "0"],
          ["Failed", c.failed ? pill(count(c.failed), "bad") : "0"],
          ["Skipped", count(c.skipped)],
          ["Archives", `${count(c.archives_opened)} opened, ${count(c.archives_not_opened)} not`],
        ])),
        r.flags?.length ? el("p", {}, el("span", { class: "label", text: "Flags " }),
          r.flags.map(([flag, n]) => tag(`${String(flag).replace(/_/g, " ")} ${count(n)}`))) : null,
        lines(r.failures, "Failures"), lines(r.conflicts, "Conflicts"), lines(r.not_opened, "Not opened"),
        truncated.length ? readOnly(`Lists cut short; in full: ${truncated.join(", ")}.`) : null,
      ];
    }, "the report");
    return node;
  }
  function ingest(ctx) {
    return el("div", { class: "view" },
      readOnly("Read-only. Runs of this instance, newest first. Starting and stopping a run stay in MCP."),
      dataTable({
        label: "Ingest runs", noun: "ingest runs", filter: "Filter runs", signal: ctx.signal,
        empty: "No ingest runs yet.",
        expand: (run) => ingestReport(ctx, run),
        columns: [
          { label: "Run", cell: (r) => [code(r.run), el("span", { class: "sub mono", text: short(r.commit, 9) })] },
          { label: "Kind", cell: (r) => tag(r.dry_run ? "Dry run" : "Copy") },
          { label: "Host", cell: (r) => [r.host || "—", r.region ? el("span", { class: "sub", text: r.region }) : null] },
          { label: "Source", cls: "mono", cell: (r) => r.bucket || "—" },
          { label: "Started", cls: "num", cell: (r) => ago(r.started_ms) },
          { label: "Host machine", cell: (r) => (r.host === "cloudflare" ? tag("Container")
            : r.vm_deleted ? pill("deleted", "plain") : r.vm ? pill("running", "info") : "—") },
        ],
        load: async () => {
          const data = await ctx.op("app_ingest_runs", { instance: ctx.instance.id });
          return [...(data.runs || [])].sort((a, b) => (b.started_ms || 0) - (a.started_ms || 0));
        },
      }));
  }

  // ---- Jobs › Runs: durable history, independent of the provider's live connection.
  const estimatedUsd = (amount) => typeof amount === "number" && Number.isFinite(amount)
    ? (amount > 0 && amount < 0.01 ? "<0.01 USD" : `${amount.toFixed(2)} USD`) : "—";
  const auditStatus = (status) => ({
    not_configured: "No audit bucket",
    retrying: "Delivery failed · retry scheduled",
    pending: "Pending",
    stored: "Stored in R2",
    verified: "Verified in R2",
  })[status] || status;
  function jobHistory(ctx, row) {
    const node = el("div", { class: "section" });
    loadInto(node, () => ctx.op("digitalocean_droplet_job", { instance: ctx.instance.id, id: row.id }), (data) => {
      const job = data.job;
      return [
        facts([
          ["Job", code(job.id)],
          ["Job state", job.state || "—"],
          ["Provider state", job.provider_state || "—"],
          ...(job.provider_observed_ms ? [["Provider checked", when(job.provider_observed_ms)]] : []),
          ["Runner stage", job.stage || "—"],
          ["Estimated cost", estimatedUsd(job.estimated_usd)],
          ["Started", when(job.started_ms)],
          ["Deadline", when(job.deadline_ms)],
          ...(job.audit_status ? [["Audit export", auditStatus(job.audit_status)]] : []),
        ]),
        job.error ? el("p", { class: "error", text: job.error }) : null,
        dataTable({
          label: "Job events", noun: "events", signal: ctx.signal,
          empty: "No runner stages have been recorded.",
          columns: [
            { label: "Time", cell: (event) => when(event.at_ms) },
            { label: "Stage", cell: (event) => event.stage },
            { label: "Outcome", cell: (event) => event.outcome },
          ],
          load: job.events || [],
        }),
      ];
    }, "the job history");
    return node;
  }

  function jobs(ctx) {
    return el("div", { class: "view" }, dataTable({
      label: "Job runs", noun: "runs", filter: "Filter runs", signal: ctx.signal,
      empty: "No runs have been recorded for this instance.",
      expand: (row) => jobHistory(ctx, row),
      columns: [
        { label: "Job", cls: "mono", cell: (job) => code(job.id) },
        { label: "Provider", cell: () => "DigitalOcean" },
        { label: "Job state", cell: (job) => job.state || "—" },
        { label: "Provider state", cell: (job) => job.provider_state || "—" },
        { label: "Runner stage", cell: (job) => job.stage || "—" },
        { label: "Estimated cost", cls: "num", cell: (job) => estimatedUsd(job.estimated_usd) },
        { label: "Started", cls: "num", cell: (job) => ago(job.started_ms) },
      ],
      load: async () => (await ctx.op("digitalocean_droplet_jobs", { instance: ctx.instance.id })).jobs || [],
    }));
  }

  // ---- Email › Mailboxes (docs/plans/email.md): mailboxes on the left with their counts, the
  // message list in the middle, the message on the right. The message's HTML is the sender's
  // and is only ever shown in shell.mailFrame (sandboxed, nothing remote loads); its text is
  // shown plain; attachments and the original are saved through the storage object reader on
  // the data store (`storage_get_object` with `source: "data"`), never opened here.
  const CHECKS = { pass: ["Sender verified", "ok"], partial: ["Partly verified", "warn"], fail: ["Sender check failed", "bad"], none: ["Not verified", "plain"] };
  const senderCheck = (c) => { const [text, tone] = CHECKS[c] || CHECKS.none; return pill(text, tone); };
  const person = (a) => (a ? a.name || a.address || "—" : "—");
  const addresses = (list) => (Array.isArray(list) && list.length ? list.map((a) => (a.name ? `${a.name} <${a.address}>` : a.address)).join(", ") : "—");
  async function save(ctx, key, name, type) {
    const d = await ctx.op("storage_get_object", { instance: ctx.instance.id, key, source: "data" });
    saveFile(name, d.content_type || type, d.encoding, d.body);
  }
  function message(ctx, id) {
    const node = el("div", { class: "section", "aria-live": "polite" });
    loadInto(node, () => ctx.op("email_get", { instance: ctx.instance.id, id }), (m) => {
      const check = m.sightings?.[0]?.sender_check || "none";
      const files = (m.attachments || []).map((a) => el("li", {},
        button(a.filename || short(a.id, 12), () => save(ctx, a.key, a.filename || a.id, a.media_type).catch((e) => shell.toast?.(String(e.message || e), "bad")),
          { variant: "ghost", size: "sm", icon: "clip", title: `Download ${a.filename || a.id}` }),
        el("span", { class: "muted", text: ` ${a.media_type || ""} · ${bytes(a.size)}` })));
      const headers = (m.headers || []).map((h) => `${h.name}: ${h.decoded || h.value}`).join("\n");
      return [
        el("div", { class: "section-head" }, el("h2", { text: m.subject || "(no subject)" }), senderCheck(check)),
        facts([
          ["From", addresses(m.from)], ["To", addresses(m.to)], m.cc?.length && ["Cc", addresses(m.cc)],
          ["Date", m.date ? iso(m.date) : "—"], ["Received", iso(m.first_seen)], ["Size", bytes(m.size)],
          m.status === "failed" && ["Parsed", pill("failed: kept as received", "warn")],
        ]),
        m.html ? el("div", { class: "section" }, el("h3", { text: "Message" }), mailFrame(m.html.body, m.subject || "Message"),
          el("p", { class: "muted", text: "Shown sandboxed; remote images are blocked." })) : null,
        m.text ? el("div", { class: "section" }, el("h3", { text: m.html ? "Text" : "Message" }),
          el("pre", { class: "code", tabindex: "0", text: m.text })) : null,
        files.length ? el("div", { class: "section" }, el("h3", { text: "Attachments" }), el("ul", {}, files)) : null,
        el("div", { class: "toolbar" }, button("Download original (.eml)", () => save(ctx, m.key, `${m.id}.eml`, "message/rfc822"), { size: "sm" })),
        el("details", {}, el("summary", { text: "All headers" }), el("pre", { class: "code", text: headers })),
      ];
    }, "the message");
    return node;
  }
  function email(ctx) {
    const id = ctx.instance.id;
    let mailbox = null;
    let rows = [];
    let next = null;
    let more = false;
    const boxes = el("nav", { class: "boxes", "aria-label": "Mailboxes" });
    const reading = el("div", { class: "section" }, readOnly("Pick a message to read it."));
    const moreButton = button("Load more", () => { more = true; table.reload(); }, { size: "sm", hidden: true });
    function choose(name) {
      mailbox = name;
      rows = [];
      next = null;
      more = false;
      for (const b of boxes.querySelectorAll("button")) b.setAttribute("aria-pressed", String(b.dataset.mailbox === (name || "")));
      table.reload();
    }
    const table = dataTable({
      label: "Messages", noun: "messages", filter: "Filter this page", signal: ctx.signal,
      empty: "No mail here yet.",
      columns: [
        { label: "From", cell: (r) => [el("span", { text: person(r.from) }), el("span", { class: "sub mono", text: r.from?.address || "" })] },
        { label: "Subject", cell: (r) => button(r.subject || "(no subject)", () => reading.replaceChildren(message(ctx, r.id)),
          { variant: "ghost", size: "sm", title: "Read this message" }) },
        { label: "Date", cls: "num", cell: (r) => iso(r.date || r.seen_at) },
        { label: "Sender", cell: (r) => senderCheck(r.sender_check) },
        { label: "", cell: (r) => (r.attachments ? el("span", { title: `${r.attachments} attachment(s)` }, icon("clip")) : null) },
      ],
      load: async () => {
        const args = { instance: id };
        if (mailbox) args.mailbox = mailbox;
        if (more && next) args.cursor = next;
        const data = await ctx.op("email_list", args);
        rows = more ? rows.concat(data.messages || []) : data.messages || [];
        next = data.next || null;
        more = false;
        moreButton.hidden = !next;
        return rows;
      },
    });
    loadInto(boxes, () => ctx.op("email_mailboxes", { instance: id }), (d) => [
      button("All mail", () => choose(null), { variant: "ghost", size: "sm", "data-mailbox": "", "aria-pressed": "true" }),
      ...(d.mailboxes || []).map((m) => button([el("span", { text: m.mailbox }), el("span", { class: "muted", text: count(m.count) })],
        () => choose(m.mailbox), { variant: "ghost", size: "sm", "data-mailbox": m.mailbox, "aria-pressed": "false",
          title: m.address ? `${m.address}${m.forward ? `, forwarded to ${m.forward}` : ""}` : "Every address not listed" })),
    ], "the mailboxes");
    return el("div", { class: "view" },
      readOnly("Read-only. Everything sent to this instance’s domain is kept here; listed addresses have mailboxes of their own."),
      el("div", { class: "mail" }, boxes, el("div", {}, table, el("div", { class: "toolbar" }, moreButton)), reading));
  }

  register("storage", "buckets", storage);
  register("source", "repository", source);
  register("ci", "jobs", ci);
  register("metrics", "workers", metrics);
  register("jobs", "runs", jobs);
  register("ingest", "runs", ingest);
  register("email", "mailboxes", email);
})();
