# Kits

A kit is a starter pack: a composition of packages, profiles, and a
direction, installed in one gesture — `elanus kit add <name-or-path>` on an
existing root, or `elanus init [dir] --kit <name-or-path>` at creation (the
flag repeats for several kits). It exists so "set up elanus for X" is one
command instead of a page of copy-paste.

The format is a directory:

```
kits/dev/
  packages/           linked onto the root's package path (default) or
                      vendored into packages/ with --copy; either way they
                      are granted the way init's stock packages are — the
                      install is the human gesture, so grants carry
                      provenance kit:<name> in the ledger
  profiles/<name>/    profile files copied only if missing; an existing
                      profile is never clobbered (identity is meant to be
                      edited, packages to be shared)
  README.md           printed after install — the kit's direction: what to
                      set, what to try first
```

Two install modes:

- **Link (default).** The kit's `packages/` dir is appended to the default
  profile's `package_path`. The kit dir stays the source: a shared kit can
  be managed in one place (even by a single agent for a fleet of roots),
  and every linking root re-reviews when it changes — grants pin the
  manifest+code hash, so an upstream edit goes stale *at dispatch* and
  re-enters review. Copying a package into the root's `packages/` shadows
  the link (first-hit-wins): that is fork-to-customize.
- **Copy (`--copy`).** Packages are vendored into the root, which becomes
  fully self-contained. Use it when the kit source won't stay around.

Nothing else is special. A kit's packages are ordinary packages: their
manifests are requests, their grants pin to the manifest+code hash, editing
them re-enters review like anything else. A kit's profile is an ordinary
profile. There is no runtime kit entity — nothing consults "the kit" after
install, there is no registry, no update channel; "what did kit X install"
is a provenance query against the grants ledger (`decided_by = 'kit:X'`).

CLI: `elanus kit add <ref> [--copy]`, `elanus kit list` (kits installable
right now, in resolution order), `elanus kit show <ref>` (README without
installing).

Resolution: a ref containing `/` is used as a path directly. A bare name
resolves, in order, against `$ELANUS_KIT_PATH` (an override, not the
mechanism), **`<root>/kits`** — the configured home: `elanus init` seeds it
with the stock `core` kit, and dropping a directory there is the whole
install story — then `~/.elanus/kits` (user-level, shared across roots),
then a `kits/` directory found walking up from the elanus executable (dev
convenience so a repo build finds `<repo>/kits`).
