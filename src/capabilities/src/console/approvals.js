// The Approvals section of the console: landings that wait for the signed-in person's passkey.
// Only landings in awaiting_approval are listed, with no settings and no history.
// Each one is approved with a single passkey ceremony bound to that landing.
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

  section("approvals", (body, data, ctx) => {
    const list = data.landings || [];
    body.append(el("div", { class: "layer", style: "gap:16px" },
      list.length
        ? el("div", { class: "layer", style: "gap:12px" }, ...list.map((l) => approvalCard(l, ctx)))
        : el("p", { class: "muted", text: "Nothing waits for you. Landings that need your passkey appear here, and this tab disappears when none do." })));
  });
})();
