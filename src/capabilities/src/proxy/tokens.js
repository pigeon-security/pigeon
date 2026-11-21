// The proxy credentials page: sign in with a passkey to see the live credentials, then issue
// or revoke one, each approved with a passkey of its own. A new credential is shown once.
// Its own scope: passkey.js declares the same names at the top level of the shared global scope.
(() => {
const { post, approve, say } = window.pigeonPasskey;

const list = document.querySelector("[data-list]");
const issue = document.querySelector("[data-issue]");
const viewButton = document.querySelector("button[data-view]");

const el = (tag, attrs = {}, ...children) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "text") node.textContent = v;
    else node.setAttribute(k, v);
  }
  node.append(...children);
  return node;
};

const inputBase =
  "box-sizing:border-box;padding:10px 12px;border-radius:10px;border:1px solid var(--line);background:var(--chip);color:var(--ink);font:var(--type-small)";
const textStyle = `width:100%;${inputBase}`;
const monoStyle = `width:100%;${inputBase};font-family:var(--font-mono)`;
const rowStyle = "display:flex;gap:8px;align-items:center";

// The signed-in person's name, for rows issued on this page.
let me = null;

const stamp = (ms) => new Date(ms).toISOString().slice(0, 16).replace("T", " ");

const emptyNote = () => el("p", { "data-empty": "", text: "No live proxy credentials." });

const showEmptyIfNeeded = () => {
  if (!list.querySelector("article")) list.replaceChildren(emptyNote());
};

const tokenCard = (token, info) => {
  const status = el("p", { "data-status": "", role: "status" });
  const button = el("button", { type: "button", class: "button", text: "Revoke" });
  const card = el(
    "article",
    { class: "card" },
    el("div", { class: "card-h" }, el("h3", { text: token.label }), el("span", { class: "pill", text: token.id })),
    el("p", { text: info }),
    el("div", { style: rowStyle }, button),
    status,
  );
  button.addEventListener("click", async () => {
    if (!confirm(`Revoke ${token.label}? Sessions using it stop reaching the proxies.`)) return;
    button.disabled = true;
    status.dataset.bad = "";
    status.textContent = "Waiting for your passkey…";
    try {
      const begin = await post("/tokens/revoke/begin", { id: token.id });
      const answer = await approve(begin.options);
      const done = await post("/tokens/revoke/finish", { id: token.id, challenge_id: begin.challenge_id, ...answer });
      card.remove();
      showEmptyIfNeeded();
      say(done.message);
    } catch (e) {
      status.textContent = e.message || String(e);
      status.dataset.bad = "1";
      button.disabled = false;
    }
  });
  return card;
};

const renderList = (tokens) => {
  if (!tokens.length) {
    list.replaceChildren(emptyNote());
    return;
  }
  list.replaceChildren(
    ...tokens.map((t) =>
      tokenCard(
        t,
        `issued by ${t.person || "someone no longer listed"} · created ${stamp(t.created_at)} · expires ${stamp(t.expires_at)}`,
      ),
    ),
  );
};

const buildIssueForm = () => {
  const label = el("input", {
    type: "text",
    placeholder: "Label, such as: pigeon cloud environment",
    maxlength: "64",
    "aria-label": "Label",
    autocomplete: "off",
    style: textStyle,
  });
  const days = el("input", {
    type: "number",
    min: "1",
    max: "90",
    value: "30",
    "aria-label": "Days until expiry",
    style: `${inputBase};width:6em`,
  });
  const button = el("button", { type: "button", class: "button", text: "Issue with passkey" });
  const status = el("p", { "data-status": "", role: "status" });
  const result = el("div");

  button.addEventListener("click", async () => {
    const text = label.value.trim();
    const n = Number(days.value);
    status.dataset.bad = "";
    if (!text) {
      status.textContent = "Enter a label.";
      status.dataset.bad = "1";
      return;
    }
    if (!Number.isInteger(n) || n < 1 || n > 90) {
      status.textContent = "Days must be a whole number from 1 to 90.";
      status.dataset.bad = "1";
      return;
    }
    button.disabled = true;
    status.textContent = "Waiting for your passkey…";
    try {
      const body = { label: text, days: n };
      const begin = await post("/tokens/issue/begin", body);
      const answer = await approve(begin.options);
      const done = await post("/tokens/issue/finish", { ...body, challenge_id: begin.challenge_id, ...answer });

      const tokenInput = el("input", {
        type: "text",
        readonly: "",
        autocomplete: "off",
        spellcheck: "false",
        "aria-label": "New credential",
        style: monoStyle,
      });
      tokenInput.value = done.token;
      const copy = el("button", { type: "button", class: "button", text: "Copy" });
      copy.addEventListener("click", async () => {
        try {
          await navigator.clipboard.writeText(tokenInput.value);
          say("Copied.");
        } catch {
          tokenInput.focus();
          tokenInput.select();
          say("Select the text and copy it yourself.");
        }
      });
      result.replaceChildren(
        el("div", { style: rowStyle }, tokenInput, copy),
        el("p", { text: done.message }),
        el("p", { text: "It will not be shown again." }),
      );
      status.textContent = "";
      label.value = "";
      list.querySelector("[data-empty]")?.remove();
      list.prepend(tokenCard(done, `issued by ${me} · expires in ${done.expires_in_days} days`));
    } catch (e) {
      status.textContent = e.message || String(e);
      status.dataset.bad = "1";
    } finally {
      button.disabled = false;
    }
  });

  return el(
    "article",
    { class: "card" },
    el("h3", { text: "Issue a credential" }),
    el("div", { style: rowStyle }, label, days, button),
    status,
    result,
  );
};

viewButton.addEventListener("click", async () => {
  viewButton.disabled = true;
  say("Waiting for your passkey…");
  try {
    const begin = await post("/tokens/view/begin", {});
    const answer = await approve(begin.options);
    const done = await post("/tokens/view/finish", { challenge_id: begin.challenge_id, ...answer });
    me = done.person;
    say(`Signed in as ${done.person}.`);
    viewButton.hidden = true;
    issue.replaceChildren(buildIssueForm());
    issue.hidden = false;
    renderList(done.tokens);
  } catch (e) {
    say(e.message || String(e), true);
    viewButton.disabled = false;
  }
});
})();
