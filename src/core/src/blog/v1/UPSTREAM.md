# blog@1's upstream

`blog@1` 1.0.0 carries the design of noisypigeon's Zola theme `site-template-blog` **1.0.5**:

- repository `github.com/noisypigeon/pigeon`, path `src/pigeon-site-templates/blog/`;
- tag `site-template-blog/v1.0.5`, commit `a7a7de43`;
- licence GPL-3.0-or-later, the same as pigeon.

`main.css`, `tokens.css` and `pigeon.css` here are byte copies of its `static/assets/css/`. Its
`templates/` are written as Rust in `../pages.rs` and `../feeds.rs`, and Zola 0.23.6's Markdown
rules in `../markdown.rs`. Departures are listed in `docs/plans/blog.md`.
