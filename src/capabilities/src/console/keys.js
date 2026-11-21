// The Keys section of the console's Settings tab: the platform's secrets (set or replaced, each
// approved with a passkey bound to that name and value) and the proxy credentials cloud sessions
// hold (issued once, revoked). A secret's value is never shown, logged or kept: inputs are
// cleared after a write, and an issued credential stays on the page only until "Done".
(() => {
  const { el, post, section, expiryPill } = window.pigeonConsole;
  const { b64u, approve, say } = window.pigeonPasskey;

  const pad = (n) => String(n).padStart(2, "0");
  const stamp = (iso) => {
    const d = new Date(iso);
    return isNaN(d) ? "unknown" : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  const day = (ms) => new Date(ms).toISOString().slice(0, 10);

  // begin → passkey prompt → finish; the finish body carries the challenge and the assertion.
  async function ceremony(path, beginBody, finishBody) {
    const begin = await post(`${path}/begin`, beginBody);
    const answer = await approve(begin.options);
    return post(`${path}/finish`, { ...finishBody, challenge_id: begin.challenge_id, ...answer });
  }

  section("credentials", (body, data, ctx) => {
    const secrets = (data.secrets && data.secrets.secrets) || [];
    const tokens = (data.proxy && data.proxy.tokens) || [];
    const secretExpiry = (data.secrets && data.secrets.expiry) || [];
    const proxyExpiry = (data.proxy && data.proxy.expiry) || [];
    const expiryOf = (name) => secretExpiry.find((r) => r.name === name);

    function secretCard(s) {
      const set = !!s.set;
      const status = el("p", { "data-status": "", role: "status" });
      const input = el("input", {
        class: "c-input", type: "password", autocomplete: "off", spellcheck: "false",
        "aria-label": `New value for ${s.name}`, placeholder: set ? "New value" : "Value",
        onkeydown: (e) => { if (e.key === "Enter") save(); },
      });
      const btn = el("button", { class: "button", type: "button", text: set ? "Replace" : "Set", onclick: () => save() });

      async function save() {
        const value = input.value.trim();
        if (!value) return say("Type the new value first.", true, status);
        btn.disabled = true;
        say("Waiting for your passkey…", false, status);
        try {
          const digest = b64u.enc(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
          const done = await ceremony("/secrets/put", { name: s.name, digest }, { name: s.name, value });
          input.value = "";
          say(done.message, false, status);
          setTimeout(ctx.reload, 1500);
        } catch (e) {
          say(e.message, true, status);
        } finally {
          btn.disabled = false;
        }
      }

      return el("article", { class: set ? "card" : "card accent", id: s.name },
        el("div", { class: "card-h" },
          el("h3", { text: s.name }),
          el("div", { class: "pills" },
            set
              ? el("span", { class: "pill", text: `set · changed ${s.modified ? stamp(s.modified) : "unknown"}` })
              : el("span", { class: "pill accent", text: "missing" }),
            set && expiryOf(s.name) ? expiryPill(expiryOf(s.name)) : null)),
        el("p", { text: `${s.owner} · ${s.purpose}` }),
        el("div", { class: "c-row" }, input, btn),
        status);
    }

    const declared = secrets.filter((s) => s.owner);
    const others = secrets.filter((s) => !s.owner);

    const secretsPart = el("div", { class: "layer" },
      el("h3", { text: "Secrets" }),
      el("p", { class: "muted", style: "margin:0", text: "Values are never shown. Setting one asks for your passkey, bound to that name and value." }),
      el("div", { class: "layer" }, ...declared.map(secretCard)));
    if (others.length) {
      secretsPart.append(el("div", { class: "layer" },
        el("span", { class: "eyebrow", text: "Other secrets in the store" }),
        ...others.map((s) => el("article", { class: "card" },
          el("div", { class: "card-h" }, el("h3", { text: s.name })),
          el("p", { text: "No capability declares this one, so it is listed only." })))));
    }

    const issueStatus = el("p", { "data-status": "", role: "status" });
    const result = el("div", { class: "layer" });
    const label = el("input", { class: "c-input", maxlength: "64", "aria-label": "Label", placeholder: "Label, such as ci-gateway session" });
    const days = el("input", { class: "c-input", type: "number", min: "1", max: "90", value: "30", "aria-label": "Days", style: "flex:0 0 90px" });
    const issueBtn = el("button", { class: "button", type: "button", text: "Issue", onclick: () => issue() });

    // The issued value, shown once; "Done" drops it from the page.
    function reveal(value) {
      const code = el("code", { style: "display:block;overflow-wrap:anywhere;padding:10px 12px", text: value });
      const copy = el("button", {
        class: "button c-quiet", type: "button", text: "Copy",
        onclick: async () => {
          try {
            await navigator.clipboard.writeText(code.textContent);
            copy.textContent = "Copied";
          } catch {
            const r = document.createRange();
            r.selectNodeContents(code);
            const sel = getSelection();
            sel.removeAllRanges();
            sel.addRange(r);
          }
        },
      });
      const finish = el("button", {
        class: "button c-quiet", type: "button", text: "Done",
        onclick: () => { code.textContent = ""; result.replaceChildren(); ctx.reload(); },
      });
      result.replaceChildren(code, el("div", { class: "c-row" }, copy, finish));
    }

    async function issue() {
      const text = label.value.trim();
      const n = Number(days.value);
      if (!text) return say("Give the credential a label.", true, issueStatus);
      if (!Number.isInteger(n) || n < 1 || n > 90) return say("Days must be a whole number from 1 to 90.", true, issueStatus);
      issueBtn.disabled = true;
      say("Waiting for your passkey…", false, issueStatus);
      try {
        const done = await ceremony("/tokens/issue", { label: text, days: n }, { label: text, days: n });
        label.value = "";
        say(done.message, false, issueStatus);
        reveal(done.token || "");
      } catch (e) {
        say(e.message, true, issueStatus);
      } finally {
        issueBtn.disabled = false;
      }
    }

    const listStatus = el("p", { "data-status": "", role: "status" });

    function tokenRow(t) {
      let armed = false, timer = 0;
      const btn = el("button", { class: "button c-quiet", type: "button", text: "Revoke", onclick: () => click() });
      const reset = () => { armed = false; btn.textContent = "Revoke"; };

      // Two clicks within 5 s: the first only arms the button.
      async function click() {
        if (!armed) {
          armed = true;
          btn.textContent = "Confirm revoke";
          timer = setTimeout(reset, 5000);
          return;
        }
        clearTimeout(timer);
        btn.disabled = true;
        say("Waiting for your passkey…", false, listStatus);
        try {
          await ceremony("/tokens/revoke", { id: t.id }, { id: t.id });
          ctx.reload();
        } catch (e) {
          say(e.message, true, listStatus);
          reset();
          btn.disabled = false;
        }
      }

      const row = proxyExpiry.find((r) => r.owner.startsWith(`${t.id} `));
      return el("li", {},
        el("div", {},
          el("strong", { text: t.label }),
          el("p", { class: "muted", style: "margin:0", text: `${t.id} · issued by ${t.person} · expires ${day(t.expires_at)}` })),
        row ? expiryPill(row) : null,
        btn);
    }

    // Expired credentials leave the live list; they stay here, flagged, for two weeks.
    const ended = proxyExpiry.filter((r) => r.status === "expired");

    const credentials = el("div", { class: "layer", id: "credentials" },
      el("h3", { text: "Proxy credentials" }),
      el("p", { class: "muted", style: "margin:0", text: "A proxy credential lets a cloud session reach the platform's git and Cloudflare proxies, and nothing else. Each is shown once, when it is issued." }),
      el("article", { class: "card" },
        el("span", { class: "eyebrow", text: "Issue a credential" }),
        el("div", { class: "c-row" }, label, days, issueBtn),
        issueStatus,
        result),
      tokens.length
        ? el("ul", { class: "c-list c-expiry" }, ...tokens.map(tokenRow))
        : el("p", { class: "muted", text: "No proxy credentials yet." }),
      ended.length
        ? el("div", { class: "layer" },
          el("span", { class: "eyebrow", text: "Expired in the last two weeks" }),
          el("ul", { class: "c-list c-expiry" }, ...ended.map((r) => el("li", {},
            el("div", {}, el("strong", { text: r.name }), el("p", { class: "muted", style: "margin:0", text: r.owner })),
            expiryPill(r)))))
        : null,
      listStatus);

    body.replaceChildren(el("div", { class: "layer", style: "gap:32px" }, secretsPart, credentials));
  });
})();
