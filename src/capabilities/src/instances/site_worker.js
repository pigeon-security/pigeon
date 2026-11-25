// pigeon's site Worker: serves the deploy that `live` names from the bucket bound as SITE.
// Layout: `live` holds a release ID; `deploys/{id}.json` maps "/path" to {blob, content_type,
// size}; `blobs/{sha256}` holds each file. Until the first deploy it answers 503.
const RECHECK_MS = 30_000;
let state = null; // { release, commit, manifest, checked }

async function current(env) {
  const now = Date.now();
  if (state && now - state.checked < RECHECK_MS) return state;
  const live = await env.SITE.get("live");
  if (!live) return (state = { commit: null, manifest: null, checked: now });
  const release = (await live.text()).trim();
  // Only immutable releases are served. A `live` that names anything else (such as a commit
  // from before releases) serves nothing until the next publish moves it to a release.
  if (!release.startsWith("release-")) return (state = { commit: null, manifest: null, checked: now });
  if (state && state.release === release && state.manifest) {
    state.checked = now;
    return state;
  }
  const m = await env.SITE.get(`deploys/${release}.json`);
  if (!m) return (state = { commit: null, manifest: null, checked: now });
  const meta = await env.SITE.get(`release-meta/${release}.json`);
  const commit = meta ? (await meta.json()).commit : null;
  return (state = { release, commit, manifest: await m.json(), checked: now });
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
    const url = new URL(request.url);
    // www is served as a redirect to the apex, so the site has one address.
    if (url.hostname.startsWith("www.")) {
      url.hostname = url.hostname.slice(4);
      return Response.redirect(url.toString(), 301);
    }
    let path;
    try {
      path = decodeURIComponent(url.pathname);
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
      "x-pigeon-release": s.release,
      ...(s.commit ? { "x-pigeon-commit": s.commit } : {}),
    };
    if (status === 200 && request.headers.get("if-none-match") === etag) {
      return new Response(null, { status: 304, headers });
    }
    const obj = await env.SITE.get(`blobs/${entry.blob}`);
    if (!obj) return plain(502, "This file is missing from the deploy.");
    return new Response(request.method === "HEAD" ? null : obj.body, { status, headers });
  },
};
