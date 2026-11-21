// The secrets page: sign in with a passkey to see the list, then replace a value with a passkey
// approval of its own. The approval's challenge is bound to the secret's name and the SHA-256 of
// the new value, worked out here, so the value itself is sent once, to be checked and written.
// Its own scope: passkey.js declares the same names at the top level of the shared global scope.
(() => {
const { b64u, post, approve, say } = window.pigeonPasskey;

const list = document.querySelector("[data-list]");
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

async function digest(value) {
  return b64u.enc(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
}

async function save(name, input, button, status) {
  const value = input.value.trim();
  if (!value) {
    status.textContent = "Type the new value first.";
    status.dataset.bad = "1";
    return;
  }
  button.disabled = true;
  status.dataset.bad = "";
  status.textContent = "Waiting for your passkey…";
  try {
    const begin = await post("/secrets/put/begin", { name, digest: await digest(value) });
    const answer = await approve(begin.options);
    status.textContent = "Checking and saving…";
    const done = await post("/secrets/put/finish", {
      name,
      value,
      challenge_id: begin.challenge_id,
      ...answer,
    });
    input.value = "";
    status.textContent = done.message;
  } catch (e) {
    status.textContent = e.message || String(e);
    status.dataset.bad = "1";
  } finally {
    button.disabled = false;
  }
}

function row(s) {
  const state = s.set
    ? `set${s.modified ? ` · changed ${s.modified.slice(0, 16).replace("T", " ")}` : ""}`
    : "missing";
  const head = el(
    "div",
    { class: "card-h" },
    el("h3", { text: s.name }),
    el("span", { class: s.set ? "pill" : "pill accent", text: state }),
  );
  const card = el("article", { class: s.set ? "card" : "card accent", id: s.name }, head);
  if (!s.owner) {
    card.append(el("p", { text: "No capability declares this one, so it is listed only." }));
    return card;
  }
  card.append(el("p", { text: `${s.owner} · ${s.purpose}` }));
  const input = el("input", {
    type: "password",
    autocomplete: "off",
    spellcheck: "false",
    "aria-label": `New value for ${s.name}`,
    placeholder: s.set ? "New value" : "Value",
    style:
      "width:100%;box-sizing:border-box;padding:10px 12px;border-radius:10px;border:1px solid var(--line);background:var(--chip);color:var(--ink);font:var(--type-small);font-family:var(--font-mono)",
  });
  const button = el("button", { class: "button", text: s.set ? "Replace" : "Set" });
  const status = el("p", { "data-status": "", role: "status" });
  button.addEventListener("click", () => save(s.name, input, button, status));
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") save(s.name, input, button, status);
  });
  card.append(el("div", { style: "display:flex;gap:8px;align-items:center" }, input, button), status);
  return card;
}

viewButton.addEventListener("click", async () => {
  viewButton.disabled = true;
  say("Waiting for your passkey…");
  try {
    const begin = await post("/secrets/view/begin", {});
    const answer = await approve(begin.options);
    const done = await post("/secrets/view/finish", { challenge_id: begin.challenge_id, ...answer });
    say(`Signed in as ${done.person}.`);
    viewButton.hidden = true;
    const declared = done.secrets.filter((s) => s.owner);
    const others = done.secrets.filter((s) => !s.owner);
    list.replaceChildren(
      el("div", { class: "layer" }, ...declared.map(row)),
      ...(others.length
        ? [el("span", { class: "eyebrow", text: "Other secrets in the store" }), el("div", { class: "layer" }, ...others.map(row))]
        : []),
    );
    const wanted = decodeURIComponent(location.hash.slice(1));
    const target = wanted && document.getElementById(wanted);
    if (target) {
      target.scrollIntoView({ block: "center" });
      target.querySelector("input")?.focus();
    }
  } catch (e) {
    say(e.message || String(e), true);
    viewButton.disabled = false;
  }
});
})();
