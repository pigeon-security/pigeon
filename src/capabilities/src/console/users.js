// The Access tab of the console has two sections. People lists each person and their passkeys, and
// lets you invite someone new. Delegation lists the apps (MCP clients and agents) acting for each
// person, and any that act for no one. Every write (removing a passkey, disconnecting an app,
// inviting a person) is one passkey ceremony bound to what it changes.
(() => {
  const { el, post, section } = window.pigeonConsole;
  const { approve, say } = window.pigeonPasskey;
  const day = (ms) => new Date(ms).toISOString().slice(0, 10);
  const statusLine = () => el("p", { "data-status": "", role: "status" });

  async function write(action, target) {
    const begin = await post(`/console/write/${action}/begin`, { target });
    const answer = await approve(begin.options);
    return post(`/console/write/${action}/finish`, { target, challenge_id: begin.challenge_id, ...answer });
  }

  // after(done) replaces the delayed reload: an invite keeps its card open until "Done".
  async function run(btn, action, target, status, ctx, after) {
    btn.disabled = true;
    say("Waiting for your passkey…", false, status);
    try {
      const done = await write(action, target);
      say(done.message, false, status);
      if (after) after(done); else setTimeout(() => ctx.reload(), 1500);
    } catch (e) {
      say(e.message, true, status);
    } finally {
      btn.disabled = false;
    }
  }

  // Two clicks within 5 s: the first only arms the button.
  function destructive(label, action, target, status, ctx) {
    let armed = null;
    const btn = el("button", { class: "button c-quiet", type: "button", text: label, onclick: () => {
      if (btn.disabled) return;
      if (!armed) {
        btn.textContent = `Confirm ${label.toLowerCase()}`;
        armed = setTimeout(() => { armed = null; btn.textContent = label; }, 5000);
        return;
      }
      clearTimeout(armed); armed = null; btn.textContent = label;
      run(btn, action, target, status, ctx);
    } });
    return btn;
  }

  const muted = (text) => el("p", { class: "muted", style: "margin:0", text });
  const grow = (...kids) => el("div", { style: "flex:1;min-width:0" }, ...kids);

  function appList(apps, status, ctx) {
    if (!apps.length) return muted("No apps yet. Add pigeon as a connector in Claude, then allow it with your passkey.");
    return el("ul", { class: "c-list" }, ...apps.map((a) => el("li", {},
      el("span", { style: "width:8px;height:8px;border-radius:50%;background:var(--good);flex-shrink:0" }),
      grow(el("strong", { text: a.name }),
        muted(`${a.tokens} active token${a.tokens === 1 ? "" : "s"}${a.linked_at ? ` · linked ${day(a.linked_at)}` : ""}`)),
      destructive("Disconnect", "app-disconnect", a.id, status, ctx))));
  }

  const block = (label, content) =>
    el("div", { class: "layer", style: "gap:6px" }, el("span", { class: "eyebrow", text: label }), content);

  function personCard(p, ctx) {
    const status = statusLine();
    const avatar = p.avatar
      ? el("img", { class: "c-avatar", alt: "", src: p.avatar, style: "width:32px;height:32px" })
      : el("span", { class: "c-avatar c-initials", "aria-hidden": "true", style: "width:32px;height:32px;font-size:12px", text: p.initials });
    const many = p.passkeys.length > 1;
    const keys = el("ul", { class: "c-list" }, ...p.passkeys.map((k) => el("li", {},
      grow(el("strong", { text: k.name }),
        muted(`added ${day(k.created_at)} · ${k.last_used_at ? `last used ${day(k.last_used_at)}` : "never used"}`)),
      many && destructive("Remove", "passkey-remove", k.id, status, ctx))));
    return el("article", { class: "card", style: "gap:14px" },
      el("div", { class: "card-h" }, avatar, el("h3", { text: p.name }), el("span", { class: "pill accent", text: "person" })),
      block(`Passkeys · ${p.passkeys.length}`, keys),
      status);
  }

  // One card per person who has at least one app acting for them.
  function delegationCard(p, ctx) {
    const status = statusLine();
    return el("article", { class: "card", style: "gap:14px" },
      el("h3", { text: `Acting for ${p.name}` }),
      appList(p.apps, status, ctx),
      status);
  }

  function inviteCard(ctx) {
    const status = statusLine();
    const input = el("input", { class: "c-input", maxlength: "64", "aria-label": "Name", placeholder: "Their name" });
    const result = el("div", { class: "layer", style: "gap:8px", hidden: true });
    const card = el("article", { class: "card", hidden: true });
    const create = el("button", { class: "button", type: "button", text: "Create invite link", onclick: () => {
      const name = input.value.trim();
      if (!name) { say("Give them a name first.", true, status); input.focus(); return; }
      run(create, "invite", name, status, ctx, (done) => {
        if (!done.link) return;
        const code = el("code", { style: "display:block;overflow-wrap:anywhere;padding:10px 12px", text: done.link });
        const select = () => {
          const r = document.createRange();
          r.selectNodeContents(code);
          const s = window.getSelection();
          s.removeAllRanges(); s.addRange(r);
        };
        const copy = el("button", { class: "button c-quiet", type: "button", text: "Copy", onclick: () => {
          try {
            navigator.clipboard.writeText(done.link).then(() => { copy.textContent = "Copied"; }, select);
          } catch { select(); }
        } });
        const doneBtn = el("button", { class: "button c-quiet", type: "button", text: "Done", onclick: () => {
          card.hidden = true;
          ctx.reload();
        } });
        result.replaceChildren(code, el("div", { class: "c-row" }, copy, doneBtn));
        result.hidden = false;
      });
    } });
    card.append(el("div", { class: "c-row" }, input, create), status, result);
    return card;
  }

  section("people", (body, data, ctx) => {
    const invite = inviteCard(ctx);
    // The section's own heading names it; the row holds only the invite button.
    const head = el("div", { style: "display:flex;justify-content:flex-end;align-items:center;flex-wrap:wrap;gap:12px" },
      el("button", { class: "button c-quiet", type: "button", text: "Invite a person", onclick: () => {
        invite.hidden = !invite.hidden;
        if (!invite.hidden) invite.querySelector("input").focus();
      } }));
    const root = el("div", { class: "layer", style: "gap:16px" }, head, invite,
      ...(data.people || []).map((p) => personCard(p, ctx)));
    body.append(root);
  });

  section("delegation", (body, data, ctx) => {
    const root = el("div", { class: "layer", style: "gap:16px" });
    const acting = (data.people || []).filter((p) => p.apps && p.apps.length);
    const unlinked = data.unlinked || [];
    root.append(...acting.map((p) => delegationCard(p, ctx)));
    if (unlinked.length) {
      const status = statusLine();
      root.append(el("article", { class: "card accent" },
        el("h3", { text: "Not acting for anyone" }), appList(unlinked, status, ctx), status));
    }
    if (!acting.length && !unlinked.length) root.append(appList([], null, ctx));
    body.append(root);
  });
})();
