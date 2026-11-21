// A stand-in for the Cloudflare API, for a dry `wrangler deploy` of src/platform: it answers each
// call wrangler makes with a plausible success and writes the call as one line,
// `METHOD /path?query`, path after /client/v4, to the file named by STANDIN_LOG; a JSON body
// under 64 KiB follows on the line as ` body=<base64>`, so its body rule is checked too. It holds
// no credential and checks none. Mode `existing` (the default) answers as for a Worker already
// deployed at its last migration; mode `fresh` as for a Worker that does not exist yet, so both
// ways wrangler deploys are recorded.
//
//   STANDIN_LOG=calls.txt STANDIN_PORT=8787 STANDIN_MODE=existing \
//     STANDIN_MIGRATION=v3 STANDIN_CLASSES=Directory,CiScheduler,CiRunner node standin.mjs
import { createServer } from "node:http";
import { appendFileSync } from "node:fs";
import { createHash, randomUUID } from "node:crypto";

const log = process.env.STANDIN_LOG || "calls.txt";
const port = Number(process.env.STANDIN_PORT || 8787);
const mode = process.env.STANDIN_MODE || "existing";
const migration = process.env.STANDIN_MIGRATION || null;
const classes = (process.env.STANDIN_CLASSES || "").split(",").filter(Boolean);
const PREFIX = "/client/v4";

// A namespace id per Durable Object class, the same on every run.
export const namespaceId = (cls) => createHash("sha256").update(cls).digest("hex").slice(0, 32);
const versions = new Map();
const applications = new Map();

const ok = (result, extra = {}) => ({ success: true, errors: [], messages: [], result, ...extra });
const missing = (code, message) => ({ success: false, errors: [{ code, message }], messages: [], result: null });

function answer(method, path, query, body) {
  const seg = path.split("/").filter(Boolean); // accounts, {a}, ...
  const rest = seg.slice(2).join("/");
  const script = seg[4];
  if (method === "GET" && /^workers\/services\/[^/]+$/.test(rest)) {
    if (mode === "fresh") return [404, missing(10090, "This Worker does not exist on your account.")];
    return [200, ok({ id: seg[4], default_environment: { environment: "production", script: { id: seg[4], migration_tag: migration, tag: "t", etag: "e", handlers: ["fetch", "queue"], last_deployed_from: "wrangler" } } })];
  }
  if (method === "GET" && /^workers\/scripts\/[^/]+\/secrets$/.test(rest)) return [200, ok([])];
  if (method === "GET" && /^workers\/scripts\/[^/]+\/deployments$/.test(rest)) return [200, ok({ deployments: [] })];
  if (method === "GET" && /^workers\/scripts\/[^/]+\/settings$/.test(rest)) return [200, ok({ bindings: [], logpush: false, tail_consumers: [], tags: [], observability: null })];
  if (method === "GET" && /^workers\/workers\/[^/]+$/.test(rest)) return [200, ok({ id: script, name: seg[4], subdomain: { enabled: false, previews_enabled: false } })];
  if (method === "GET" && rest === "queues") {
    const name = query.get("name");
    return [200, ok(name ? [{ queue_id: createHash("sha256").update(name).digest("hex").slice(0, 32), queue_name: name }] : [], { result_info: { page: 1, per_page: 100, count: 1, total_count: 1, total_pages: 1 } })];
  }
  const upload = (method === "POST" && /^workers\/scripts\/[^/]+\/versions$/.test(rest))
    || (method === "PUT" && /^workers\/scripts\/[^/]+$/.test(rest));
  if (upload) {
    const id = randomUUID();
    versions.set(id, true);
    const bindings = classes.map((c) => ({ type: "durable_object_namespace", name: c, class_name: c, namespace_id: namespaceId(c) }));
    const result = { id, etag: "e", startup_time_ms: 1, metadata: {}, resources: { bindings, script: { etag: "e", handlers: ["fetch", "queue"] } }, deployment_id: id, number: 1 };
    return [200, ok(rest.endsWith("versions") ? result : { ...result, id: seg[4] })];
  }
  if (method === "GET" && /^workers\/scripts\/[^/]+\/versions\/[^/]+$/.test(rest)) {
    const bindings = classes.map((c) => ({ type: "durable_object_namespace", name: c, class_name: c, namespace_id: namespaceId(c) }));
    return [200, ok({ id: seg[6], number: 1, metadata: {}, resources: { bindings } })];
  }
  if (method === "POST" && /^workers\/scripts\/[^/]+\/deployments$/.test(rest)) return [200, ok({ id: randomUUID(), strategy: "percentage", versions: [] })];
  if (method === "PATCH" && /^workers\/scripts\/[^/]+\/script-settings$/.test(rest)) return [200, ok({ logpush: false, tail_consumers: [], observability: null, tags: [] })];
  if (method === "POST" && /^workers\/scripts\/[^/]+\/subdomain$/.test(rest)) return [200, ok({ enabled: false, previews_enabled: false })];
  if (method === "GET" && rest === "workers/durable_objects/namespaces") {
    return [200, ok(classes.map((c) => ({ id: namespaceId(c), class: c, script: script || process.env.STANDIN_SCRIPT, name: c, use_sqlite: true })), { result_info: { page: 1, per_page: 1000, count: classes.length, total_count: classes.length, total_pages: 1 } })];
  }
  if (method === "POST" && rest === "containers/image-preparations") return [200, ok({ status: "ready" })];
  const app = /^containers\/applications\/([^/]+)$/.exec(rest);
  if (method === "GET" && app) {
    const found = applications.get(app[1]);
    return found ? [200, ok(found)] : [404, missing(1000, "application not found")];
  }
  if (method === "GET" && rest === "containers/applications") return [200, ok([...applications.values()])];
  if (method === "POST" && rest === "containers/applications") {
    let parsed = {};
    try { parsed = JSON.parse(body); } catch {}
    const id = parsed.durable_objects?.namespace_id || randomUUID();
    const made = { id, ...parsed, instances: 0, created_at: new Date().toISOString() };
    applications.set(id, made);
    return [200, ok(made)];
  }
  if (method === "PATCH" && app) return [200, ok({ ...(applications.get(app[1]) || {}), id: app[1] })];
  if (method === "GET" && seg.length === 2 && seg[0] === "accounts") return [200, ok({ id: seg[1], name: "stand-in" })];
  return [404, missing(7003, `the stand-in does not know ${method} ${path}`)];
}

createServer((req, res) => {
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  req.on("end", () => {
    const url = new URL(req.url, "http://standin");
    const path = url.pathname.startsWith(PREFIX) ? url.pathname.slice(PREFIX.length) : url.pathname;
    const raw = Buffer.concat(chunks);
    const json = (req.headers["content-type"] || "").startsWith("application/json");
    const body = json && raw.length > 0 && raw.length < 65536 ? ` body=${raw.toString("base64")}` : "";
    appendFileSync(log, `${req.method} ${path}${url.search}${body}\n`);
    const [status, answered] = answer(req.method, path, url.searchParams, raw.toString("utf8"));
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(JSON.stringify(answered));
  });
}).listen(port, "127.0.0.1", () => console.log(`stand-in on 127.0.0.1:${port}, mode ${mode}`));
