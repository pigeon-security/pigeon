// The main module of an instance Worker that runs its data store inline
// (docs/plans/linked-data.md). `site.js` is the site module exactly as an instance without a
// data store has it (redirects and protected origin included); "./index.js" is the data
// Worker's build (src/data-worker, by worker-build), whose handlers are its entrypoint's
// prototype methods, called here with this Worker's own env and ctx as the runtime would.
// Only a POST to /data/call on the workers.dev host reaches the data Worker's fetch, which
// answers 404 unless the call is signed with DATA_KEY; Email Routing's messages and the backup
// queue go to the data Worker; every other request goes to the site unchanged.
import site from "./site.js";
import data from "./index.js";

const CALL_PATH = "/data/call";

function run(handler, arg, env, ctx) {
  return data.prototype[handler].call({ env, ctx }, arg);
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === CALL_PATH && url.hostname.endsWith(".workers.dev")) {
      return run("fetch", request, env, ctx);
    }
    return site.fetch(request, env, ctx);
  },
  async email(message, env, ctx) {
    return run("email", message, env, ctx);
  },
  async queue(batch, env, ctx) {
    return run("queue", batch, env, ctx);
  },
};
