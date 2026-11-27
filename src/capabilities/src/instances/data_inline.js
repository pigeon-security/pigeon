// The main module of an instance Worker that runs its data store inline
// (docs/plans/linked-data.md). `site.js` is the site module exactly as an instance without a
// data store has it (redirects and protected origin included); "./index.js" is the data
// Worker's build (src/data-worker, by worker-build), whose handlers are its entrypoint's
// prototype methods, called here with this Worker's own env and ctx as the runtime would.
// On the workers.dev host, only signed Data calls and the enabled historical mail import
// routes reach the data Worker's fetch. Email Routing's messages and the backup
// queue go to the data Worker; every other request goes to the site unchanged.
import site from "./site.js";
import data from "./index.js";

const CALL_PATH = "/data/call";
const MAIL_IMPORT_PATHS = new Set(["/data/mail-import", "/data/mail-import/terminal"]);

function run(handler, arg, env, ctx) {
  return data.prototype[handler].call({ env, ctx }, arg);
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const dataPath = url.pathname === CALL_PATH || (env.MAIL_IMPORT_INSTANCE && MAIL_IMPORT_PATHS.has(url.pathname));
    if (request.method === "POST" && dataPath && url.hostname.endsWith(".workers.dev")) {
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
