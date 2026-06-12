# Kits

A kit is a starter pack: a composition of packages, profiles, and a
direction, installed in one gesture — `elanus init [dir] --kit <name-or-path>`
(the flag repeats for several kits). It exists so "set up elanus for X" is
one command instead of a page of copy-paste.

The format is a directory:

```
kits/dev/
  packages/           copied into the root's packages/, then granted exactly
                      the way init's stock packages are — init IS the human
                      install gesture, so kit packages carry the same "init"
                      provenance in the grants ledger
  profiles/<name>/    profile files copied only if missing; an existing
                      profile is never clobbered
  README.md           printed after init — the kit's direction: what to set,
                      what to try first
```

Nothing else is special. A kit's packages are ordinary packages: their
manifests are requests, their grants pin to the manifest+code hash, editing
them re-enters review like anything else. A kit's profile is an ordinary
profile. The kit is gone after install — there is no kit registry, no
update channel; the root is self-contained and yours to edit.

Resolution: a `--kit` value containing `/` is used as a path directly. A
bare name resolves against `$ELANUS_KIT_PATH` (colon-separated directories,
each containing kit dirs by name), then against a `kits/` directory found by
walking up from the elanus executable — that last hop is a dev convenience
so a repo build finds `<repo>/kits`; packaged installs should set
`ELANUS_KIT_PATH`.
