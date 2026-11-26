// Appended to pigeon's site Worker when its instance has redirects, with the site module's
// export renamed to `served`. PIGEON_REDIRECTS is the plan's map (instances/redirects.rs
// `config`): a lowercase host to { to, status, keep_path, cache }. A request to one of those hosts is
// answered with the redirect, with no body, before anything else (a protected origin's key
// check included: the host is a public custom domain); every other request goes on unchanged.
let redirectMap = null;

export default {
  async fetch(request, env, ctx) {
    redirectMap ??= JSON.parse(env.PIGEON_REDIRECTS || "{}");
    const url = new URL(request.url);
    const rule = Object.hasOwn(redirectMap, url.hostname) ? redirectMap[url.hostname] : null;
    if (rule) {
      const location = rule.keep_path ? rule.to + url.pathname + url.search : rule.to;
      return new Response(null, {
        status: rule.status,
        headers: { location, "cache-control": rule.cache },
      });
    }
    if (!env.SITE) return plain(404, "Not found.");
    return served.fetch(request, env, ctx);
  },
};
