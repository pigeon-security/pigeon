//! Sites and blogs: the apex, rendering, auth and deploys.

use super::*;

/// The apex (pigeon.dev) answers only the site's pages; the platform's paths never answer there.
#[test]
fn site_on_the_apex() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::site::{PAGES, Site, is_apex, page};
    use pigeon_core::home::{Asset, Route};

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

    // The same pages and images pigeon-sites serves, so the apex looks the same on either.
    assert_eq!(PAGES, ["/", "/architecture", "/architecture.json"]);
    let want = [Route::Home, Route::Architecture, Route::Json];
    for (path, want) in PAGES.iter().zip(want) {
        assert_eq!(page("GET", path), Some(want), "GET {path}");
        assert_eq!(page("HEAD", path), Some(want), "HEAD {path}");
    }
    for a in Asset::ALL {
        assert_eq!(
            page("GET", a.path()),
            Some(Route::Asset(a)),
            "GET {}",
            a.path()
        );
    }
    assert_eq!(
        page("GET", "/logo.png"),
        None,
        "the iOS app icon is pigeon-sites' alone"
    );
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
        ("POST", "/favicon-32.png"),
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
        platform_worker: "pigeon-platform",
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
        ("/mark/head.webp", Route::Asset(home::Asset::MarkHead)),
        ("/favicon-32.png", Route::Asset(home::Asset::Favicon)),
        (
            "/apple-touch-icon.png",
            Route::Asset(home::Asset::AppleTouch),
        ),
    ];
    for (p, want) in known {
        assert_eq!(home::route("GET", p), want);
        assert_eq!(home::route("HEAD", p), want);
        assert_eq!(home::route("POST", p), Route::MethodNotAllowed);
        assert_eq!(home::route("DELETE", p), Route::MethodNotAllowed);
    }
    for p in [
        "/nope",
        "//",
        "/architecture/",
        "",
        "/logo.png/",
        "/mark/",
        "/favicon.ico",
    ] {
        assert_eq!(home::route("GET", p), Route::NotFound, "{p}");
    }
    assert_eq!(home::route("POST", "/nope"), Route::NotFound);

    // `/platform` and `/platform/*` on the apex go to the console, path and query kept; both
    // apex Workers (pigeon-sites' apex.rs and core's site.rs) answer this with a 302.
    let console = "https://platform.pigeon.dev/app";
    for (path, query, want) in [
        ("/platform", None, console.to_string()),
        ("/platform/", None, format!("{console}/")),
        (
            "/platform/pigeon-security%2Fplatform/inbox",
            None,
            format!("{console}/pigeon-security%2Fplatform/inbox"),
        ),
        (
            "/platform/pigeon.dev/ci",
            Some("tab=jobs&x=1"),
            format!("{console}/pigeon.dev/ci?tab=jobs&x=1"),
        ),
        ("/platform", Some("a=b"), format!("{console}?a=b")),
        ("/platform", Some(""), console.to_string()),
    ] {
        assert_eq!(
            home::console_redirect(path, query),
            Some(want),
            "{path} {query:?}"
        );
    }
    // Every other path is unaffected: no redirect, and the same route as before.
    for p in [
        "/",
        "/architecture",
        "/architecture.json",
        "/logo.png",
        "/mark/head.webp",
        "/platformx",
        "/platform.json",
        "/app",
        "/x/platform",
        "",
    ] {
        assert_eq!(home::console_redirect(p, Some("q=1")), None, "{p}");
    }
    assert_eq!(home::route("GET", "/platform"), Route::NotFound);
    // The images are real and small: the mark's artwork, the favicon and the touch icon.
    for a in home::Asset::ALL {
        let b = a.bytes();
        let magic: &[u8] = if a.content_type() == "image/webp" {
            b"RIFF"
        } else {
            b"\x89PNG"
        };
        assert!(b.starts_with(magic), "{}", a.path());
        assert!(b.len() < 40_000, "{} is {} bytes", a.path(), b.len());
    }
    let build = home::Build {
        commit: "0123456789abcdef",
        digest: "a1b2c3d4",
    };
    let stamp = "<span>0123456789ab · digest a1b2c3d4</span>";
    let h = home::home_html(Some(build));
    // One primary action, the console; no sign-up, no scripts, no web fonts.
    assert!(h.contains(
        "<a class=\"btn btn-primary\" href=\"https://platform.pigeon.dev/app\">Open console "
    ));
    assert!(h.contains("↗"));
    assert!(!h.contains("<form") && !h.contains("<script") && !h.contains("@font-face"));
    assert!(h.contains("<a href=\"/\" aria-current=\"page\">Overview</a>"));
    assert!(h.contains("<a href=\"/architecture\">Architecture</a>"));
    assert!(h.contains("<h1>Find the pattern. Act with confidence.</h1>"));
    assert!(h.contains("<span class=\"pill accent\">Invite only</span>"));
    assert!(h.contains("aria-label=\"Example of a pattern pigeon finds\""));
    assert!(h.contains("<span class=\"tag\">Example</span>"));
    assert!(h.contains("<a class=\"btn\" href=\"/architecture\">View architecture</a>"));
    assert!(h.contains("href=\"/architecture.json\""));
    assert!(h.contains(stamp));
    // The mark, the favicon and the touch icon come from the apex itself, as the policy allows.
    assert!(h.contains("url(\"/mark/head.webp\")"));
    assert!(h.contains("href=\"/favicon-32.png\"") && h.contains("href=\"/apple-touch-icon.png\""));
    // The console's tokens, by the system theme only; the old look is gone.
    assert!(h.contains("--accent: #B03A6A;") && h.contains("--accent: #E07FA8;"));
    assert!(h.contains("@media (prefers-color-scheme: dark)"));
    assert!(!h.contains("data-theme"));
    // One gradient: the hero's dotted ground, and nowhere else.
    assert_eq!(h.matches("radial-gradient").count(), 1);
    assert!(h.contains(".hero::before { content: \"\"; position: absolute; z-index: -1;"));
    assert!(h.contains("--dot: #D5D1DB;") && h.contains("--dot: #34333D;"));
    assert!(!h.contains("Inter Tight"));
    assert!(!h.contains("box-shadow") && !h.contains("color-mix"));
    assert!(!h.contains("data:image/svg+xml"));
    // Discreet: it reads as a money-records workspace and nothing more.
    let lower = h.to_lowercase();
    for word in ["abuse", "survivor", "safety", "victim", "domestic"] {
        assert!(!lower.contains(word), "{word}");
    }

    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    let a = home::architecture_html(svg, build);
    assert!(a.contains("<a href=\"/architecture\" aria-current=\"page\">Architecture</a>"));
    assert!(a.contains("<span class=\"label\">Architecture</span><h1>Diagram</h1>"));
    assert!(a.contains(&format!(
        "data:image/svg+xml;base64,{}",
        pigeon_core::git::base64(svg.as_bytes())
    )));
    assert!(!a.contains("<svg"), "the drawing is an image, not HTML");
    assert!(!a.contains("<h2>Workers</h2>"));
    assert!(a.contains(stamp));
    let escaped = home::architecture_html(
        svg,
        home::Build {
            commit: "<b>",
            digest: "<i>",
        },
    );
    assert!(escaped.contains("<span>&lt;b&gt; · digest &lt;i&gt;</span>"));
    assert!(home::home_html(None).contains("<span>Deployment metadata unavailable</span>"));
    let u = home::unavailable_html(None);
    assert!(u.contains("The architecture is not available right now."));
    assert!(u.contains("<a href=\"/architecture\" aria-current=\"page\">Architecture</a>"));
    assert!(home::unavailable_html(Some(build)).contains(stamp));
    assert!(!home::CSP.contains("script-src"));
    assert!(home::CSP.contains("frame-ancestors 'none'"));
}

#[test]
fn site_object_staging_bounds_parallel_writes_and_drains_failed_batches() {
    use pigeon_core::r2::put_in_batches;
    use std::cell::Cell;
    use std::future::{Future, poll_fn};
    use std::task::{Context, Poll, Waker};

    for failure in [None, Some(0)] {
        let started = Cell::new(0);
        let completed = Cell::new(0);
        let release = Cell::new(false);
        let mut context = Context::from_waker(Waker::noop());
        let writes = put_in_batches(0..7, |index| {
            let started = &started;
            let completed = &completed;
            let release = &release;
            async move {
                started.set(started.get() + 1);
                poll_fn(|_| {
                    if release.get() || failure == Some(index) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                completed.set(completed.get() + 1);
                if failure == Some(index) {
                    Err(index)
                } else {
                    Ok(())
                }
            }
        });
        let mut writes = std::pin::pin!(writes);
        assert_eq!(writes.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(started.get(), 4);
        assert_eq!(completed.get(), usize::from(failure.is_some()));

        release.set(true);
        assert_eq!(
            writes.as_mut().poll(&mut context),
            Poll::Ready(failure.map_or(Ok(()), Err))
        );
        let expected = if failure.is_some() { 4 } else { 7 };
        assert_eq!(started.get(), expected);
        assert_eq!(completed.get(), expected);
    }
}

/// The sender path `Remote::call` takes for a global-connector call (no instance endpoint): the
/// targeted declaration, then the context, then the approved record's binding. `None` is the
/// platform (legacy) envelope; an error is a refusal before any request is sent.
fn global_send(
    worker: &str,
    capability: &str,
    supported: bool,
    tool: &str,
    args: &Value,
    record: Option<&pigeon_capabilities::instances::record::Instance>,
) -> Result<Option<(pigeon_contract::WorkerContext, Value)>, String> {
    use pigeon_capabilities::instances::tools::{bind_global, global_target};
    use pigeon_capabilities::remote::global_context;
    let Some((target, id)) = global_target(capability, tool, args)? else {
        return Ok(None);
    };
    let Some(context) = global_context(worker, supported, target, id)? else {
        return Ok(None);
    };
    Ok(Some((context, bind_global(id, record, tool, args)?)))
}

fn publishing_record(publishing: Value) -> pigeon_capabilities::instances::record::Instance {
    serde_json::from_value(json!({
        "id":"sandbox","title":"Sandbox","version":4,
        "account":{"kind":"linked","account_id":"fixture"},
        "repo":{"kind":"github","full_name":"example/sandbox","installation":12},
        "sites":[{"name":"primary","bucket":"sandbox-site"}],
        "publishing": publishing
    }))
    .unwrap()
}

/// A global-connector `site_deploy` or `site_rollback` naming an approved instance is signed
/// for pigeon-sites and that instance, as the instance endpoint signs it, so Sites takes its
/// typed `sites_scope` callbacks rather than the legacy ones.
#[test]
fn global_site_deploy_and_rollback_are_signed_for_the_approved_instance() {
    let record = publishing_record(json!({"state":"enabled","site":"primary"}));
    for (tool, args) in [
        ("site_deploy", json!({"instance":"sandbox","rev":"main"})),
        ("site_deploy", json!({"instance":"sandbox"})),
        (
            "site_rollback",
            json!({"instance":"sandbox","release":"r1"}),
        ),
    ] {
        let (context, bound) =
            global_send("pigeon-sites", "sites", true, tool, &args, Some(&record))
                .unwrap()
                .unwrap_or_else(|| panic!("{tool} was sent legacy"));
        assert_eq!(context.recipient, "pigeon-sites");
        assert_eq!(context.instance.as_deref(), Some("sandbox"));
        assert_eq!(bound, args, "{tool}");

        // Nested `call` reaches the same path with the same arguments.
        let outer = json!({"tool": tool, "args": args});
        let (name, inner) = pigeon_capabilities::dispatch::requested(&outer).unwrap();
        let nested = global_send("pigeon-sites", "sites", true, name, &inner, Some(&record))
            .unwrap()
            .unwrap();
        assert_eq!(nested, (context, bound));
    }
}

/// Unknown or unapproved instances, arguments outside the registration, and a Sites Worker
/// without context support are refused before anything is sent, with no legacy fallback.
#[test]
fn global_site_calls_refuse_unapproved_targets_and_old_receivers_before_sending() {
    let enabled = publishing_record(json!({"state":"enabled","site":"primary"}));
    let disabled = publishing_record(json!({"state":"disabled"}));
    let deploy = json!({"instance":"sandbox","rev":"main"});
    let refused = |supported, tool, args: &Value, record| {
        global_send("pigeon-sites", "sites", supported, tool, args, record).unwrap_err()
    };

    let why = refused(true, "site_deploy", &deploy, None);
    assert!(why.contains("no approved instance `sandbox`"), "{why}");
    let other = json!({"instance":"elsewhere","rev":"main"});
    let why = refused(true, "site_deploy", &other, Some(&enabled));
    assert!(why.contains("no approved instance `elsewhere`"), "{why}");
    let rollback = json!({"instance":"sandbox","release":"r1"});
    for (tool, args) in [("site_deploy", &deploy), ("site_rollback", &rollback)] {
        let why = refused(true, tool, args, Some(&disabled));
        assert!(why.contains("unavailable on this instance"), "{why}");
        // An old Sites manifest is refused even for an approved instance.
        let why = refused(false, tool, args, Some(&enabled));
        assert!(why.contains("pigeon-sites does not advertise"), "{why}");
    }
    let wider = json!({"instance":"sandbox","rev":"main","bucket":"other"});
    let why = refused(true, "site_deploy", &wider, Some(&enabled));
    assert!(why.contains("`bucket`"), "{why}");
    let why = refused(true, "site_deploy", &json!({"instance": 7}), Some(&enabled));
    assert!(why.contains("`instance`"), "{why}");
}

/// Calls that name no instance, and tools that are not instance-targeted, keep the platform path.
#[test]
fn global_calls_without_an_instance_target_stay_on_the_platform_path() {
    let enabled = publishing_record(json!({"state":"enabled","site":"primary"}));
    for supported in [true, false] {
        for (capability, tool, args) in [
            ("sites", "site_deploy", json!({"rev":"main"})),
            ("sites", "site_rollback", json!({"release":"r1"})),
            ("sites", "site_render", json!({"instance":"sandbox"})),
            ("models", "site_deploy", json!({"instance":"sandbox"})),
        ] {
            assert_eq!(
                global_send(
                    "pigeon-sites",
                    capability,
                    supported,
                    tool,
                    &args,
                    Some(&enabled)
                ),
                Ok(None),
                "{capability} {tool}"
            );
        }
    }
}

/// CI keeps its behaviour: explicit instance reads are signed when the gateway supports context
/// and the record enables CI; an older gateway keeps the legacy envelope.
#[test]
fn global_ci_reads_keep_their_instance_targeting() {
    use pigeon_capabilities::instances::record::Instance;
    let record: Instance = serde_json::from_value(json!({
        "id":"platform","title":"CI","account":{"kind":"home"},
        "repo":{"kind":"artifacts","namespace":"example","repo":"project"},
        "ci":{"state":"enabled","jobs":["test"]}
    }))
    .unwrap();
    let gateway = "pigeon-ci-gateway";
    let read = json!({"instance":"platform","commit":"abc"});
    let (context, bound) = global_send(gateway, "ci", true, "ci_jobs", &read, Some(&record))
        .unwrap()
        .unwrap();
    assert_eq!(context.recipient, gateway);
    assert_eq!(context.instance.as_deref(), Some("platform"));
    assert_eq!(bound, read);
    assert_eq!(
        global_send(gateway, "ci", false, "ci_jobs", &read, Some(&record)),
        Ok(None)
    );
    for (tool, args) in [
        ("ci_status", json!({"id":"job"})),
        ("ci_submit", read.clone()),
    ] {
        assert_eq!(
            global_send(gateway, "ci", true, tool, &args, Some(&record)),
            Ok(None)
        );
    }
    assert!(
        global_send(
            gateway,
            "ci",
            true,
            "ci_jobs",
            &json!({"instance":null}),
            None
        )
        .is_err()
    );
    let wider = json!({"instance":"platform","repo":"other"});
    assert!(global_send(gateway, "ci", true, "ci_jobs", &wider, Some(&record)).is_err());
    assert!(global_send(gateway, "ci", true, "ci_jobs", &read, None).is_err());
    let mut disabled = record.clone();
    disabled.ci = None;
    assert!(global_send(gateway, "ci", true, "ci_jobs", &read, Some(&disabled)).is_err());
}

/// The home landing's content (`site/`), as the landing renderer reads it.
fn landing_site() -> pigeon_landing::Input {
    pigeon_landing::Input {
        site_toml: include_str!("../../site/.pigeon/site.toml").into(),
        index: Some(include_str!("../../site/content/index.toml").into()),
        commit: "abc1234".into(),
        ..pigeon_landing::Input::default()
    }
}

/// Every `on…=` attribute in an HTML text's tags: event handlers, which no landing page may
/// carry.
fn event_handlers(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    for tag in lower.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        for word in tag.split(|c: char| c.is_whitespace() || c == '/' || c == '"' || c == '\'') {
            if word.starts_with("on") && word.contains('=') {
                found.push(word.to_string());
            }
        }
    }
    found
}

/// The pigeon.dev landing page as `landing@1` content (`site/`): it renders the shipped home
/// page's copy, the same `<main>` as `pigeon_core::home` byte for byte, with the same
/// stylesheet and images, no scripts and no event handlers, and nothing but a money-records
/// workspace.
#[test]
fn landing_template_renders_the_home_landing() {
    use pigeon_core::home;

    let out = pigeon_landing::render(&landing_site()).expect("the home landing renders");
    assert_eq!((out.template, out.version), ("landing@1", "1.1.0"));
    assert_eq!(out.digest, pigeon_landing::digest());
    assert_eq!(
        out.files.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "404.html",
            "apple-touch-icon.png",
            "assets/pigeon.css",
            "favicon-32.png",
            "index.html",
            "mark/head.webp"
        ]
    );
    let text = |p: &str| String::from_utf8(out.files[p].0.clone()).unwrap();
    let index = text("index.html");
    for copy in [
        "<h1>Find the pattern. Act with confidence.</h1>",
        "<span class=\"pill accent\">Invite only</span>",
        "<li><h3>Bring it in</h3>",
        "<li><h3>See what repeats</h3>",
        "<li><h3>Keep a clear record</h3>",
        "<tr><th>Sign-in</th><td>Passkeys on your own device. No passwords to guess or reset.</td><td class=\"st\"><span class=\"pill ok\">Live</span></td></tr>",
        "<tr><th>Originals</th><td>Files are stored by their content and never edited, so a record can always be traced back.</td><td class=\"st\"><span class=\"pill warn\">In testing</span></td></tr>",
        "<tr><th>Email in</th><td>A private address per person for forwarding messages and receipts.</td><td class=\"st\"><span class=\"pill\">Planned</span></td></tr>",
        "<span class=\"tag\">Example</span>",
        "<td class=\"num muted\">—</td>",
        "<a href=\"/\" aria-current=\"page\">Overview</a><a href=\"/architecture\">Architecture</a>",
        "<a class=\"btn btn-primary\" href=\"https://platform.pigeon.dev/app\">Open console <span aria-hidden=\"true\">↗</span></a>",
        "<link rel=\"stylesheet\" href=\"/assets/pigeon.css\">",
        "<span>abc1234</span>",
    ] {
        assert!(index.contains(copy), "{copy}");
    }
    // The same page as the apex serves today: the content between the top bar and the footer,
    // byte for byte, and one stylesheet and one set of images for both.
    let main = |h: &str| {
        let start = h.find("<main id=\"main\">").expect("a main");
        let end = h.find("</main>").expect("a main's end");
        h[start..end].to_string()
    };
    assert_eq!(main(&index), main(&home::home_html(None)));
    assert_eq!(text("assets/pigeon.css"), pigeon_landing::CSS);
    assert!(home::home_html(None).contains(pigeon_landing::CSS));
    for (path, bytes, _) in pigeon_landing::ASSETS {
        let home::Route::Asset(asset) = home::route("GET", &format!("/{path}")) else {
            panic!("/{path} is an apex image");
        };
        assert_eq!(asset.bytes(), bytes, "{path}");
        assert_eq!(out.files[path].0, bytes);
    }
    assert!(text("404.html").contains("Page not found"));
    // No scripts, ever, in any page or the stylesheet; and discreet.
    let words = ["abuse", "survivor", "safety", "victim", "domestic"];
    for (path, (bytes, kind)) in &out.files {
        if !kind.starts_with("text/") {
            continue;
        }
        let body = String::from_utf8(bytes.clone()).unwrap();
        let lower = body.to_lowercase();
        assert!(!lower.contains("<script"), "{path}");
        assert!(!lower.contains("javascript:"), "{path}");
        assert_eq!(event_handlers(&body), Vec::<String>::new(), "{path}");
        for word in words {
            assert!(!lower.contains(word), "{path}: {word}");
        }
    }
    for source in [
        include_str!("../../site/.pigeon/site.toml"),
        include_str!("../../site/content/index.toml"),
    ] {
        for word in words {
            assert!(!source.to_lowercase().contains(word), "{word}");
        }
    }
}

/// `landing@1`'s digest covers its stylesheet (the console's tokens, from the apex's
/// home.css) and its images, and is tied to its version here: a change to either is a new
/// template version with a changelog line, then a new digest below.
#[test]
fn landing_template_digest_tied_to_version() {
    use pigeon_core::versions::{Component, Entry, Kind};

    let pinned = [(
        "1.1.0",
        "e07231b78d16fe66fcf6d3752491ef07ef635a4e58ca9c9ce08140e9f4857c55",
    )];
    assert_eq!(pigeon_landing::TEMPLATES.len(), pinned.len());
    for (t, (version, digest)) in pigeon_landing::TEMPLATES.iter().zip(pinned) {
        assert_eq!(t.version, version);
        assert_eq!(
            pigeon_landing::template_digest(t),
            digest,
            "landing@{} {version} changed its stylesheet or images: add a version and a \
             CHANGELOG line in src/landing/src/lib.rs, then pin its digest here",
            t.major
        );
    }
    assert!(pigeon_landing::CSS.contains("--accent: #B03A6A;"));
    assert_ne!(
        pigeon_landing::renderer_digest(),
        pigeon_blog::renderer_digest()
    );

    let serves: Vec<&str> = pigeon_landing::TEMPLATES
        .iter()
        .map(|t| t.version)
        .collect();
    let mut landing = Component::new("landing", Kind::Template, serves[0]).serving(&serves);
    landing.changelog = pigeon_landing::CHANGELOG
        .iter()
        .map(|e| Entry {
            version: e.version.into(),
            breaking: e.breaking,
            summary: e.summary.into(),
            migration: e.migration.map(str::to_string),
        })
        .collect();
    assert_eq!(landing.problems(), Vec::<String>::new());
    assert_eq!(
        landing.changelog.last().map(|e| e.summary.as_str()),
        Some(pigeon_core::versions::BEGAN)
    );
    assert_eq!(landing.version.to_string(), pigeon_landing::VERSION);
}

/// What `landing` refuses: links that are not a site path, a fragment or an http(s) or mailto
/// URL; unknown keys and tones; a missing home page; uncarried majors and pins that allow
/// nothing; files under static/ the template generates. Markdown pages render with raw HTML
/// shown as text.
#[test]
fn landing_template_problems_and_pages() {
    use pigeon_landing::{Problem, render, render_pinned};

    let has = |problems: &[Problem], file: &str, text: &str| {
        problems
            .iter()
            .any(|p| p.file == file && p.message.contains(text))
    };
    let mut bad = landing_site();
    bad.site_toml = bad.site_toml.replace(
        "link = \"/architecture\"\n",
        "link = \"javascript:alert(1)\"\n",
    );
    bad.index = bad
        .index
        .map(|i| i.replace("highlight = true\n", "highlight = true\ncolour = 1\n"));
    let problems = render(&bad).expect_err("refused");
    assert!(
        has(&problems, ".pigeon/site.toml", "javascript:alert(1)"),
        "{problems:#?}"
    );
    assert!(
        has(&problems, "content/index.toml", "unknown field `colour`"),
        "{problems:#?}"
    );

    let mut bad = landing_site();
    bad.index = bad.index.map(|i| {
        i.replace("link = \"#how\"", "link = \"data:text/html,x\"")
            .replace("tone = \"warn\"", "tone = \"red\"")
    });
    bad.pages = vec![
        (
            "about.md".into(),
            "+++\ntitle = \"About\"\n+++\n\n[x](javascript:alert(1))\n".into(),
        ),
        ("Bad Name.md".into(), "+++\ntitle = \"x\"\n+++\n".into()),
    ];
    let problems = render(&bad).expect_err("refused");
    for (file, text) in [
        ("content/index.toml", "data:text/html,x"),
        ("content/index.toml", "tone `red`"),
        ("content/pages/about.md", "javascript:alert(1)"),
        ("content/pages/Bad Name.md", "lowercase"),
    ] {
        assert!(has(&problems, file, text), "{file}: {text}: {problems:#?}");
    }

    let mut no_index = landing_site();
    no_index.index = None;
    let problems = render(&no_index).expect_err("refused");
    assert!(has(
        &problems,
        "content/index.toml",
        "add content/index.toml"
    ));
    let mut clash = landing_site();
    clash.statics = ["assets/pigeon.css".to_string(), "robots.txt".to_string()].into();
    let problems = render(&clash).expect_err("refused");
    assert!(has(
        &problems,
        "static/assets/pigeon.css",
        "generated by landing@1"
    ));
    assert_eq!(problems.len(), 1);

    let mut two = landing_site();
    two.site_toml = two.site_toml.replace("landing@1", "landing@2");
    let problems = render(&two).expect_err("landing@2 is not carried");
    assert!(has(&problems, ".pigeon/site.toml", "carries no landing@2"));
    let problems = render_pinned(&landing_site(), &|v| v == "1.0.1").expect_err("pinned away");
    assert!(has(
        &problems,
        ".pigeon/site.toml",
        "landing@1 versions 1.1.0"
    ));
    let mut blog = landing_site();
    blog.site_toml = blog.site_toml.replace("landing@1", "blog@1");
    let problems = render(&blog).expect_err("not a landing");
    assert!(has(
        &problems,
        ".pigeon/site.toml",
        "not a landing template"
    ));

    let mut paged = landing_site();
    paged.pages = vec![(
        "about.md".into(),
        "+++\ntitle = \"About pigeon\"\n+++\n\nA <script>x</script> and <img src=x onerror=y>.\n\n- [Console](https://platform.pigeon.dev/app)\n".into(),
    )];
    paged.statics = ["robots.txt".to_string()].into();
    let out = render(&paged).expect("renders");
    let about = String::from_utf8(out.files["about/index.html"].0.clone()).unwrap();
    assert!(about.contains("<title>About pigeon · pigeon</title>"));
    assert!(about.contains("&lt;script&gt;x&lt;/script&gt;"));
    assert!(!about.to_lowercase().contains("<script") && !about.contains("<img"));
    assert_eq!(event_handlers(&about), Vec::<String>::new());
    assert!(about.contains("<a href=\"https://platform.pigeon.dev/app\">Console</a>"));
    assert_eq!(out.statics, ["robots.txt"]);
    assert_eq!(render(&paged), render(&paged), "pure");
}

/// pigeon-sites dispatches on `site.toml`'s `template` prefix: `landing@…` to the landing
/// renderer, everything else to the blog exactly as before, with each template's own pin and
/// renderer digest.
#[test]
fn sites_template_dispatch() {
    use crate::sites_templates::{Parts, Template, render, split};

    assert_eq!(
        Template::of("template = \"landing@1\"\n"),
        Template::Landing
    );
    assert_eq!(
        Template::of("template = \"landing@7\"\n"),
        Template::Landing
    );
    for other in [
        "template = \"blog@1\"\n",
        "",
        "not toml [",
        "template = 3\n",
    ] {
        assert_eq!(Template::of(other), Template::Blog, "{other}");
    }
    assert_eq!(
        (Template::Blog.component(), Template::Landing.component()),
        ("blog", "landing")
    );
    assert_eq!(
        Template::Blog.renderer_digest(),
        pigeon_blog::renderer_digest()
    );
    assert_eq!(
        Template::Landing.renderer_digest(),
        pigeon_landing::renderer_digest()
    );

    // A blog renders byte for byte as the blog renderer alone renders it, problems included.
    let blog = pigeon_blog::Input {
        site_toml:
            "template = \"blog@1\"\n\n[site]\ntitle = \"Example\"\ndescription = \"Notes.\"\n"
                .into(),
        posts: vec![(
            "2025-01-02-a.md".into(),
            "+++\ntitle = \"A\"\ndate = 2025-01-02\n+++\nHello.\n".into(),
        )],
        statics: ["apple-touch-icon", "favicon-16x16", "favicon-32x32"]
            .iter()
            .map(|f| format!("assets/images/site/{f}.png"))
            .collect(),
        base_url: "https://example.com".into(),
        commit: "abc1234".into(),
        commit_year: 2025,
        ..pigeon_blog::Input::default()
    };
    let parts = Parts {
        site_toml: blog.site_toml.clone(),
        posts: blog.posts.clone(),
        statics: blog.statics.clone(),
        base_url: blog.base_url.clone(),
        commit: blog.commit.clone(),
        commit_year: blog.commit_year,
        ..Parts::default()
    };
    let direct = pigeon_blog::render(&blog).expect("renders");
    let via = render(&parts, &|_, _| true).expect("renders");
    assert_eq!(via.kind, Template::Blog);
    assert_eq!(via.files, direct.files);
    assert_eq!(
        (via.template, via.version),
        (direct.template, direct.version)
    );
    assert_eq!(via.digest, direct.digest);
    assert_eq!(via.renderer_digest, pigeon_blog::renderer_digest());
    let broken = Parts {
        site_toml: "template = \"blog@1\"\n".into(),
        ..parts.clone()
    };
    let want: Vec<String> = pigeon_blog::render(&pigeon_blog::Input {
        site_toml: broken.site_toml.clone(),
        ..blog.clone()
    })
    .expect_err("no [site]")
    .iter()
    .map(ToString::to_string)
    .collect();
    let got: Vec<String> = render(&broken, &|_, _| true)
        .expect_err("no [site]")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(got, want);

    // The landing, from a repository's files, under the landing pin.
    let site = landing_site();
    let files = vec![
        (
            ".pigeon/site.toml".to_string(),
            site.site_toml.clone().into_bytes(),
        ),
        (
            "content/index.toml".to_string(),
            site.index.clone().unwrap().into_bytes(),
        ),
        (
            "content/posts/2025-01-02-a.md".to_string(),
            b"ignored".to_vec(),
        ),
        ("static/robots.txt".to_string(), b"User-agent: *\n".to_vec()),
        ("README.md".to_string(), vec![0xff]),
    ];
    let s = split(files).expect("sorted");
    assert!(s.has_site_toml);
    assert!(s.parts.posts.is_empty() && s.parts.index.is_some());
    assert_eq!(
        s.statics,
        [("robots.txt".to_string(), b"User-agent: *\n".to_vec())]
    );
    let parts = Parts {
        commit: "abc1234".into(),
        ..s.parts
    };
    let landing_only = |t: Template, v: &str| t == Template::Landing && v == "1.1.0";
    let out = render(&parts, &landing_only).expect("renders");
    assert_eq!((out.kind, out.template), (Template::Landing, "landing@1"));
    assert_eq!(out.renderer_digest, pigeon_landing::renderer_digest());
    assert_eq!(out.statics, ["robots.txt"]);
    let alone = pigeon_landing::render(&pigeon_landing::Input {
        statics: ["robots.txt".to_string()].into(),
        ..landing_site()
    })
    .expect("renders");
    assert_eq!(out.files, alone.files);
    let problems =
        render(&parts, &|t, _| t == Template::Blog).expect_err("the landing pin allows none");
    assert!(
        problems[0]
            .to_string()
            .starts_with(".pigeon/site.toml: this pigeon carries landing@1 versions 1.1.0"),
        "{problems:?}"
    );
    // A repository with no site.toml is the blog's, as before; only the chosen template's files
    // must be text.
    let none = split(vec![("content/pages/a.md".into(), b"x".to_vec())]).unwrap();
    assert!(!none.has_site_toml);
    assert!(split(vec![("content/index.toml".into(), vec![0xff])]).is_ok());
    assert!(
        split(vec![
            (
                ".pigeon/site.toml".into(),
                b"template = \"landing@1\"".to_vec()
            ),
            ("content/index.toml".into(), vec![0xff]),
        ])
        .is_err()
    );
}
