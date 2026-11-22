// pigeon's site Worker: serves the deploy that `live` names from the bucket bound as SITE.
// Layout: `live` holds a commit; `deploys/{commit}.json` maps "/path" to {blob, content_type,
// size}; `blobs/{sha256}` holds each file. Until the first deploy it answers 503.
const RECHECK_MS = 30_000;
let state = null; // { commit, manifest, checked }

async function current(env) {
  const now = Date.now();
  if (state && now - state.checked < RECHECK_MS) return state;
  const live = await env.SITE.get("live");
  if (!live) return (state = { commit: null, manifest: null, checked: now });
  const commit = (await live.text()).trim();
  if (state && state.commit === commit && state.manifest) {
    state.checked = now;
    return state;
  }
  const m = await env.SITE.get(`deploys/${commit}.json`);
  if (!m) return (state = { commit: null, manifest: null, checked: now });
  return (state = { commit, manifest: await m.json(), checked: now });
}

function resolve(manifest, path) {
  if (path.split("/").includes("..")) return null;
  if (Object.hasOwn(manifest, path)) return path;
  const candidate = `${path.replace(/\/+$/, "")}/index.html`;
  return Object.hasOwn(manifest, candidate) ? candidate : null;
}

function plain(status, text, extra = {}) {
  return new Response(text, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store", ...extra },
  });
}

export default {
  async fetch(request, env) {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return plain(405, "Method not allowed.", { allow: "GET, HEAD" });
    }
    let path;
    try {
      path = decodeURIComponent(new URL(request.url).pathname);
    } catch {
      return plain(400, "Bad path.");
    }
    const s = await current(env);
    if (!s.manifest) return plain(503, "Nothing is deployed here yet.");

    let key = resolve(s.manifest, path);
    let status = 200;
    if (!key) {
      status = 404;
      key = Object.hasOwn(s.manifest, "/404.html") ? "/404.html" : null;
      if (!key) return plain(404, "Not found.");
    }
    const entry = s.manifest[key];
    const etag = `"${entry.blob}"`;
    const headers = {
      "content-type": entry.content_type,
      etag,
      "cache-control": key.endsWith(".html") || key.endsWith(".xml")
        ? "public, max-age=60"
        : "public, max-age=3600",
      "x-pigeon-commit": s.commit,
    };
    if (status === 200 && request.headers.get("if-none-match") === etag) {
      return new Response(null, { status: 304, headers });
    }
    const obj = await env.SITE.get(`blobs/${entry.blob}`);
    if (!obj) return plain(502, "This file is missing from the deploy.");
    return new Response(request.method === "HEAD" ? null : obj.body, { status, headers });
  },
};
