# Commit attribution: new commits name the person on GitHub

Status: **designed; built on a branch, not landed** (planner run `fHVIjaxK6nJZ`).

## Goal

Every git commit pigeon makes from now on is attributed on GitHub to the person's user,
`noisypigeon`, through their public noreply address
`167243387+noisypigeon@users.noreply.github.com`. GitHub links a commit to a user by the commit's
author email, so that address goes in the author line. Existing commits stay as they are: history
is never rewritten.

## Where pigeon makes or rewrites commits (main at `b40f820`)

| Where | What it writes | Author today | Committer today |
| --- | --- | --- | --- |
| Lander, rebase onto main (`src/capabilities/src/land/merge.rs`, `pigeon_core::rebase::commit`) | When main moved since the branch's base, the branch's change laid onto main as **one** commit, with the tip's message | the tip's author, kept byte for byte | `pigeon <land@pigeon.dev>` |
| Lander, fast-forward (`land.rs`, `merge::push`) | Nothing new: main moves to the branch's own commits | unchanged | unchanged |
| Lander, landing notes (`land.rs`, `notes_commit`, `refs/notes/land`) | One notes commit per push of notes | `pigeon <land@pigeon.dev>` | the same |
| CI gateway, job notes (`src/ci/src/notes.rs`, `refs/notes/ci`) | One notes commit per push of notes | `pigeon <ci@pigeon.dev>` | the same |
| AI agent runs (`src/ai-agents/src/sandbox.rs` asks `pigeon-sandboxd`'s `prepare`, which runs `git config user.name/user.email`; `commit_push` runs `git commit`) | One commit per `commit_push` | `pigeon AI agent run <id> <ai-agents@pigeon.dev>` | the same |
| GitHub mirror (`mirror.rs`) | Nothing new: copies the objects as they are | — | — |
| Git proxy (`git_proxy.rs`) | Nothing new: passes pushes through, may move or delete a ref with an empty pack | — | — |
| Sites and blog deploys (`src/sites`, `src/blog`, `github.rs`) | No commits: they read a tarball and write R2 | — | — |
| Changelog (docs/plans/changelog.md) | No commits: it writes R2 objects | — | — |

Only main, the session branches and tags reach GitHub (the mirror copies `main` and tags). The
notes refs never do, but their commits follow the same scheme so there is one rule.

What pigeon cannot control: **commits made in Claude Code sessions**. They use the session's own
git config, which today writes `Claude <noreply@anthropic.com>` as author and committer (239 of
the 271 commits on main). The Lander keeps that author when it rebases. To have those commits
name the person, the person sets the identity in the Claude Code environment, one of:

- environment variables in the cloud environment's settings: `GIT_AUTHOR_NAME=noisypigeon` and
  `GIT_AUTHOR_EMAIL=167243387+noisypigeon@users.noreply.github.com` (leave the committer as
  Claude, which keeps who typed the commit visible); or
- an `env` block with the same two variables in `.claude/settings.json`. That file is generated
  from the policy and changes only with the person's passkey, so this is the person's call.

## The scheme

- **Author: the person.** Name `noisypigeon`, email
  `167243387+noisypigeon@users.noreply.github.com`, on every commit pigeon writes itself: notes
  commits and AI agent commits.
- **Committer: pigeon.** The part of pigeon that wrote it, as today: `pigeon <land@pigeon.dev>`,
  `pigeon <ci@pigeon.dev>`, `pigeon AI agent run <id> <ai-agents@pigeon.dev>`. This keeps it
  visible that a machine made the commit, and which part.
- **A rebase keeps the author.** When the Lander lays a branch onto main it keeps the tip
  commit's author line byte for byte (name, email, time and zone) and sets only the committer.
  This was already so; it is now covered by the smoke suite. The argument for keeping it: the
  author is whoever wrote the change, and rewriting it would falsify a commit the Lander did not
  write. Commits that already name the person keep naming them.
  - One limit, unchanged: a rebase lays the whole branch down as one commit with the tip's
    author and message, so the authors of earlier commits on that branch are not carried. Adding
    `Co-authored-by:` trailers for them is a possible follow-up; it is not built here.
- **One setting.** The person's identity and pigeon's committer identities live in one module,
  `pigeon_core::attribution`. No other code spells them out.
- **No signatures.** pigeon never writes a `gpgsig` header. The rebase drops the original's
  signature, since it no longer holds. If the person turns on GitHub's vigilant mode, unsigned
  commits that name them show as "Unverified"; a real signing key for pigeon would be a separate
  piece of work.

### AI agent runs without touching `ci/`

`pigeon-sandboxd` lives in `ci/sandbox/`, which this work does not change. Its `prepare` sets
`user.name` and `user.email`, which git uses for both author and committer. Git 2.22 and later
(the image has Debian bookworm's 2.39) also reads `author.name` and `author.email`, which win for
the author only. So after each successful `prepare`, the ai-agents Worker runs one `exec` in the
checkout that sets those two keys to the person. `prepare` keeps setting the committer.

## Phases

1. `pigeon_core::attribution` with the identities; `gitobj::Object::commit` takes an author and
   a committer; the Lander and CI notes use them; the Lander's rebase committer comes from it.
2. The ai-agents Worker sets the author keys after `prepare`.
3. Smoke: the notes commits name the person as author and pigeon as committer; a rebase keeps
   the original author line byte for byte and only changes the committer; the sandbox's
   author command sets exactly the person's identity.

## Follow-ups for the person

- Set the Claude Code identity as above, if wanted.
- Optional, in `ci/`: give `sandboxd`'s `prepare` optional `author_name` and `author_email`
  fields that set `author.name`/`author.email`, so the extra `exec` can go.
