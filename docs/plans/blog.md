# The blog capability: template `blog@1` and its renderer

Status: **designed, awaiting the person's go-ahead** (2026-11-22). Nothing is built. Branch
`claude/blog`. Preview: https://claude.ai/artifact/JQPrj9GRS8WaQWoGMijnBk (home page and one post,
rendered by a Rust spike from the real content).

## Goal

The person's words: "build a blog capability that brings over the static site and design of the
site (the new one) that the consumer then uses".

- pigeon owns the blog's design as a versioned template, `blog@1`.
- A consumer repository holds only content and settings. The first consumer is
  `noisypigeon/noisypigeon`, set up by a separate move.
- pigeon turns that repository into static files that look and link like noisypigeon.com.

Hard lines:
- No new policy rules.
- Tests only in `tests/smoke.rs`, which stays under 60 s.
- Nothing in the noisypigeon Cloudflare account is created or changed by this plan.

## Ownership

| Who | Owns |
| --- | --- |
| This plan | The template, the renderer (consumer repository in, static files out), `.pigeon/site.toml` validation, and showing the template and version in the architecture and console |
| linked-accounts ([linked-accounts.md](linked-accounts.md)) | Instance records, the GitHub App, build triggering, fetching the repository, uploads (blobs, manifest, `live`), serving, the subdomain and the cutover |

## What is true today (read 2026-11-22)

From `noisypigeon/pigeon` at `841e7fb`:

- **Config.** `workloads/noisypigeon.com/blog/config.yaml` pins template `blog` **1.0.2** with
  features `[math]`. It also sets the site metadata, `author.alternate_names`, the nav and the
  manifest icons.
- **Template source.** It lives in `src/pigeon-site-templates/blog/`, a Zola theme tagged
  `site-template-blog/vX.Y.Z`. The latest version is **1.0.5**. It contains:
  - `templates/`: `base`, `header`, `index`, `post`, `page`, `tags/list`, `tags/single`,
    `changelog`, `posts_feed.xml` and `robots.txt`;
  - `static/assets/css/`: `main.css` (24 KB), `tokens.css` and `pigeon.css`;
  - `scaffold/`: `config.toml`, the `posts` and `pages` sections, `manifest.json`;
  - `schema.json` and `template.yaml` (the features and their defaults).
- **"The new one".** The 1.0.2 → 1.0.5 diff changes only the look:
  - It adds `tokens.css` and `pigeon.css` (the app's look: dot grid, ink and accent colours, a
    round avatar, cards with a hard shadow), from change !817.
  - `look.color` defaults to `pigeon`; `gameboy` stays selectable.
  - Markup, URLs and settings are unchanged.

  The live site is still on 1.0.2 (`gameboy`) because its config was never bumped. So `blog@1`
  carries 1.0.5's design. The colours change and nothing else does.
- **Build.** The old harness assembles the theme and runs Zola 0.23.6 with
  `SITE_COMMIT_SHA=<short sha>`:
  - Features are the template's defaults, plus those listed, minus `no-<feature>`. Defaults on:
    `feed`, `fonts`, `commit`. Live therefore has `feed`, `fonts`, `commit` and `math`.
  - `base_url` is `https://noisypigeon.com`.
- **Content.** 57 posts and 2 pages, plus 102 files (28 MB) under `assets/`.
  - Front matter: TOML `+++`, with `title`, `date`, `description`, `aliases` (6 files),
    `[taxonomies] tags` (53 posts) and `[extra] og_image` (20).
  - Markdown with raw `<figure>`/`<img>` HTML, one table, plain fenced code. No shortcodes, no
    `@/` links. No post uses math today.
- **Links that must keep working.** 45 links go to `/pages/names/`, an alias of
  `/pages/lineage`. There are also `changelog/…` aliases for four posts and one old post slug.
- **Checking against Zola.** Zola was not run here: building it in this session was refused, as
  running it was in linked-accounts phase 0. Its rules below were read from its source at
  `v0.23.6`. The live site is not reachable from this session's network.

## Decisions

### 1. The consumer repository

```
.pigeon/site.toml
content/posts/YYYY-MM-DD-slug.md
content/pages/name.md
static/            copied as-is, same paths (static/assets/x → /assets/x)
```

`.pigeon/site.toml`, converted from today's config.yaml:

```toml
template = "blog@1"
features = ["math"]          # optional: template defaults plus these; "no-feed" turns a default off

[site]
title = "Willow Graysen"
short_name = "noisypigeon"
description = "Infra dev, data hoarder, mechanic-ish, photographer."
intro = "Hi, I’m Willow Graysen. This is my digital shoebox — …"   # Markdown
avatar = "/assets/images/site/avatar.png"
social_preview = "/assets/images/site/social_preview.png"
author_page = "/pages/about"
author_name = ""             # optional: the byline and JSON-LD name; defaults to title
alternate_names = ["Willow Finch", "Kara Finch", "Kara Graysen", "Luna Graysen"]   # optional
[[site.nav]]                 # in order
title = "About"
link = "/pages/about"

[[site.icons]]               # optional: the manifest's icons
src = "/assets/images/site/icon-192.png"
sizes = "192x192"
type = "image/png"
```

Unknown keys are refused. `static/` must hold the favicons `base.html` links
(`/assets/images/site/{favicon-16x16,favicon-32x32,apple-touch-icon}.png`), and must not hold a path
the renderer generates (`manifest.json`, `robots.txt`, `sitemap.xml`, …).

- **Two keys beyond the agreed list.** `alternate_names` (the home page's JSON-LD) and `icons`
  (the manifest) are optional additions. Without them, today's output would lose both.
- **Also optional:** `[site.look]` (`color` is `pigeon` by default; `gameboy` keeps today's
  colours), plus `[site.labels]`, `[site.banner]`, `[site.theme_color]` and `[site.og_image_size]`,
  with the template's defaults.
- **Front matter**, TOML `+++`:
  - Posts: `title` and `date` are required. Optional: `description`, `updated`, `aliases`,
    `[taxonomies] tags` and `[extra] og_image`.
  - Pages: `title` is required. Optional: `description`, `aliases` and `[extra]`.
  - `aliases`, `updated` and `og_image` are beyond the agreed list. Today's content uses them.
- **Nothing about hosting goes in the repository.** `base_url`, the commit and the commit's date
  come from the deploy. Infrastructure, build and secrets stay in pigeon.

### 2. The renderer: pure Rust in `pigeon_core::blog`

```rust
pub fn render(input: &Input) -> Result<Output, Vec<Problem>>

pub struct Input {
    site_toml: String,
    posts: Vec<(String, String)>,     // file name, text
    pages: Vec<(String, String)>,
    statics: BTreeSet<String>,        // paths under static/, never their bytes
    base_url: String,                 // from the instance record, e.g. https://noisypigeon.com
    commit: String,                   // short sha, for the footer
    commit_year: u16,                 // the footer's year, instead of now()
}

pub struct Output {
    files: BTreeMap<String, (Vec<u8>, &'static str)>,   // path → bytes, content type
    statics: Vec<String>,             // copied from static/ to the same path
    template: &'static str,           // "blog@1"
    version: &'static str,            // "1.0.0"
    digest: String,                   // sha256 of the template's embedded files
}
```

- **No I/O, no clock, no environment.** The same `Input` gives byte-identical `Output`:
  - maps are ordered;
  - posts sort by date, newest first, ties by file name;
  - the feed's `updated` is the newest post date;
  - the sitemap is sorted by URL.
- **Problems** name the file and line and say how to fix it:
  - an unknown key, with the nearest known one;
  - a file name that is not `YYYY-MM-DD-slug.md`, or a date prefix that differs from `date`;
  - an avatar, social preview, icon or `og_image` path missing from `static/`;
  - `author_page` or a site-relative nav link that names no page;
  - two files answering one URL (an alias, a page, a static file);
  - `template` other than `blog@1`.

  Every problem is reported at once, not only the first.
- **Markdown follows Zola 0.23.6.**
  - pulldown-cmark 0.13 with tables, footnotes, strikethrough, task lists and heading
    attributes. No smart punctuation.
  - Headings get `id`s, slugified as Zola does, with `-1` and `-2` for repeats.
  - Fenced code renders as `<pre><code data-lang="x">` (no highlighting).
  - Raw HTML passes through.
  - Math stays client-side: KaTeX from jsDelivr, pinned with SRI, as today. Markdown escapes
    apply as they do in Zola.
- **The templates are Rust functions, not a template engine.** Each is transliterated from the
  upstream file and names that file. Escaping goes through one helper.
  - This avoids Tera or minijinja in the Worker, and the templates are checked at compile time.
  - The cost: changing the design means changing Rust. It lands through the gate like any other
    pigeon change.

### 3. The output, URL for URL (as Zola builds today's site)

| Path | From |
| --- | --- |
| `/index.html` | The intro, then every post grouped by year |
| `/posts/{slug}/index.html` | Each post. `{slug}` is the file name after `YYYY-MM-DD-` |
| `/pages/{name}/index.html` | Each page |
| `/{alias}/index.html` | Zola's redirect page, for each alias (`/pages/names/`, `/changelog/…/`, `/posts/i-have-an-dedicated-office/`) |
| `/posts/posts_feed.xml` | Atom, every post, as `posts_feed.xml` |
| `/tags/index.html`, `/tags/{tag}/index.html` | The taxonomy. Zola renders it even though the `tags` feature (post links) is off |
| `/sitemap.xml` | Zola's format: pages with `lastmod`, the home page, the taxonomy pages |
| `/robots.txt`, `/manifest.json`, `/404.html` | `robots.txt` and the manifest from the template; Zola's built-in 404 page |
| `/assets/css/{main,tokens,pigeon}.css` | The template |
| `static/**` | Copied to the same path |

`/posts/` and `/pages/` have no index (`render = false`, as today). A site-relative link in
content is left as written.

**Departures from today's bytes, all intended:**
- the colours (decision "the new one" above);
- the feed's `<generator>` names pigeon rather than Zola;
- the footer's year is the commit's year rather than the build's.

### 4. Where it runs: in the Worker, during linked-accounts' deploy step

Content is about 60 small Markdown files, and a render takes milliseconds. Running it in the
Worker removes several pieces from linked-accounts phase 4:
- the `pigeon-ci-site` image and Zola;
- a gate change;
- the job-side GitHub git route;
- the `/sites/build` upload.

Agreed with the linked-accounts planner (2026-11-22). That planner dropped the CI site job, the
Zola image and `/sites/build`. How a deploy uses the renderer:
1. The deploy reads the repository through the GitHub App: the recursive git tree at the pushed
   commit, then only the blobs whose git sha it hasn't stored yet.
2. It passes the text files and the list of static paths to `render`.
3. It streams static bytes straight to blobs: the renderer never holds images.
4. It writes `Output.files` as blobs, then the manifest, then flips `live`.

A render that returns problems deploys nothing and reports them: on the GitHub deployment status
and the console's Deploys section.

**Bundle size.**
- Measured with the spike: pulldown-cmark compiled to wasm32 (`opt-level = "z"`, LTO) is
  **313 KB raw, 105 KB gzipped**.
- The template adds about 60 KB of text.
- Phase 1 measures the platform's bundle before and after. Fallback if it doesn't fit: run the
  same `render` as a small binary in a CI job. The interface doesn't change.

### 5. Versions

- `blog@1` is a major line. Within it, changes keep URLs and `site.toml` keys working: fixes and
  look tweaks only. A breaking change becomes `blog@2`, embedded beside `@1`. A consumer moves to
  it by editing `template`.
- `site.toml` pins the major version. Each deploy records the exact version (`1.0.0`, from
  upstream 1.0.5) and the template digest.
- Rollback flips `live` to an earlier manifest. Blobs are content-addressed, so a rollback
  restores the exact bytes even after the template has moved on.
- Provenance is in `src/core/src/blog/v1/UPSTREAM.md`: upstream path, tag
  `site-template-blog/v1.0.5`, commit `a7a7de43`, and licence GPL-3.0-or-later, the same as
  pigeon.

### 6. The architecture model and console

- **Per deploy.** Agreed with the linked-accounts planner: no record change. `SiteRecord.build`
  is `.pigeon/site.toml`, and the template is read from it at deploy time. Each deploy record keeps
  `template`, `template_version`, `template_digest`, the commit and any problems.
- **One render per deploy.** `base_url` is `https://{the zone target}`, and the same files are
  served on the preview, the subdomain and the zone. Every link and asset in the output is
  root-relative. `base_url` appears only where an absolute URL is required: canonical,
  `og:url`/`og:image`, JSON-LD, the feed, the sitemap and robots.txt.
- **Console.** The Instance tab's **Sites** section shows, per site, the live deploy's
  `blog@1 · 1.0.0`. The Deploys list shows each deploy's version.
- **Architecture.** The instances layer draws each site with its template and version, read
  from the instance record and the site's live deploy. It is still one Directory read, cached per
  record version and live pointer, as linked-accounts decision 8 has it.
- **Code-derived model.** The `blog` capability gets a footprint: the template versions it
  embeds, and no hostnames or bindings of its own.

## Phases

Each phase is one landing, checked before the next. Executors are AI Gateway models
(`models_submit`), each given one whole file with the interface and the upstream template or
Zola rule it follows. I read every line, run fmt, clippy and the smoke suite, and land.

| Phase | What | Live change | Who |
| --- | --- | --- | --- |
| 1 | `pigeon_core::blog`: `site.rs` (site.toml and validation), `content.rs` (front matter, names, aliases), `markdown.rs` (Zola's rules), `pages.rs` (the templates), `feeds.rs` (Atom, sitemap, robots, manifest, 404, alias pages), `v1/` (CSS byte copies, UPSTREAM.md), `render`. Smoke scenario `blog_renders_a_consumer_repo`: an inline `example.com` fixture with two posts, a page, an alias, a tag and a bad file; it checks URLs, problems and that two renders give the same bytes. `cargo run -p pigeon-core --example blog -- <repo> <out>` for local previews. Bundle size measured | none (code only) | executors (`claude-sonnet-5-5`: markdown, feeds, pages); me (site, content, render) |
| 2 | Fidelity: render today's content (converted in a scratch directory; the real move is separate), compare every URL Zola would produce, and republish the preview from the real renderer. With the person's approval of the read-only hosts `noisypigeon.com` and `www.noisypigeon.com` for this session, fetch live `/sitemap.xml` and check every live URL has a file in the output | none | me |
| 3 | With linked-accounts phase 4: the deploy calls `render`; the deploy record carries the template version; the console Sites/Deploys and architecture instances layer show it; the `blog` footprint | with that phase | me; linked-accounts planner |

## Open questions for the person

1. **The look.** `blog@1` uses the new `pigeon` colours. noisypigeon.com changes colour at the
   cutover; URLs and markup don't. Is that what you want? `look.color = "gameboy"` keeps today's
   colours if not.
2. **Keys beyond the agreed layout.** OK to add `alternate_names`, `icons` and the optional
   `look` to `site.toml`, and to keep `aliases`, `updated` and `og_image` in front matter? They
   are needed for today's output and links.

## Log

- 2026-11-22: design. Read: the old repository (template 1.0.2 to 1.0.5, harness, content), Zola
  0.23.6's markdown and sitemap code, and pigeon's instances and linked-accounts work.
  - A spike rendered the home page and one post with pulldown-cmark. Its wasm size was measured.
  - Building Zola for a byte comparison was refused in this session.
  - The interface was proposed to the linked-accounts planner, and they agreed: it runs in the
    Worker, the tree and blobs are read through the GitHub App, and there is no record change.
    They move the content into noisypigeon/noisypigeon with the `site.toml` schema above.
    `/pages/names/` stays an alias on `lineage.md`.
