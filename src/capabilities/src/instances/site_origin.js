// The public site module is appended here with its export renamed to site.
const originEncoder = new TextEncoder();

function authorized(request, expected) {
  if (typeof expected !== "string" || !/^[0-9a-f]{64}$/.test(expected)) return false;
  const supplied = request.headers.get("authorization")?.match(/^Bearer ([0-9a-f]{64})$/)?.[1];
  return !!supplied && crypto.subtle.timingSafeEqual(
    originEncoder.encode(expected), originEncoder.encode(supplied)
  );
}

export default {
  async fetch(request, env) {
    if (!authorized(request, env.PIGEON_ORIGIN_KEY)) {
      return plain(401, "Origin authorization required.");
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      return plain(405, "Method not allowed.", { allow: "GET, HEAD" });
    }
    const answer = new URL(request.url).pathname === "/_pigeon/origin-check"
      ? new Response(null, { status: 204 })
      : await site.fetch(request, env);
    const response = new Response(answer.body, answer);
    response.headers.set("x-pigeon-origin-protected", "1");
    response.headers.set("cache-control", "private, no-store");
    return response;
  },
};
