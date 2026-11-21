//! The architecture page: what is deployed, layer by layer, in the order a request travels.

use pigeon_capabilities::design::Page;

/// The day the page was last brought in line with what is deployed.
const AS_OF: &str = "2026-11-21";

/// The page, rendered.
pub fn page() -> String {
    Page {
        title: "pigeon architecture",
        description: "What pigeon runs today, layer by layer.",
        body: &body(),
    }
    .render()
}

fn body() -> String {
    format!(
        r#"<main>
<header>
  <span class="eyebrow">pigeon · architecture</span>
  <h1>What's deployed</h1>
  <p>pigeon.dev is public; the platform sits behind Cloudflare Access. pigeon-platform holds one parent token and mints a short-lived, scoped token for each capability it runs.</p>
  <div class="pills"><span class="pill">as of {AS_OF}</span><span class="pill">2 Workers</span><span class="pill">9 capabilities</span></div>
</header>

<div class="arrow" aria-hidden="true"></div>

<section class="layer" aria-labelledby="l-clients">
  <span class="eyebrow" id="l-clients">Layer 1 · Clients</span>
  <div class="cards">
    <article class="card">
      <div class="card-h"><span class="dot"></span><h3>Service-token callers</h3></div>
      <p>Machines holding an Access service token, sent as <code>CF-Access-Client-Id</code> and <code>CF-Access-Client-Secret</code>.</p>
      <div class="pills"><span class="pill">pigeon-platform</span></div>
    </article>
    <article class="card">
      <div class="card-h"><span class="dot"></span><h3>Visitors</h3></div>
      <p>Anyone with a browser, on pigeon.dev.</p>
    </article>
    <article class="card">
      <div class="card-h"><span class="dot"></span><h3>Deploys</h3></div>
      <p>By hand, with <code>wrangler deploy</code> from each Worker's directory.</p>
    </article>
  </div>
</section>

<div class="arrow"><span>HTTPS</span></div>

<section class="region cloudflare" aria-label="Cloudflare">
  <span class="region-name">Cloudflare · pigeon-security</span>

  <section class="layer" aria-labelledby="l-access">
    <span class="eyebrow" id="l-access">Layer 2 · Hostnames</span>
    <div class="cards">
      <article class="card kind-access">
        <div class="card-h"><span class="dot"></span><h3>platform.pigeon.dev</h3><span class="pill accent">subdomain</span></div>
        <ul class="kv"><li>Access: admits <code>pigeon-platform</code> service token only</li><li>serves pigeon-platform</li></ul>
      </article>
      <article class="card">
        <div class="card-h"><span class="dot"></span><h3>pigeon.dev</h3><span class="pill accent">domain</span></div>
        <ul class="kv"><li>public, no Access</li><li>serves pigeon</li></ul>
      </article>
    </div>
  </section>

  <div class="arrow" aria-hidden="true"></div>

  <section class="layer" aria-labelledby="l-workers">
    <span class="eyebrow" id="l-workers">Layer 3 · Workers</span>
    <div class="cards">
      <article class="card kind-worker accent">
        <div class="card-h"><span class="dot"></span><h3>pigeon-platform</h3></div>
        <ul class="kv">
          <li><code>GET /health</code></li>
          <li><code>POST /setup/platform</code> → subdomain</li>
          <li><code>POST /setup/site</code> → domain</li>
          <li><code>POST /setup/source</code> → artifacts, queues</li>
          <li><code>POST /source/token</code> · <code>GET /source/tokens</code> · <code>DELETE /source/tokens/:id</code></li>
          <li><code>POST /source/mirror</code> and Artifacts push events → mirror</li>
        </ul>
        <p>workers.dev and preview URLs off.</p>
      </article>
      <article class="card kind-worker">
        <div class="card-h"><span class="dot"></span><h3>pigeon</h3></div>
        <ul class="kv"><li><code>GET /</code> this page</li></ul>
        <p>Styled by the design capability.</p>
      </article>
    </div>
  </section>

  <div class="arrow"><span>calls</span></div>

  <section class="layer" aria-labelledby="l-caps">
    <span class="eyebrow" id="l-caps">Layer 4 · Capabilities</span>
    <div class="cards">
      <article class="card">
        <div class="card-h"><h3>subdomain</h3><span class="pill accent">serve</span></div>
        <div class="chain"><span>calls</span><span class="pill">domain</span><span class="pill">zero_trust</span><span class="pill">custom domain</span></div>
      </article>
      <article class="card">
        <div class="card-h"><h3>domain</h3><span class="pill accent">serve · protect · active</span></div>
        <div class="chain"><span>calls</span><span class="pill">zero_trust</span><span class="pill">custom domain</span></div>
      </article>
      <article class="card">
        <div class="card-h"><h3>zero_trust</h3><span class="pill accent">protect · unprotect</span></div>
        <div class="chain"><span>calls</span><span class="pill">secrets_store</span></div>
      </article>
      <article class="card">
        <div class="card-h"><h3>secrets_store</h3><span class="pill accent">read · Writer</span></div>
        <p>Reads by binding; writes by API.</p>
      </article>
      <article class="card">
        <div class="card-h"><h3>tokens</h3><span class="pill accent">Scoped</span></div>
        <p>Mints one token per capability from the parent, revokes it after. Every capability above uses it.</p>
      </article>
      <article class="card">
        <div class="card-h"><h3>design</h3><span class="pill accent">Page</span></div>
        <p>Tokens, base styles and components. No token, no API calls.</p>
      </article>
      <article class="card">
        <div class="card-h"><h3>artifacts</h3></div>
        <div class="pills"><span class="pill accent">ensure</span><span class="pill accent">mint</span><span class="pill accent">tokens</span><span class="pill accent">revoke</span><span class="pill accent">store</span></div>
        <div class="chain"><span>calls</span><span class="pill">secrets_store</span></div>
        <p>Git repositories on Cloudflare and the tokens that reach them.</p>
      </article>
      <article class="card">
        <div class="card-h"><h3>mirror</h3><span class="pill accent">mirror</span></div>
        <div class="chain"><span>calls</span><span class="pill">artifacts</span></div>
        <p>Copies an Artifacts repository to a read-only git host over git's HTTP protocol: branches only move forward, tags are only added.</p>
      </article>
      <article class="card">
        <div class="card-h"><h3>queues</h3><span class="pill accent">ensure</span></div>
        <p>Has a queue exist and a Worker consume it.</p>
      </article>
    </div>
  </section>

  <div class="arrow"><span>Cloudflare API · minted tokens</span></div>

  <section class="layer" aria-labelledby="l-account">
    <span class="eyebrow" id="l-account">Layer 5 · Account resources</span>
    <div class="cards">
      <article class="card kind-store">
        <div class="card-h"><span class="dot"></span><h3>Secrets Store</h3></div>
        <ul class="kv">
          <li><code>CF_PARENT_TOKEN</code> bound to pigeon-platform</li>
          <li><code>PLATFORM_ACCESS_CLIENT_ID</code>, <code>_SECRET</code></li>
          <li><code>GITHUB_MIRROR_TOKEN</code> bound to pigeon-platform</li>
        </ul>
      </article>
      <article class="card kind-api">
        <div class="card-h"><span class="dot"></span><h3>API tokens</h3></div>
        <ul class="kv"><li>parent: Account API Tokens Write</li><li>minted: one per capability call, 1 h, revoked after</li></ul>
      </article>
      <article class="card kind-api">
        <div class="card-h"><span class="dot"></span><h3>Zone pigeon.dev</h3></div>
        <ul class="kv"><li>active</li><li>custom domains: pigeon.dev, platform.pigeon.dev</li></ul>
      </article>
      <article class="card kind-api">
        <div class="card-h"><span class="dot"></span><h3>Artifacts</h3></div>
        <ul class="kv"><li><code>pigeon-security/pigeon</code></li><li>the source of truth</li><li>git tokens: 15 min by default, 24 h at most</li></ul>
      </article>
      <article class="card kind-api">
        <div class="card-h"><span class="dot"></span><h3>Queue</h3></div>
        <ul class="kv"><li><code>pigeon-source-events</code></li><li>Artifacts push events, consumed by pigeon-platform</li></ul>
      </article>
    </div>
  </section>
</section>

<div class="arrow"><span>mirror</span></div>

<section class="region github" aria-label="GitHub">
  <span class="region-name">GitHub</span>
  <div class="cards">
    <article class="card">
      <div class="card-h"><h3>pigeon-security/pigeon</h3></div>
      <ul class="kv"><li>read-only copy, for disaster recovery</li><li>follows Artifacts; never in the critical path</li></ul>
    </article>
  </div>
</section>

<section class="layer" aria-label="Legend" style="margin-top:28px">
  <ul class="legend">
    <li><span class="dot kind-access"></span>Behind Access</li>
    <li><span class="dot"></span>Public or client</li>
    <li><span class="dot kind-worker"></span>Worker</li>
    <li><span class="dot kind-store"></span>Secrets</li>
    <li><span class="dot kind-api"></span>Cloudflare API object</li>
    <li><span class="pill accent">name</span>capability or its entry point</li>
  </ul>
</section>
</main>
<footer>pigeon-security/pigeon</footer>"#
    )
}
