# Plans

This directory is the record of what was planned, what was built and where the building departed
from the plan. Each plan says its status in its first lines and is updated as it is built; the
code is the last word, and the architecture page is drawn from it.

A design made in Claude Docs (or any other artifact) is a working draft for talking a plan
through. Once a plan here exists, the plan wins where the two differ, and the draft is not kept
up to date.

| Plan | What | Status |
| --- | --- | --- |
| [route-through-platform.md](route-through-platform.md) | Cloud sessions reach git and the Cloudflare API through the platform's proxies (`/git/`, `/cf/`) with a proxy credential, not pasted tokens; CI runners and file reads (phase 5) | Deployed (phases 1 to 5) |
| [ci-gateway.md](ci-gateway.md) | The CI gateway: `.pigeon/ci.toml`, routing to connectors, the scheduler, job tokens, notes; Containers, Buildkite, GitHub Actions | Phases 1 and 2 deployed; phase 3 (Buildkite) being built on `claude/ci-gateway`; phase 4 to come |
| [console.md](console.md) | The console at the platform's root: tabs, sections, passkey sign-in and passkey-bound writes | Deployed |
| [friction.md](friction.md) | `call`, the land flow, read-only diagnostics, the pinned wrangler and its allowlist check, async model jobs, housekeeping | Built on `claude/friction`, awaiting deploy (the last hand-merged one) |

When a plan's status changes, change it in both places: the plan's first lines and this table.

The first change taken into main by `land` (rather than merged by hand) was a one-line docs commit on `claude/land-check-2`.
