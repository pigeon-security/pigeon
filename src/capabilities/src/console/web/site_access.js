// Visitor management follows the app's selected instance.
(() => {
  "use strict";
  const { register, el, card } = window.pigeonApp;
  const { post, approve, say } = window.pigeonPasskey;

  function visitorPanel(selection) {
    const status = el("p", { "data-status": "", role: "status" });
    const visitors = el("div", {});
    const input = el("input", {
      class: "c-input", maxlength: "64", "aria-label": `Visitor name for ${selection.host}`,
      placeholder: "Visitor name",
    });
    input.maxLength = 64;
    input.required = true;
    const invite = el("button", { class: "button", type: "submit", text: "Create visitor invite" });
    const form = el("form", { class: "c-row", hidden: true }, input, invite);
    const result = el("div", { class: "layer" });
    const load = el("button", { class: "button secondary", type: "button", text: "Show visitors" });

    async function refresh() {
      const data = await post("/console/site-access/list", selection);
      const rows = data.visitors || [];
      visitors.replaceChildren(rows.length
        ? el("ul", { class: "c-list" }, ...rows.map((visitor) => el("li", {},
          el("strong", { text: visitor.name }),
          el("span", { class: "muted", text: visitor.enrolled ? "Passkey registered" : "Awaiting enrollment" }))))
        : el("p", { class: "muted", text: "No visitors enrolled or invited for this hostname." }));
      form.hidden = !data.can_invite;
      if (!data.can_invite) visitors.append(el("p", { class: "muted",
        text: "The allowlist for this hostname does not admit new site visitors." }));
    }

    load.onclick = async () => {
      load.disabled = true;
      try {
        await refresh();
        load.hidden = true;
        say("", false, status);
      } catch (e) {
        say(e.message, true, status);
      } finally {
        load.disabled = false;
      }
    };

    form.onsubmit = async (event) => {
      event.preventDefault();
      if (invite.disabled) return;
      const name = input.value.trim();
      if (!name) { input.focus(); return; }
      invite.disabled = true;
      result.replaceChildren();
      say("Waiting for your passkey…", false, status);
      try {
        const target = { ...selection, name };
        const begin = await post("/console/site-access/invite/begin", target);
        const assertion = await approve(begin.options);
        const done = await post("/console/site-access/invite/finish", {
          ...target, challenge_id: begin.challenge_id, ...assertion,
        });
        const link = el("input", { class: "c-input", value: done.enrol,
          "aria-label": "Visitor enrollment link" });
        link.readOnly = true;
        const copy = el("button", { class: "button secondary", type: "button", text: "Copy link" });
        copy.onclick = async () => {
          try {
            await navigator.clipboard.writeText(done.enrol);
            copy.textContent = "Copied";
          } catch {
            link.focus();
            link.select();
          }
        };
        result.replaceChildren(link, copy);
        input.value = "";
        say(`Invite created. The link expires in ${done.expires_in_minutes} minutes and can be used once.`, false, status);
        try {
          await refresh();
        } catch {
          load.textContent = "Refresh visitors";
          load.hidden = false;
        }
      } catch (e) {
        say(e.message, true, status);
      } finally {
        invite.disabled = false;
      }
    };

    return card(el("h2", { text: selection.host }),
      el("p", { class: "app-muted", text: selection.site }), load, visitors, form, status, result);
  }

  async function renderSiteAccess({ instance }) {
    if (!instance) return card(el("p", { text: "Select an instance to manage site visitors." }));
    const { hosts } = await post("/console/site-access/hosts", { instance: instance.id });
    if (!hosts.length) return card(el("p", { text: "This instance has no applied passkey-protected site hostnames." }));
    return el("div", { class: "layer" },
      ...hosts.map(({ host, site }) => visitorPanel({ instance: instance.id, host, site })));
  }
  register("site-access", renderSiteAccess);
  register("linked-site-access", renderSiteAccess);
})();
