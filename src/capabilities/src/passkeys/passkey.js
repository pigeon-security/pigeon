// The browser's side of pigeon's passkey ceremonies: each button names a flow (enrol or
// link) and the value it acts on; the server issues the challenge and checks the answer.
const b64u = {
  enc: (buf) =>
    btoa(String.fromCharCode(...new Uint8Array(buf)))
      .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""),
  dec: (s) =>
    Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4)), (c) => c.charCodeAt(0)),
};

const status = document.querySelector("[data-status]");
const say = (text, bad) => {
  status.textContent = text;
  status.dataset.bad = bad ? "1" : "";
};

async function post(path, body) {
  const res = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const json = await res.json();
  if (!res.ok || json.error) throw new Error(json.error || res.statusText);
  return json;
}

async function enrol(o) {
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: b64u.dec(o.challenge),
      rp: o.rp,
      user: { id: b64u.dec(o.user.id), name: o.user.name, displayName: o.user.displayName },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      authenticatorSelection: { residentKey: "required", userVerification: "required" },
      attestation: "none",
      timeout: 300000,
    },
  });
  const r = cred.response;
  return {
    credential: {
      id: cred.id,
      clientDataJSON: b64u.enc(r.clientDataJSON),
      authenticatorData: b64u.enc(r.getAuthenticatorData()),
      publicKey: b64u.enc(r.getPublicKey()),
      publicKeyAlgorithm: r.getPublicKeyAlgorithm(),
    },
  };
}

async function approve(o) {
  const cred = await navigator.credentials.get({
    publicKey: {
      challenge: b64u.dec(o.challenge),
      rpId: o.rpId,
      allowCredentials: o.allow.map((id) => ({ type: "public-key", id: b64u.dec(id) })),
      userVerification: "required",
      timeout: 300000,
    },
  });
  const r = cred.response;
  return {
    assertion: {
      id: cred.id,
      clientDataJSON: b64u.enc(r.clientDataJSON),
      authenticatorData: b64u.enc(r.authenticatorData),
      signature: b64u.enc(r.signature),
    },
  };
}

for (const button of document.querySelectorAll("button[data-flow]")) {
  button.addEventListener("click", async () => {
    const { flow, key, value } = button.dataset;
    button.disabled = true;
    say("Waiting for your passkey…");
    try {
      const begin = await post(`/auth/${flow}/begin`, { [key]: value });
      const answer = flow === "enrol" ? await enrol(begin.options) : await approve(begin.options);
      const done = await post(`/auth/${flow}/finish`, { [key]: value, challenge_id: begin.challenge_id, ...answer });
      say(done.message);
    } catch (e) {
      say(e.message || String(e), true);
      button.disabled = false;
    }
  });
}
