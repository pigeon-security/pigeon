// The Landing section of the console's Settings tab: whether every landing waits for a person,
// and the latest landings, each with its state and steps. Approving a landing, and changing the
// setting, is one passkey ceremony bound to that landing, or to the new setting.
(() => {
  const { el, post, section } = window.pigeonConsole;
  const { approve, say } = window.pigeonPasskey;
  const when = (ms) => (ms ? new Date(ms).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "");
  const statusLine = () => el("p", { "data-status": "", role: "status" });
  const short = (c) => (c || "").slice(0, 12);
  const branch = (b) => (b || "").replace(/^refs\/heads\//, "");
  const tone = { landed: "good", awaiting_approval: "accent", testing: "", merging: "", deploying: "" };

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

  function settingCard(approval, ctx) {
    const status = statusLine();
    const on = !!(approval && approval.on);
    const btn = el("button", { class: "button c-quiet", type: "button",
      text: on ? "Stop requiring approval" : "Require approval",
      onclick: () => run(btn, "land-approval", on ? "off" : "on", status, ctx) });
    const said = on
      ? "Every landing started from now on waits for a person's passkey after its tests pass, before main moves."
      : "Landings go from tests to main and the deploy without a person, unless they change what the gate runs (.pigeon, ci, the pinned wrangler).";
    const by = approval && approval.by ? ` Set by ${approval.by}, ${when(approval.at)}.` : "";
    return el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: "Require approval to land" }),
        el("span", { class: `pill ${on ? "accent" : ""}`, text: on ? "on" : "off" })),
      el("p", { class: "muted", style: "margin:0", text: said + by }),
      el("div", { class: "c-row" }, btn), status);
  }

  function landingCard(l, ctx) {
    const status = statusLine();
    const rows = [
      ["Branch", branch(l.branch)],
      ["Commit", short(l.commit)],
      ["By", l.by],
      ["Started", when(l.created_at)],
      l.finished_at && ["Finished", when(l.finished_at)],
      l.needs_approval && ["Needs approval", l.needs_approval],
      l.approved_by && ["Approved by", l.approved_by],
      l.detail && ["Detail", l.detail],
    ].filter(Boolean);
    const card = el("article", { class: "card", style: "gap:10px" },
      el("div", { class: "card-h" }, el("h3", { text: `Landing ${l.id}` }),
        el("span", { class: `pill ${tone[l.state] || ""}`, text: l.state.replace(/_/g, " ") })),
      el("dl", { class: "c-kv" }, ...rows.flatMap(([k, v]) => [el("dt", { text: k }), el("dd", { text: String(v) })])));
    if (l.state === "awaiting_approval") {
      const btn = el("button", { class: "button", type: "button", text: `Approve ${short(l.commit)}`,
        onclick: () => run(btn, "land-approve", l.id, status, ctx) });
      card.append(el("div", { class: "c-row" }, btn), status);
    }
    return card;
  }

  section("landing", (body, data, ctx) => {
    const list = data.landings || [];
    body.append(el("div", { class: "layer", style: "gap:16px" },
      settingCard(data.approval, ctx),
      list.length
        ? el("div", { class: "layer", style: "gap:12px" }, ...list.map((l) => landingCard(l, ctx)))
        : el("p", { class: "muted", text: "No landings yet. A session lands its branch with the land tool." })));
  });
})();
