// The Instances tab of the console: every instance pigeon runs, here or in a linked account,
// with a switcher between them. An instance is data: editing one shows exactly what changes and
// asks for a passkey bound to that record.
(() => {
  "use strict";

  const { el, post, section } = window.pigeonConsole;
  const { b64u, approve, say } = window.pigeonPasskey;

  const STORE_KEY = "pigeon.instance";
  const NEW = "__new__";

  // Storage that never throws (private mode, blocked cookies)
  function readStored() {
    try { return localStorage.getItem(STORE_KEY); } catch { return null; }
  }
  function writeStored(id) {
    try { localStorage.setItem(STORE_KEY, id); } catch { /* no storage: fine */ }
  }

  // Keep the chosen id in ?i= without touching path or hash
  function writeUrl(id) {
    try {
      const url = new URL(location.href);
      url.searchParams.set("i", id);
      history.replaceState(history.state, "", url.pathname + url.search + url.hash);
    } catch { /* ignore */ }
  }

  function remember(id) {
    writeStored(id);
    writeUrl(id);
  }

  // Pick the starting choice: ?i=, then storage, then the first instance
  function initialChoice(instances) {
    const has = (id) => id && instances.some((i) => i.id === id);
    let fromUrl = null;
    try { fromUrl = new URLSearchParams(location.search).get("i"); } catch { /* ignore */ }
    if (has(fromUrl)) return fromUrl;
    const stored = readStored();
    if (has(stored)) return stored;
    return instances.length ? instances[0].id : NEW;
  }

  // Build a dl of key/value pairs; values may be strings or element arrays
  function kv(pairs) {
    return el("dl", { class: "c-kv" },
      ...pairs.flatMap(([k, v]) => [
        el("dt", { text: k }),
        el("dd", {}, ...(Array.isArray(v) ? v : [v])),
      ]));
  }

  function fmtValue(v) {
    return v === null || v === undefined ? "none" : JSON.stringify(v);
  }

  function errorText(e) {
    if (typeof e === "string") return e;
    if (e && typeof e.message === "string") return e.message;
    return JSON.stringify(e);
  }

  // Header pill showing the link state
  function linkPill(inst) {
    if (inst.account && inst.account.kind === "home") {
      return el("span", { class: "pill ok", text: "this account" });
    }
    const status = inst.link && inst.link.status;
    if (status === "linked") return el("span", { class: "pill ok", text: "linked" });
    return el("span", { class: "pill warn", text: status || "not linked" });
  }

  function accountText(account) {
    if (!account) return "unknown";
    if (account.kind === "home") return "home";
    if (account.kind === "linked") {
      return account.account_id ? `linked (${account.account_id})` : "not yet linked";
    }
    return JSON.stringify(account);
  }

  function repoText(repo) {
    if (!repo) return "none";
    if (repo.kind === "github") {
      const app = repo.installation !== null && repo.installation !== undefined
        ? `App installation ${repo.installation}`
        : "App not installed";
      return `github: ${repo.full_name} (${app})`;
    }
    if (repo.kind === "artifacts") return `artifacts: ${repo.namespace}/${repo.repo}`;
    return JSON.stringify(repo);
  }

  function instanceCard(inst) {
    return el("div", { class: "card" },
      el("div", { class: "card-h" },
        el("strong", { text: inst.title || inst.id }),
        linkPill(inst)),
      kv([
        ["Id", inst.id],
        ["Account", accountText(inst.account)],
        ["Zones", inst.zones && inst.zones.length ? inst.zones.join(", ") : "none"],
        ["Repository", repoText(inst.repo)],
        ["Worker", inst.worker || "none (no sites yet)"],
        ["Version", String(inst.version)],
      ]));
  }

  // Link sends the browser to Cloudflare's consent screen; Unlink asks for the passkey.
  function linkRow(inst, ctx) {
    const status = el("p", { "data-status": "", role: "status" });
    const linked = inst.link && inst.link.status !== "not linked";
    const linkBtn = el("button", { class: "button", type: "button",
      text: linked ? "Link again" : "Link Cloudflare account" });
    linkBtn.onclick = async () => {
      linkBtn.disabled = true;
      try {
        const begin = await post("/linked/begin", { instance: inst.id });
        location.href = begin.url;
      } catch (e) {
        say(e.message, true, status);
        linkBtn.disabled = false;
      }
    };
    const kids = [linkBtn];
    if (linked) {
      const unlinkBtn = el("button", { class: "button secondary", type: "button", text: "Unlink" });
      unlinkBtn.onclick = async () => {
        unlinkBtn.disabled = true;
        try {
          say("Waiting for your passkey…", false, status);
          const begin = await post("/linked/unlink/begin", { instance: inst.id });
          const answer = await approve(begin.options);
          const done = await post("/linked/unlink/finish",
            { instance: inst.id, challenge_id: begin.challenge_id, ...answer });
          say(done.message || "Unlinked.", false, status);
          setTimeout(() => ctx.reload(), 1500);
        } catch (e) {
          say(e.message, true, status);
          unlinkBtn.disabled = false;
        }
      };
      kids.push(unlinkBtn);
    }
    const lines = [];
    if (inst.link && inst.link.status === "linked") {
      lines.push(el("p", { class: "muted", text:
        `Linked to ${inst.link.account_name || ""} (${inst.link.account_id}) by ${inst.link.linked_by || "someone"}.` }));
    } else if (inst.link && inst.link.why) {
      lines.push(el("p", { class: "muted", text: `Refreshing failed: ${inst.link.why}` }));
    }
    return el("div", { class: "layer", style: "gap:8px" }, ...lines, el("div", { class: "c-row" }, ...kids), status);
  }

  // One line per publishing target
  function targetLine(t, zone) {
    if (t.kind === "preview") {
      return el("li", {}, "preview: the Worker's workers.dev address");
    }
    if (t.kind === "zone") {
      return el("li", {},
        el("code", { text: t.hostname }),
        " (route ",
        el("code", { text: t.route }),
        t.enabled ? ") — on" : ") — off until the cutover");
    }
    if (t.kind === "subdomain") {
      return el("li", {},
        el("code", { text: t.label }),
        ".",
        el("code", { text: zone }));
    }
    return el("li", { text: JSON.stringify(t) });
  }

  function siteCard(site, zone) {
    const bucket = site.bucket + (site.jurisdiction === "eu" ? " (EU)" : "");
    return el("div", { class: "card" },
      el("div", { class: "card-h" }, el("strong", { text: site.name })),
      kv([
        ["Bucket", bucket],
        ["Build", site.build || "none"],
      ]),
      el("p", { class: "muted", text: "Targets" }),
      el("ul", {}, ...(site.targets || []).map((t) => targetLine(t, zone))));
  }

  // The JSON editor with the passkey save flow
  function buildEditor(record, ctx, onSavedId) {
    const textarea = el("textarea", {
      rows: 18,
      style: "width:100%;font-family:var(--mono,monospace)",
      "aria-label": "Instance record (JSON)",
    });
    textarea.value = JSON.stringify(record, null, 2);

    const status = el("p", { "data-status": "", role: "status" });
    const results = el("div", {});
    const saveBtn = el("button", { class: "button", type: "button", text: "Save with passkey" });

    saveBtn.onclick = async () => {
      results.replaceChildren();

      // Parse first; a typo should not cost a passkey prompt
      let parsed;
      try {
        parsed = JSON.parse(textarea.value);
      } catch (e) {
        say("That is not valid JSON: " + e.message, true, status);
        return;
      }

      saveBtn.disabled = true;
      let saved = false;
      try {
        const text = JSON.stringify(parsed);
        const digest = b64u.enc(
          await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));

        const begin = await post("/instance/put/begin", { record: text, digest });

        // Server-side validation problems
        if (begin.errors && begin.errors.length) {
          results.replaceChildren(
            el("ul", {}, ...begin.errors.map((e) => el("li", { text: errorText(e) }))));
          say("The server did not accept this record.", true, status);
          return;
        }

        // Show exactly what the passkey will approve
        const changes = begin.changes || [];
        results.replaceChildren(
          el("h4", { text: "You are approving these changes" }),
          el("ul", {},
            ...(changes.length
              ? changes.map((c) => el("li", {
                  text: `${c.path}: ${fmtValue(c.from)} → ${fmtValue(c.to)}`,
                }))
              : [el("li", { text: "No changes" })])));

        say("Waiting for your passkey…", false, status);
        const answer = await approve(begin.options);

        const done = await post("/instance/put/finish", {
          record: text,
          challenge_id: begin.challenge_id,
          ...answer,
        });

        saved = true;
        say(done.message || "Saved.", false, status);
        if (typeof parsed.id === "string") onSavedId(parsed.id);
        setTimeout(() => ctx.reload(), 1500);
      } catch (e) {
        say(e.message, true, status);
      } finally {
        // After a successful save keep it disabled until the reload
        if (!saved) saveBtn.disabled = false;
      }
    };

    return el("div", { class: "card" },
      el("div", { class: "layer", style: "gap:16px" },
        textarea,
        el("div", { class: "c-row" }, saveBtn),
        status,
        results));
  }

  section("instances", (body, data, ctx) => {
    const instances = Array.isArray(data.instances) ? data.instances : [];
    let current = initialChoice(instances);

    // Switcher
    const selectId = "instances-switcher";
    const select = el("select", { class: "c-select", id: selectId },
      ...instances.map((i) => el("option", {
        value: i.id,
        text: `${i.title || i.id} (${i.id})`,
      })),
      el("option", { value: NEW, text: "New instance…" }));
    select.value = current;

    const switcher = el("div", { class: "c-row" },
      el("label", { for: selectId, text: "Instance" }),
      select);

    const view = el("div", { class: "layer", style: "gap:16px" });

    // Save of a new instance should land on that instance after reload
    const onSavedId = (id) => { remember(id); };

    function renderView() {
      view.replaceChildren();

      if (!instances.length) {
        view.append(el("p", { class: "muted", text: "No instances yet." }));
      }

      if (current === NEW) {
        view.append(buildEditor(data.template ?? {}, ctx, onSavedId));
        return;
      }

      const inst = instances.find((i) => i.id === current);
      if (!inst) return;

      view.append(instanceCard(inst));
      if (inst.account && inst.account.kind === "linked") view.append(linkRow(inst, ctx));
      for (const site of inst.sites || []) view.append(siteCard(site, data.zone));

      const editorArea = el("div", {});
      const editBtn = el("button", { class: "button secondary", type: "button", text: "Edit" });
      editBtn.onclick = () => {
        const { link, ...record } = inst; // the link state is not part of the record
        editBtn.hidden = true;
        editorArea.replaceChildren(buildEditor(record, ctx, onSavedId));
      };
      view.append(el("div", { class: "c-row" }, editBtn), editorArea);
    }

    select.onchange = () => {
      current = select.value;
      if (current !== NEW) remember(current);
      renderView();
    };

    // Reflect the starting choice in the URL and storage
    if (current !== NEW) remember(current);

    renderView();

    body.replaceChildren(
      el("div", { class: "layer", style: "gap:16px" },
        switcher,
        view,
        el("p", {
          class: "muted",
          text: "Instances are data: changing one needs your passkey, and pigeon touches only what an instance names.",
        })));
  });
})();
