# Contributing to Floppa VPN

This file covers the contribution **process**. For what the pieces are and how they fit
together, read [CLAUDE.md](CLAUDE.md) — it is written for agents but is the most current
description of the architecture, and the commands below are its commands.

## Before you start

- For non-trivial changes, open an issue first to discuss the approach.
- Run the checks locally. CI runs the same ones and nothing else.

## Checks

```sh
just check          # everything: fmt, clippy, tests, frontend types and lint
just client-check   # the client half — Tauri crates, shared crates, Kotlin, Android clippy
just server-check   # the server half — needs a live database for sqlx
```

Two things are generated and must never be hand-edited. Both take about a second, and a
stale one is a broken build rather than a stale comment:

```sh
just bindings       # after any change to a #[specta::specta] command or a #[derive(Type)]
just openapi        # after any change to the server's HTTP contract
```

If you touched a `query!` / `query_as!` / `query_scalar!`, run `just sqlx-prepare` and commit
`.sqlx/` — CI compiles the queries offline and will fail without it.

## Commits

Conventional prefixes, scoped where it helps:

- `feat:` — new behaviour
- `fix:` — a bug fix
- `refactor:` — no behaviour change
- `docs:` — documentation only
- `chore:` / `ci:` / `build:` — tooling, release, dependencies

Scope by the part of the system: `fix(android): …`, `feat(client): …`, `fix(actor): …`,
`feat(server): …`.

Write the message about **why**, and about what the change makes impossible or fixes. The
diff already says what moved. If a bug had a symptom on a device, name the symptom — that is
what someone reading the history in six months will be searching for.

## Changelog entry (required for user-visible changes)

The changelog is not a formality here: `floppa-web-shared/src/changelog.json` is shown to
every user inside the app under "What's new", in English and Russian, and CI renders the same
entry into the GitHub release body.

Add your entry to the `sections` of the current version, in both languages:

```jsonc
{ "type": "fixed", "items": [{ "en": "…", "ru": "…" }] }
```

Types are `added`, `changed`, `fixed` and `notes` — the app's own schema allows no others.

Write it for the person holding the phone. Not "gate the IPv6 route on the tunnel's address
family" but "VLESS carried no traffic at all, because …". If you cannot say what a change does
for a user, it probably does not need an entry.

**Skip the entry** for internal refactors with no behaviour change, and for docs-only,
CI-only or test-only changes.

## Pull requests

- Target `main`, one logical change per PR.
- Say **why** in the description. Add a "Testing" section only for what CI cannot cover —
  hardware, a specific device, a network condition you reproduced by hand.
- CI must pass.

## Releasing

1. `./scripts/new-changelog.py X.Y.Z` — rotates the current entry into `history` and leaves a
   stub for the new version.
2. Replace every `TODO` in that stub. `scripts/release-notes.py` refuses to render a release
   that still has one, so a stub cannot reach a release body.
3. Set `version` in `floppa-client/src-tauri/tauri.conf.json5` — the single source of truth.
   Everything else is derived: `Cargo.toml` by `scripts/sync-version.ts`, the Android
   `versionCode` by the Tauri CLI.
4. Commit, then `git tag vX.Y.Z && git push && git push origin vX.Y.Z`.
5. CI builds every platform and opens a **draft** release. Check it, then publish.
