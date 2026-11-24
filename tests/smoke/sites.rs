//! Sites and blogs: the apex, rendering, auth and deploys.

use super::*;

/// The apex (pigeon.dev) answers only the site's pages; the platform's paths never answer there.
#[test]
fn site_on_the_apex() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::site::{PAGES, Site, is_apex, page};

    assert!(is_apex("pigeon.dev", "pigeon.dev"), "the apex");
    assert!(is_apex("PIGEON.dev", "pigeon.dev"), "ignoring case");
    for host in [
        "platform.pigeon.dev",
        "proxy.pigeon.dev",
        "www.pigeon.dev",
        "pigeon.dev.evil.com",
    ] {
        assert!(!is_apex(host, "pigeon.dev"), "{host} is not the apex");
    }

    for (i, path) in PAGES.iter().enumerate() {
        assert_eq!(page("GET", path), Some(i), "GET {path}");
        assert_eq!(page("HEAD", path), Some(i), "HEAD {path}");
    }
    for path in [
        "/mcp",
        "/auth/",
        "/oauth/token",
        "/.well-known/oauth-authorization-server",
        "/secrets",
        "/tokens",
        "/health",
        "/git/x",
        "/cf/client/v4",
        "/settings",
        "/architecture.json/",
        "//",
    ] {
        assert_eq!(page("GET", path), None, "GET {path} on the apex");
    }
    for (method, path) in [
        ("POST", "/"),
        ("PUT", "/architecture.json"),
        ("DELETE", "/"),
    ] {
        assert_eq!(page(method, path), None, "{method} {path} on the apex");
    }

    let site = Site {
        zone: "pigeon.dev".into(),
        worker: "pigeon-platform".into(),
        also: vec!["pigeon-sites".into()],
        bindings: vec![("pigeon-sites".into(), "SITES".into())],
    };
    let fp = site.footprint();
    assert_eq!(fp.hostnames.len(), 1, "one hostname");
    let h = &fp.hostnames[0];
    assert_eq!(h.host, "pigeon.dev", "the apex");
    assert_eq!(h.worker, "pigeon-platform", "served by the platform");
    assert!(h.access.is_empty(), "in public");
    assert!(fp.workers.is_empty(), "no Worker of its own");
    let tools: Vec<String> = site.tools().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(tools, ["site_publish"], "site_publish only");
    let schema = &site.tools()[0].input_schema;
    assert_eq!(
        schema["properties"]["worker"]["enum"],
        json!(["pigeon-platform", "pigeon-sites"]),
        "the apex may move to the sites Worker"
    );
}

/// The blog renderer (`pigeon_blog`): a consumer repository in, a static site out, with
/// Zola's URLs (posts, pages, alias redirects, tags, feed, sitemap), root-relative links, every
/// problem reported at once, and the same bytes from the same input.
#[test]
fn blog_renders_a_consumer_repo() {
    use pigeon_blog::{Input, render};

    let site_toml = r#"
template = "blog@1"
features = ["math", "no-commit"]

[site]
title = "Example <Blog>"
description = "Notes."
intro = "Hi, I'm **Example**."
avatar = "/assets/a.png"
author_page = "/pages/about"
alternate_names = ["Ex"]

[[site.nav]]
title = "About"
link = "/pages/about"

[[site.nav]]
title = "Elsewhere"
link = "https://example.org/me"

[[site.icons]]
src = "/assets/i.png"
sizes = "192x192"
type = "image/png"
"#;
    let post = |date: &str, extra: &str, body: &str| {
        format!("+++\ntitle = \"Post {date}\"\ndate = {date}T10:00:00+00:00\n{extra}+++\n{body}\n")
    };
    let mut statics: std::collections::BTreeSet<String> = [
        "assets/a.png",
        "assets/i.png",
        "assets/images/site/favicon-32x32.png",
        "assets/images/site/favicon-16x16.png",
        "assets/images/site/apple-touch-icon.png",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let input = Input {
        site_toml: site_toml.into(),
        posts: vec![
            (
                "2024-05-01-older.md".into(),
                post(
                    "2024-05-01",
                    "[taxonomies]\ntags = [\"Travel Notes\"]\n",
                    "Old.",
                ),
            ),
            (
                "2025-02-03-newer.md".into(),
                post(
                    "2025-02-03",
                    "aliases = [\"changelog/newer\"]\n[taxonomies]\ntags = [\"Travel Notes\"]\n",
                    "## Hello\n\n## Hello\n\n[up](#hello)\n\n```rust\nlet a = 1 < 2;\n```\n",
                ),
            ),
        ],
        pages: vec![(
            "about.md".into(),
            "+++\ntitle = \"About\"\naliases = [\"pages/names\"]\n+++\nMe.\n".into(),
        )],
        statics: statics.clone(),
        base_url: "https://example.com/".into(),
        commit: "abc1234".into(),
        commit_year: 2026,
    };
    let out = render(&input).expect("renders");
    assert_eq!((out.template, out.version), ("blog@1", "1.0.0"));
    assert_eq!(out.digest.len(), 64);
    assert_eq!(
        render(&input).unwrap(),
        out,
        "the same input renders the same bytes"
    );

    let file = |p: &str| String::from_utf8(out.files[p].0.clone()).unwrap();
    for p in [
        "index.html",
        "posts/newer/index.html",
        "posts/older/index.html",
        "pages/about/index.html",
        "pages/names/index.html",
        "changelog/newer/index.html",
        "tags/index.html",
        "tags/travel-notes/index.html",
        "posts/posts_feed.xml",
        "sitemap.xml",
        "robots.txt",
        "manifest.json",
        "404.html",
        "assets/css/main.css",
        "assets/css/tokens.css",
        "assets/css/pigeon.css",
    ] {
        assert!(out.files.contains_key(p), "{p}");
    }
    assert_eq!(out.statics.len(), statics.len());

    // The home page: newest first by year, the intro, escaped text, the pigeon look, JSON-LD.
    let home = file("index.html");
    let (newer, older) = (
        home.find("&#x2F;posts&#x2F;newer&#x2F;").unwrap(),
        home.find("&#x2F;posts&#x2F;older&#x2F;").unwrap(),
    );
    assert!(newer < older && home.find(">2025<").unwrap() < home.find(">2024<").unwrap());
    assert!(home.contains("<p>Hi, I'm <strong>Example</strong>.</p>"));
    assert!(home.contains("<title>Example &lt;Blog&gt; — Notes.</title>"));
    assert!(home.contains("color-theme-pigeon") && home.contains("<h1 class=\"m-0\">"));
    assert!(
        home.contains("\"alternateName\": [\"Ex\"]")
            && home.contains("\"sameAs\": [\"https://example.org/me\"]")
    );
    assert!(
        home.contains("\"name\": \"Example \\u003cBlog>\""),
        "JSON-LD escapes <"
    );
    assert!(
        home.contains("katex@0.16.22") && !home.contains("abc1234"),
        "math on, commit off"
    );

    // A post: Zola's heading ids, root-relative anchors, code blocks, absolute canonical URL.
    let p = file("posts/newer/index.html");
    assert!(
        p.contains("<h2 id=\"hello\">Hello</h2>") && p.contains("<h2 id=\"hello-1\">Hello</h2>")
    );
    assert!(p.contains("<a href=\"/posts/newer/#hello\">up</a>"));
    assert!(p.contains("<pre><code data-lang=\"rust\">let a = 1 &lt; 2;\n</code></pre>"));
    assert!(p.contains("<link rel=\"canonical\" href=\"https:&#x2F;&#x2F;example.com&#x2F;posts&#x2F;newer&#x2F;\">"));
    assert!(
        p.contains("href=\"&#x2F;pages&#x2F;about\">Example &lt;Blog&gt;</a>"),
        "byline"
    );
    assert!(p.contains("2025 February 3"));

    // Aliases redirect, root-relative; tags list their posts.
    assert!(file("pages/names/index.html").contains("const target = \"/pages/about/\";"));
    let tag = file("tags/travel-notes/index.html");
    assert!(
        tag.contains("Tag: Travel Notes") && tag.contains("href=\"&#x2F;posts&#x2F;older&#x2F;\"")
    );
    assert!(tag.contains("content=\"Posts tagged &quot;Travel Notes&quot;.\""));

    // Feed, sitemap, robots, manifest.
    let feed = file("posts/posts_feed.xml");
    assert!(feed.contains("<updated>2025-02-03T10:00:00+00:00</updated>"));
    assert!(feed.find("/posts/newer/").unwrap() < feed.find("/posts/older/").unwrap());
    let map = file("sitemap.xml");
    assert!(map.contains("<loc>https://example.com/posts/newer/</loc>\n        <lastmod>2025-02-03T10:00:00Z</lastmod>"));
    assert!(
        map.contains("<loc>https://example.com/tags/travel-notes/</loc>") && !map.contains("names")
    );
    assert_eq!(
        file("robots.txt"),
        "User-agent: *\nAllow: /\n\nSitemap: https://example.com/sitemap.xml\n"
    );
    assert!(file("manifest.json").contains("\"type\": \"image/png\""));

    // Everything wrong is reported at once, by file, and nothing renders.
    let mut bad = input.clone();
    bad.site_toml = site_toml.replace(
        "features = [\"math\", \"no-commit\"]",
        "features = [\"sparkles\"]\nlayout = \"x\"",
    );
    assert!(
        render(&bad).unwrap_err()[0]
            .message
            .contains("unknown field `layout`")
    );
    bad.site_toml = site_toml.replace("\"math\", ", "\"sparkles\", ");
    bad.posts
        .push(("2025-01-01-mismatch.md".into(), post("2025-01-02", "", "x")));
    bad.posts
        .push(("Not Dated.md".into(), post("2025-01-02", "", "x")));
    bad.posts.push((
        "2025-03-03-taken.md".into(),
        post("2025-03-03", "aliases = [\"pages/names\"]\n", "x"),
    ));
    bad.pages.push((
        "extra.md".into(),
        "+++\ntitle = \"X\"\ncolour = \"red\"\n+++\n".into(),
    ));
    statics.remove("assets/a.png");
    statics.insert("robots.txt".into());
    bad.statics = statics;
    let problems: Vec<String> = render(&bad)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();
    let has = |file: &str, words: &str| {
        problems
            .iter()
            .any(|p| p.starts_with(file) && p.contains(words))
    };
    assert!(
        has(".pigeon/site.toml", "`sparkles` is not a feature"),
        "{problems:#?}"
    );
    assert!(has(
        "content/posts/2025-01-01-mismatch.md",
        "is prefixed 2025-01-01, but its date is 2025-01-02"
    ));
    assert!(has("content/posts/Not Dated.md", "YYYY-MM-DD-slug.md"));
    assert!(has("content/pages/extra.md", "unknown field `colour`"));
    assert!(has(
        ".pigeon/site.toml",
        "avatar `/assets/a.png` is not under static/"
    ));
    assert!(has("static/robots.txt", "is generated by blog@1"));
    assert!(has("content/", "/pages/names/ is also"), "{problems:#?}");
}

/// Template versions are selected by the instance's pin: the newest carried version of
/// `site.toml`'s major that the pin allows, with a problem naming the carried versions when the
/// pin allows none, and a changelog that keeps the versioning rules.
#[test]
fn blog_template_versions_selected_by_pin() {
    use pigeon_blog::{Input, render, render_pinned};
    use pigeon_core::versions::{Component, Entry, Kind, Requirement, Version};

    let input = |template: &str| Input {
        site_toml: format!(
            "template = \"{template}\"\n\n[site]\ntitle = \"Example\"\ndescription = \"Notes.\"\n"
        ),
        statics: ["apple-touch-icon", "favicon-16x16", "favicon-32x32"]
            .iter()
            .map(|f| format!("assets/images/site/{f}.png"))
            .collect(),
        base_url: "https://example.com".into(),
        commit: "abc1234".into(),
        commit_year: 2025,
        ..Input::default()
    };
    let pinned = |template: &str, pin: &str| {
        let req = Requirement::parse(pin).expect("a requirement");
        render_pinned(&input(template), &|v| {
            Version::parse(v).is_ok_and(|v| req.matches(&v))
        })
    };

    for pin in ["^1", "=1.0.0"] {
        let out = pinned("blog@1", pin).expect("renders");
        assert_eq!(out.version, "1.0.0");
        assert_eq!(out.template, "blog@1");
        assert_eq!(out.digest, pigeon_blog::digest());
    }
    for pin in ["^2", "=1.0.1"] {
        let problems = pinned("blog@1", pin).expect_err("the pin allows none");
        assert!(
            problems.iter().any(|p| p.file == ".pigeon/site.toml"
                && p.message.contains("blog@1 versions 1.0.0")
                && p.message.contains("pin allows none")),
            "{problems:#?}"
        );
    }
    let problems = render(&input("blog@2")).expect_err("blog@2 is not carried");
    assert!(
        problems
            .iter()
            .any(|p| p.file == ".pigeon/site.toml" && p.message.contains("blog@1")),
        "{problems:#?}"
    );
    assert_eq!(
        render(&input("blog@1")),
        render_pinned(&input("blog@1"), &|_| true)
    );

    let serves: Vec<&str> = pigeon_blog::TEMPLATES.iter().map(|t| t.version).collect();
    let mut blog = Component::new("blog", Kind::Template, serves[0]).serving(&serves);
    blog.changelog = pigeon_blog::CHANGELOG
        .iter()
        .map(|e| Entry {
            version: e.version.into(),
            breaking: e.breaking,
            summary: e.summary.into(),
            migration: e.migration.map(str::to_string),
        })
        .collect();
    assert_eq!(blog.problems(), Vec::<String>::new());
    assert_eq!(blog.changelog[0].summary, pigeon_core::versions::BEGAN);
}

/// A record with one site on the platform subdomain `notes`, for the sign-in scenarios.
fn sites_auth_record() -> pigeon_capabilities::instances::record::Instance {
    use pigeon_capabilities::instances::record::{Instance, Target};
    let mut notes: Instance = serde_json::from_value(pigeon_capabilities::instances::template())
        .expect("the template is a record");
    notes.id = "notes".into();
    notes.title = "notes".into();
    notes.worker = Some("notes".into());
    notes.zones.clear();
    notes.sites[0].bucket = "notes-site".into();
    notes.sites[0].build = None;
    notes.sites[0].targets = vec![Target::Subdomain {
        label: "notes".into(),
    }];
    notes
}

/// A site that does not opt in behaves as before: its record has no `auth` or `allow`, its host
/// entry has none, and the gate passes everything, `/_pigeon/` paths and POSTs included.
#[test]
fn sites_auth_off_by_default() {
    use pigeon_core::site_auth::{Gate, config_of, gate};

    let notes = sites_auth_record();
    let text = serde_json::to_value(&notes).unwrap();
    assert!(text["sites"][0].get("auth").is_none() && text["sites"][0].get("allow").is_none());
    assert_eq!(
        serde_json::from_value::<pigeon_capabilities::instances::record::Instance>(text).unwrap(),
        notes
    );
    let entry = json!({ "host": "notes.pigeon.dev", "instance": "notes", "upstream": "https://x.workers.dev" });
    assert!(config_of(&entry).is_none());
    let other = json!({ "host": "notes.pigeon.dev", "auth": "password", "allow": ["usr_fake1"] });
    assert!(config_of(&other).is_none());
    for (method, path) in [
        ("GET", "/"),
        ("GET", "/_pigeon/sign-in"),
        ("GET", "/_pigeon/passkey.js"),
        ("POST", "/_pigeon/sign-in/finish"),
        ("POST", "/anything"),
    ] {
        assert_eq!(
            gate(None, method, path, false),
            Gate::Pass,
            "{method} {path}"
        );
    }
}

/// On a gated host every path needs a session, assets included; the reserved paths are
/// reachable; a sign-in's `next` stays on the host; and the record's rules hold.
#[test]
fn sites_auth_gates_every_path() {
    use pigeon_capabilities::instances::record::{Rules, Target, validate};
    use pigeon_core::site_auth::{Config, Gate, Reserved, config_of, gate, safe_next};

    let entry = json!({
        "host": "notes.pigeon.dev", "upstream": "https://x.workers.dev",
        "auth": "passkey", "allow": ["usr_fake1", "site"],
    });
    let config = config_of(&entry).expect("a gated host");
    assert_eq!(
        config,
        Config {
            allow: vec!["usr_fake1".into(), "site".into()]
        }
    );
    for path in [
        "/",
        "/post/a",
        "/assets/x.css",
        "/favicon.ico",
        "/_pigeon/other",
    ] {
        assert_eq!(
            gate(Some(&config), "GET", path, false),
            Gate::SignIn,
            "{path}"
        );
        assert_eq!(
            gate(Some(&config), "HEAD", path, false),
            Gate::SignIn,
            "{path}"
        );
        assert_eq!(
            gate(Some(&config), "POST", path, false),
            Gate::Unauthorized,
            "{path}"
        );
        assert_eq!(gate(Some(&config), "GET", path, true), Gate::Pass, "{path}");
    }
    for (method, path, which) in [
        ("GET", "/_pigeon/passkey.js", Reserved::Script),
        ("GET", "/_pigeon/sign-in", Reserved::SignInPage),
        ("POST", "/_pigeon/sign-in/begin", Reserved::SignInBegin),
        ("POST", "/_pigeon/sign-in/finish", Reserved::SignInFinish),
        ("GET", "/_pigeon/enrol", Reserved::EnrolPage),
        ("POST", "/_pigeon/enrol/begin", Reserved::EnrolBegin),
        ("POST", "/_pigeon/enrol/finish", Reserved::EnrolFinish),
        ("POST", "/_pigeon/sign-out", Reserved::SignOut),
    ] {
        assert_eq!(
            gate(Some(&config), method, path, false),
            Gate::Reserved(which),
            "{method} {path}"
        );
    }
    // `next` is a path on this host or "/".
    assert_eq!(safe_next("/post/a?x=1"), "/post/a?x=1");
    for bad in [
        "//evil.example",
        "/\\evil.example",
        "https://evil.example/",
        "evil.example",
        "",
        "/a\r\nSet-Cookie: x=1",
    ] {
        assert_eq!(safe_next(bad), "/", "{bad:?}");
    }

    // The record: only "passkey", a non-empty allow list, and only platform subdomains.
    let platform: pigeon_capabilities::instances::record::Instance =
        serde_json::from_value(json!({
            "id": "pigeon-security/platform", "title": "pigeon",
            "account": { "kind": "home" }, "zones": ["pigeon.dev"],
            "repo": { "kind": "artifacts", "namespace": "pigeon-security", "repo": "pigeon" }
        }))
        .unwrap();
    let reserved = pigeon_capabilities::instances::record::reserved_labels(
        &["platform.pigeon.dev".into(), "pigeon.dev".into()],
        "pigeon.dev",
    );
    let workers = vec!["pigeon-platform".to_string()];
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
    };
    let check = |notes: &pigeon_capabilities::instances::record::Instance| {
        validate(notes, std::slice::from_ref(&platform), &rules)
    };
    let mut notes = sites_auth_record();
    assert_eq!(check(&notes), Vec::<String>::new());
    notes.sites[0].auth = Some("passkey".into());
    assert!(
        check(&notes).iter().any(|p| p.contains("allow")),
        "{:?}",
        check(&notes)
    );
    notes.sites[0].allow = vec!["usr_fake1".into()];
    assert_eq!(check(&notes), Vec::<String>::new());
    let text = serde_json::to_value(&notes).unwrap();
    assert_eq!(text["sites"][0]["auth"], "passkey");
    assert_eq!(text["sites"][0]["allow"], json!(["usr_fake1"]));
    notes.sites[0].auth = Some("password".into());
    assert!(
        check(&notes)
            .iter()
            .any(|p| p.contains("must be \"passkey\""))
    );
    notes.sites[0].auth = Some("passkey".into());
    notes.sites[0].targets.push(Target::Preview);
    assert!(
        check(&notes)
            .iter()
            .any(|p| p.contains("platform subdomain"))
    );
}

/// A session ends at its expiry, and not before.
#[test]
fn sites_auth_session_expires() {
    use pigeon_core::site_auth::{
        COOKIE, SESSION_MS, clear_cookie, cookie_value, issue, session_key, set_cookie, verify,
    };

    let key = session_key(b"fake caller key", "notes.pigeon.dev");
    let now = 1_000_000;
    let value = issue(&key, "notes.pigeon.dev", "usr_fake1", now);
    let session = verify(&key, "notes.pigeon.dev", &value, now + 1).expect("a fresh session");
    assert_eq!(session.user_id, "usr_fake1");
    assert_eq!(session.expires_ms, now + SESSION_MS);
    assert!(verify(&key, "notes.pigeon.dev", &value, now + SESSION_MS - 1).is_some());
    assert!(verify(&key, "notes.pigeon.dev", &value, now + SESSION_MS).is_none());
    assert!(verify(&key, "notes.pigeon.dev", &value, now + SESSION_MS + 1).is_none());
    assert_eq!(SESSION_MS, 12 * 3_600_000);

    // The cookie: found among others, host-only, and cleared with Max-Age=0.
    let header = format!("a=1; {COOKIE}={value}; b=2");
    assert_eq!(cookie_value(Some(&header)), Some(value.as_str()));
    assert_eq!(cookie_value(Some("a=1")), None);
    assert_eq!(cookie_value(None), None);
    let set = set_cookie(&value);
    for part in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=43200",
    ] {
        assert!(set.contains(part), "{part}: {set}");
    }
    assert!(!set.contains("Domain") && set.starts_with("__Host-pigeon-site="));
    assert!(clear_cookie().contains("Max-Age=0"));
}

/// A session belongs to one host: a cookie of a.pigeon.dev fails on b.pigeon.dev, a site user of
/// a is not let in on b, a service user never, and a changed cookie fails.
#[test]
fn sites_auth_session_is_host_scoped() {
    use pigeon_core::site_auth::{
        Config, allowed, issue, session_key, split_subject, subject, verify,
    };

    let caller = b"fake caller key";
    let (key_a, key_b) = (
        session_key(caller, "a.pigeon.dev"),
        session_key(caller, "b.pigeon.dev"),
    );
    assert_ne!(key_a, key_b);
    let now = 5_000;
    let value = issue(
        &key_a,
        "a.pigeon.dev",
        &subject("site:a.pigeon.dev", "usr_fake1"),
        now,
    );
    let session = verify(&key_a, "a.pigeon.dev", &value, now).expect("valid on a");
    assert_eq!(
        split_subject(&session.user_id),
        Some(("site:a.pigeon.dev", "usr_fake1"))
    );
    assert!(verify(&key_b, "b.pigeon.dev", &value, now).is_none());
    assert!(
        verify(&key_a, "b.pigeon.dev", &value, now).is_none(),
        "even with a's key"
    );

    // A changed cookie: another user, a longer expiry, or another signature.
    let parts: Vec<&str> = value.split('.').collect();
    let other_user = issue(&key_a, "a.pigeon.dev", "usr_fake2", now);
    let forged = format!(
        "{}.{}.{}.{}",
        other_user.split('.').next().unwrap(),
        parts[1],
        parts[2],
        parts[3]
    );
    assert!(verify(&key_a, "a.pigeon.dev", &forged, now).is_none());
    let longer = format!("{}.{}.{}9.{}", parts[0], parts[1], parts[2], parts[3]);
    assert!(verify(&key_a, "a.pigeon.dev", &longer, now).is_none());
    let last = if value.ends_with('0') { '1' } else { '0' };
    let flipped = format!("{}{last}", &value[..value.len() - 1]);
    assert!(verify(&key_a, "a.pigeon.dev", &flipped, now).is_none());
    for junk in ["", "a.b.c", "x.y.z.w", &format!("{value}.extra")] {
        assert!(
            verify(&key_a, "a.pigeon.dev", junk, now).is_none(),
            "{junk:?}"
        );
    }

    // Who is allowed: a listed standard user anywhere; "site" only the host's own site users.
    let listed = Config {
        allow: vec!["usr_fake1".into(), "site".into()],
    };
    assert!(allowed(&listed, "a.pigeon.dev", "usr_fake1", "standard"));
    assert!(allowed(
        &listed,
        "a.pigeon.dev",
        "usr_fake1",
        "site:a.pigeon.dev"
    ));
    assert!(allowed(
        &listed,
        "a.pigeon.dev",
        "usr_fake9",
        "site:a.pigeon.dev"
    ));
    assert!(!allowed(
        &listed,
        "b.pigeon.dev",
        "usr_fake9",
        "site:a.pigeon.dev"
    ));
    assert!(!allowed(
        &listed,
        "b.pigeon.dev",
        "usr_fake1",
        "site:a.pigeon.dev"
    ));
    assert!(!allowed(&listed, "a.pigeon.dev", "usr_fake9", "standard"));
    assert!(!allowed(&listed, "a.pigeon.dev", "usr_fake1", "service"));
    assert!(!allowed(&listed, "a.pigeon.dev", "usr_fake9", "service"));
    let none = Config { allow: Vec::new() };
    assert!(!allowed(&none, "a.pigeon.dev", "usr_fake1", "standard"));
}

/// The sites deploy's pure parts: a repository tarball as GitHub serves one (a top folder, gzip)
/// read into its files, sorted into what the renderer reads, with the commit's year.
#[test]
fn sites_deploy_reads_a_repository_tarball() {
    let dir = std::env::temp_dir().join(format!("pigeon-tarball-{}", std::process::id()));
    let top = dir.join("noisypigeon-noisypigeon-2787e52");
    for d in [
        ".pigeon",
        "content/posts",
        "content/pages",
        "static/assets/img",
    ] {
        std::fs::create_dir_all(top.join(d)).unwrap();
    }
    std::fs::write(top.join(".pigeon/site.toml"), "template = \"blog@1\"\n").unwrap();
    std::fs::write(
        top.join("content/posts/2026-01-02-a.md"),
        "+++\ntitle = \"A\"\n+++\n",
    )
    .unwrap();
    std::fs::write(
        top.join("content/pages/about.md"),
        "+++\ntitle = \"About\"\n+++\n",
    )
    .unwrap();
    std::fs::write(top.join("static/assets/img/a.png"), [0u8, 1, 2]).unwrap();
    std::fs::write(top.join("README.md"), "hi").unwrap();
    let gz = dir.join("repo.tar.gz");
    let ok = Command::new("tar")
        .args(["--mtime=@1795315200", "-czf"])
        .arg(&gz)
        .arg("-C")
        .arg(&dir)
        .arg("noisypigeon-noisypigeon-2787e52")
        .status()
        .unwrap();
    assert!(ok.success());
    let archive = pigeon_core::tarball::read(&std::fs::read(&gz).unwrap()).unwrap();
    assert_eq!(pigeon_core::tarball::year_of(archive.mtime), 2026);
    assert_eq!(archive.files.len(), 5);
    let s = pigeon_blog::repo::split(archive.files).unwrap();
    assert_eq!(s.site_toml.as_deref(), Some("template = \"blog@1\"\n"));
    assert_eq!((s.posts.len(), s.pages.len()), (1, 1));
    assert_eq!(
        s.statics,
        vec![("assets/img/a.png".to_string(), vec![0u8, 1, 2])]
    );
    assert_eq!(pigeon_core::tarball::year_of(951_782_400), 2000);
    assert!(pigeon_core::tarball::read(b"not gzip").is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The pigeon.dev apex pages (`pigeon_core::home`): routing, the home page, the architecture
/// page and the policy.
#[test]
fn sites_apex_pages() {
    use pigeon_core::home::{self, Route};
    let known = [
        ("/", Route::Home),
        ("/architecture", Route::Architecture),
        ("/architecture.json", Route::Json),
        ("/logo.png", Route::Logo),
    ];
    for (p, want) in known {
        assert_eq!(home::route("GET", p), want);
        assert_eq!(home::route("HEAD", p), want);
        assert_eq!(home::route("POST", p), Route::MethodNotAllowed);
        assert_eq!(home::route("DELETE", p), Route::MethodNotAllowed);
    }
    for p in ["/nope", "//", "/architecture/", "", "/logo.png/"] {
        assert_eq!(home::route("GET", p), Route::NotFound, "{p}");
    }
    assert_eq!(home::route("POST", "/nope"), Route::NotFound);
    let build = home::Build {
        commit: "0123456789abcdef",
        digest: "a1b2c3d4",
    };
    let h = home::home_html(Some(build));
    assert!(h.contains("href=\"https://platform.pigeon.dev/app\""));
    assert!(h.contains(">Console ") && h.contains("↗"));
    assert!(h.contains("href=\"/architecture\">Architecture"));
    assert!(h.contains("href=\"/\" aria-current=\"page\">Overview"));
    assert!(h.contains(">P</span>pigeon"));
    assert!(h.contains("radial-gradient"));
    assert!(h.contains(">Coming soon</p><h1>Find the pattern. Act with confidence.</h1>"));
    assert!(h.contains("<footer>0123456789ab · digest a1b2c3d4</footer>"));
    assert!(!h.contains("data:image/svg+xml"));

    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    let a = home::architecture_html(svg, build);
    assert!(a.contains("href=\"/architecture\" aria-current=\"page\">Architecture"));
    assert!(a.contains(">Architecture</p><h1>Diagram</h1>"));
    assert!(a.contains(&format!(
        "data:image/svg+xml;base64,{}",
        pigeon_core::git::base64(svg.as_bytes())
    )));
    assert!(!a.contains("<svg"), "the drawing is an image, not HTML");
    assert!(!a.contains("<h2>Workers</h2>"));
    assert!(a.contains("<footer>0123456789ab · digest a1b2c3d4</footer>"));
    let escaped = home::architecture_html(
        svg,
        home::Build {
            commit: "<b>",
            digest: "<i>",
        },
    );
    assert!(escaped.contains("<footer>&lt;b&gt; · digest &lt;i&gt;</footer>"));
    assert!(home::home_html(None).contains("Deployment metadata unavailable"));
    assert!(home::unavailable_html(None).contains("The architecture is not available right now."));
    assert!(home::unavailable_html(Some(build)).contains("0123456789ab · digest a1b2c3d4"));
    assert!(!home::CSP.contains("script-src"));
    assert!(home::CSP.contains("frame-ancestors 'none'"));
}
