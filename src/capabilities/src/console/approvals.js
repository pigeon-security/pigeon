// The Approvals section of the console: what waits for the signed-in person's passkey.
// Landings in awaiting_approval, and single tool calls the policy gates (approvals capability).
// Each is approved with a single passkey ceremony bound to it: a landing by its id, a tool call
// by its approval's id and the call's digest. A tool call can also be denied.
(() => {
  const { el, post, section } = window.pigeonConsole;
  const { approve, say } = window.pigeonPasskey;
  const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "");
  const statusLine = () => el("p", { "data-status": "", role: "status" });
  const short = (c) => (c || "").slice(0, 12);
  const branch = (b) => (b || "").replace(/^refs\/heads\//, "");

  // How long the landing has waited, from the start of the wait or from creation.
  function waiting(l) {
    const since = l.awaiting_since || l.created_at;
    const mins = Math.max(0, Math.floor((Date.now() - since) / 60000));
    return mins >= 60 ? `waiting ${Math.floor(mins / 60)} h` : `waiting ${mins} min`;
  }

  async function write(action, target) {
    const begin = await post(`/console/write/${action}/begin`, { target });
    const answer = await approve(begin.options);
    return post(`/console/write/${action}/finish`, { target, challenge_id: begin.challenge_id, ...answer });
  }

  async function run(btn, action, target, status, ctx) {
    btn.disabled = true;
    say("Waiting for your passkey…", false, status);
    try {
      const done = await write(action, target);
      say(done.message || "Done.", false, status);
      setTimeout(() => ctx.reload(), 1500);
    } catch (e) {
      say(e.message, true, status);
    } finally {
      btn.disabled = false;
    }
  }

  // One card per landing that waits for this person.
  function approvalCard(l, ctx) {
    const status = statusLine();
    const btn = el("button", { class: "button", type: "button", text: "Approve with passkey",
      onclick: () => run(btn, "land-approve", l.id, status, ctx) });
    const rows = [
      ["Why", l.needs_approval],
      ["Commit", short(l.commit)],
      ["Asked by", l.by],
      ["Started", when(l.created_at)],
      l.detail && ["Detail", l.detail],
    ].filter(Boolean);
    return el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: `Land ${branch(l.branch)}` }),
        el("span", { class: "pill warn", text: waiting(l) })),
      el("dl", { class: "c-kv" }, ...rows.flatMap(([k, v]) => [el("dt", { text: k }), el("dd", { text: String(v) })])),
      el("div", { class: "c-row" }, btn),
      status);
  }

  const until = (ms) => {
    const mins = Math.max(0, Math.ceil((ms - Date.now()) / 60000));
    return `expires in ${mins} min`;
  };

  // One card per tool call that waits for this person: the call, as its caller asked it.
  function callCard(c, ctx) {
    const status = statusLine();
    const approveBtn = el("button", { class: "button", type: "button", text: "Approve with passkey",
      onclick: () => run(approveBtn, "approval-approve", c.id, status, ctx) });
    const denyBtn = el("button", { class: "button c-quiet", type: "button", text: "Deny",
      onclick: () => run(denyBtn, "approval-deny", c.id, status, ctx) });
    const args = JSON.stringify(c.args || {}, null, 2);
    const rows = [
      ["Asked by", c.acting_for ? `${c.caller}, acting for ${c.acting_for}` : c.caller],
      ["Asked", when(c.created_ms)],
      ["Call digest", c.digest],
    ];
    return el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: `Run ${c.tool}` }),
        el("span", { class: "pill warn", text: until(c.expires_ms) })),
      el("dl", { class: "c-kv" }, ...rows.flatMap(([k, v]) => [el("dt", { text: k }), el("dd", { text: String(v || "") })])),
      el("pre", { class: "c-code", text: args }),
      el("p", { class: "muted", text: "Approving lets this exact call run once, when its caller calls again within 10 minutes." }),
      el("div", { class: "c-row" }, approveBtn, denyBtn),
      status);
  }

  // One card per instance draft an agent proposed: exactly what changes, applied on approval.
  function draftCard(d, ctx) {
    const status = statusLine();
    const approveBtn = el("button", { class: "button", type: "button", text: "Approve with passkey",
      onclick: () => run(approveBtn, "approval-approve", d.id, status, ctx) });
    const denyBtn = el("button", { class: "button c-quiet", type: "button", text: "Deny",
      onclick: () => run(denyBtn, "approval-deny", d.id, status, ctx) });
    const show = (v) => (v === null || v === undefined ? "none" : JSON.stringify(v));
    const changes = ((d.summary && d.summary.changes) || []).map((c) =>
      el("li", { text: `${c.path}: ${show(c.from)} → ${show(c.to)}` }));
    return el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: `Change instance ${d.instance}` }),
        el("span", { class: "pill warn", text: until(d.expires_ms) })),
      el("dl", { class: "c-kv" },
        el("dt", { text: "From version" }), el("dd", { text: String((d.summary || {}).version_from ?? 0) }),
        el("dt", { text: "Asked" }), el("dd", { text: when(d.created_ms) })),
      el("ul", {}, ...changes),
      el("p", { class: "muted", text: "Approving applies exactly these changes now, unless the instance changed since this was proposed." }),
      el("div", { class: "c-row" }, approveBtn, denyBtn),
      status);
  }

  // One card per plan for a linked account: each step, applied once after approval.
  function planCard(p, ctx) {
    const status = statusLine();
    const approveBtn = el("button", { class: "button", type: "button", text: "Approve with passkey",
      onclick: () => run(approveBtn, "approval-approve", p.id, status, ctx) });
    const denyBtn = el("button", { class: "button c-quiet", type: "button", text: "Deny",
      onclick: () => run(denyBtn, "approval-deny", p.id, status, ctx) });
    const summary = p.summary || {};
    const steps = (summary.plan || []).map((line) => el("li", { text: String(line) }));
    return el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: `Plan for ${p.instance}` }),
        el("span", { class: "pill warn", text: until(p.expires_ms) })),
      el("dl", { class: "c-kv" },
        el("dt", { text: "Account" }), el("dd", { text: String(summary.account_id || "") }),
        el("dt", { text: "Record version" }), el("dd", { text: String(summary.version ?? "") }),
        el("dt", { text: "Asked" }), el("dd", { text: when(p.created_ms) }),
        el("dt", { text: "Plan digest" }), el("dd", { text: p.digest })),
      el("ol", {}, ...steps),
      el("p", { class: "muted", text: "Approving lets exactly these steps run once. If the account no longer gives the same plan, nothing runs. Nothing is deleted." }),
      el("div", { class: "c-row" }, approveBtn, denyBtn),
      status);
  }

  section("approvals", (body, data, ctx) => {
    // With several owners the data comes by capability name; with one, as it is.
    const landings = (data.land || data).landings || [];
    const calls = (data.approvals || data).calls || [];
    const drafts = (data.approvals || data).drafts || [];
    const plans = (data.approvals || data).plans || [];
    const cards = [...calls.map((c) => callCard(c, ctx)), ...drafts.map((d) => draftCard(d, ctx)),
      ...plans.map((p) => planCard(p, ctx)),
      ...landings.map((l) => approvalCard(l, ctx))];
    body.append(el("div", { class: "layer", style: "gap:16px" },
      cards.length
        ? el("div", { class: "layer", style: "gap:12px" }, ...cards)
        : el("p", { class: "muted", text: "Nothing waits for you. Landings and tool calls that need your passkey appear here, and this tab disappears when none do." })));
  });
})();
