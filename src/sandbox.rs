//! The cage and the camera (docs/sandbox.md).
//!
//! Cage: OS-enforced write restriction applied at spawn — Seatbelt via
//! sandbox-exec on macOS (deprecated-but-functional; same mechanism Claude
//! Code's sandbox uses). Inherited across fork/exec, so it covers the whole
//! process tree of a shell tool call. No enforcement off macOS yet (Landlock
//! lands with the VPS move); the camera works everywhere.
//!
//! Camera: boundary stat-diff of the writable roots around each tool call.
//! The cage is what makes the camera complete: writes can only land inside
//! the diffed roots. Events are trace lines today (topic `obs/fs/<path>`), bus
//! observations later.

use crate::paths::Root;
use crate::profile::SandboxCfg;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Snapshot cap: a walk this large is a misconfigured root, not a workspace.
const WALK_CAP: usize = 100_000;

pub struct Cage {
    /// Canonical write roots: the harness root + fs_write. Camera scope.
    pub write_roots: Vec<PathBuf>,
    /// Camera exclusions, prefix-matched against root-relative paths.
    pub exclude: Vec<String>,
    /// Seatbelt profile, when enforcement is on (fs_write nonempty + macOS).
    sbpl: Option<String>,
}

/// Paths the cage fences from actors even though they sit inside an allowed
/// write root (docs/identity.md): the kernel and the human's uncaged
/// surfaces write them, caged actors must not.
///
/// - `deny_write_files`: the approvals ledger and its write-ahead log (exact
///   files) — where a committed grant would have to land, so denying these
///   stops an actor granting itself authority by editing the database. The
///   `-shm` index is spared: read-only consumers (the history view) need it,
///   and it cannot conjure an approved row while the db and its log are
///   write-denied.
/// - `deny_write_trees`: the profiles directory — a profile confers authority
///   (writable prefixes, model, skill visibility) with no grant gate, so an
///   agent editing one would escalate — and the config repo (its `live` tree
///   and `.git`), kernel-owned truth an agent must never rewrite directly; it
///   proposes into its own clone instead. Both stay readable. The human edits
///   them from an uncaged surface; caged actors are kept out.
/// - `deny_all_trees`: the secret store — neither readable nor writable by
///   any actor.
pub struct Protect {
    pub deny_write_files: Vec<PathBuf>,
    pub deny_write_trees: Vec<PathBuf>,
    pub deny_all_trees: Vec<PathBuf>,
}

impl Protect {
    // PRECONDITION: root.dir is canonical. macOS SBPL `subpath` matches the real
    // (inode) path, so a deny rule given via a symlinked path component would not
    // bind and the fence would silently not apply. paths::resolve() canonicalizes
    // every root, and seatbelt_actually_cages exercises the config fence against
    // a real cage to catch a regression of this invariant.
    pub fn for_root(root: &Root) -> Protect {
        let db = root.db();
        let wal = db.with_extension("db-wal");
        Protect {
            deny_write_files: vec![db, wal],
            // profiles confer authority; the config repo (live tree + .git) is
            // kernel-owned truth an agent must not silently rewrite — both are
            // readable but write-fenced (docs/config.md). The agent proposes
            // into its own clone (increment 3), never the live tree directly.
            deny_write_trees: vec![root.profiles(), root.config()],
            deny_all_trees: vec![root.secrets()],
        }
    }
}

impl Cage {
    pub fn from_profile(root: &Root, cfg: &SandboxCfg) -> Cage {
        let mut roots: Vec<PathBuf> = vec![root.dir.clone()];
        for w in &cfg.fs_write {
            let p = if Path::new(w).is_absolute() {
                PathBuf::from(w)
            } else {
                root.dir.join(w)
            };
            match p.canonicalize() {
                Ok(c) => roots.push(c),
                Err(e) => eprintln!("[sandbox] fs_write {w:?} ignored: {e}"),
            }
        }
        // The whole-agent grant cage only enforces when fs_write is declared:
        // an empty declaration means "no cage", v1 behavior preserved.
        Cage::from_roots(
            roots,
            cfg.exclude_or_default(),
            !cfg.fs_write.is_empty(),
            &Protect::for_root(root),
        )
    }

    /// A cage over an explicit write set — what leases and package actors
    /// spawn under. `enforce` still requires a platform mechanism; without
    /// one the cage is camera-scope only (warned, never silent). `protect`
    /// fences the ledger and the secret store from the actor even though they
    /// sit inside a write root (docs/identity.md).
    pub fn from_roots(
        roots: Vec<PathBuf>,
        exclude: Vec<String>,
        enforce: bool,
        protect: &Protect,
    ) -> Cage {
        let mut roots = roots;
        roots.sort();
        roots.dedup();
        // Drop roots nested inside another: one walk, one subpath rule.
        let mut top: Vec<PathBuf> = Vec::new();
        for r in roots {
            if !top.iter().any(|t| r.starts_with(t)) {
                top.push(r);
            }
        }
        let can_enforce =
            enforce && cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists();
        if enforce && !can_enforce {
            eprintln!(
                "[sandbox] enforcement requested but no mechanism on this platform; camera only"
            );
        }
        let sbpl = can_enforce.then(|| sbpl(&top, protect));
        Cage {
            write_roots: top,
            exclude,
            sbpl,
        }
    }

    /// Wrap an arbitrary program (a package actor's `run`) in the cage.
    pub fn command(&self, program: &Path) -> std::process::Command {
        match &self.sbpl {
            Some(profile) => {
                let mut c = std::process::Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(profile).arg(program);
                c
            }
            None => std::process::Command::new(program),
        }
    }

    pub fn enforcing(&self) -> bool {
        self.sbpl.is_some()
    }

    /// Build the command that runs `sh -c cmd`, caged when enforcement is on.
    pub fn shell_command(&self, cmd: &str) -> std::process::Command {
        match &self.sbpl {
            Some(profile) => {
                let mut c = std::process::Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(profile).arg("sh").arg("-c").arg(cmd);
                c
            }
            None => {
                let mut c = std::process::Command::new("sh");
                c.arg("-c").arg(cmd);
                c
            }
        }
    }
}

impl SandboxCfg {
    fn exclude_or_default(&self) -> Vec<String> {
        self.capture_exclude.clone()
    }
}

/// Seatbelt: allow everything except file writes; allow writes only inside
/// the write roots, system temp, and /dev. Temp dirs are an accepted hole
/// (staging is visible-by-absence; exfil needs network, the cage's other
/// half — docs/sandbox.md).
fn sbpl(write_roots: &[PathBuf], protect: &Protect) -> String {
    let mut allow = String::new();
    for r in write_roots {
        allow.push_str(&format!(
            "  (subpath \"{}\")\n",
            sbpl_escape(&r.display().to_string())
        ));
    }
    // Fence the ledger, the profiles tree, and the secrets even though they
    // live inside a write root. SBPL is last-match-wins, so these denials come
    // AFTER the allow block and override it. The kernel and exec handlers run
    // uncaged and are unaffected.
    let mut fence = String::new();
    for p in &protect.deny_write_files {
        fence.push_str(&format!(
            "(deny file-write* (literal \"{}\"))\n",
            sbpl_escape(&p.display().to_string())
        ));
    }
    for p in &protect.deny_write_trees {
        fence.push_str(&format!(
            "(deny file-write* (subpath \"{}\"))\n",
            sbpl_escape(&p.display().to_string())
        ));
    }
    for p in &protect.deny_all_trees {
        // Deny both directions: unreadable and unwritable.
        fence.push_str(&format!(
            "(deny file-read* (subpath \"{p}\"))\n(deny file-write* (subpath \"{p}\"))\n",
            p = sbpl_escape(&p.display().to_string())
        ));
    }
    format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n{allow}\
         \x20 (subpath \"/private/tmp\")\n  (subpath \"/private/var/folders\")\n  (subpath \"/dev\")\n)\n{fence}"
    )
}

fn sbpl_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

type Stamp = (SystemTime, u64); // (mtime, len)

pub struct Snapshot {
    files: HashMap<PathBuf, Stamp>,
    pub capped: bool,
}

#[derive(Debug)]
pub struct Change {
    pub op: &'static str, // create | modify | unlink
    pub path: PathBuf,
    pub size: u64,
}

pub fn snapshot(cage: &Cage) -> Snapshot {
    let mut files = HashMap::new();
    let mut capped = false;
    for root in &cage.write_roots {
        walk(root, root, &cage.exclude, &mut files, &mut capped);
    }
    Snapshot { files, capped }
}

fn walk(
    dir: &Path,
    rel_root: &Path,
    exclude: &[String],
    out: &mut HashMap<PathBuf, Stamp>,
    capped: &mut bool,
) {
    if *capped {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        if out.len() >= WALK_CAP {
            *capped = true;
            return;
        }
        let p = e.path();
        if let Ok(rel) = p.strip_prefix(rel_root) {
            let rel_s = rel.to_string_lossy();
            if exclude
                .iter()
                .any(|x| rel_s.starts_with(x.as_str()) || rel_s == x.trim_end_matches('/'))
            {
                continue;
            }
        }
        // symlink_metadata: never follow links — a link's target may be
        // outside the roots, and following would both lie and loop.
        let Ok(md) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            walk(&p, rel_root, exclude, out, capped);
        } else {
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.insert(p, (mtime, md.len()));
        }
    }
}

pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    let mut out = Vec::new();
    for (p, stamp) in &after.files {
        match before.files.get(p) {
            None => out.push(Change {
                op: "create",
                path: p.clone(),
                size: stamp.1,
            }),
            Some(b) if b != stamp => out.push(Change {
                op: "modify",
                path: p.clone(),
                size: stamp.1,
            }),
            _ => {}
        }
    }
    for (p, stamp) in &before.files {
        if !after.files.contains_key(p) {
            out.push(Change {
                op: "unlink",
                path: p.clone(),
                size: stamp.1,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

// ── Read camera status (read-provenance M3) ──────────────────────────────────
//
// The read camera is HONESTLY TWO-TIER (read-provenance handoff, sandbox.md):
//
//   ADVISORY    (M1, shipping): the tool-stream read events — Claude Code's
//               Read/Grep/Glob projected into the spatial `obs/fs/<path>` read
//               flavor. Available on EVERY platform (it rides events already on
//               the bus); on/off is the `sandbox.read_camera` config toggle.
//               Honest-agent tier only — a `Bash`+`cat` walks around it.
//
//   AUTHORITATIVE (M2, NOT BUILT — deferred): the cage/syscall read camera that
//               sits below the shell and catches shell-buried reads. The only
//               unprivileged authoritative mechanism is Linux seccomp
//               user-notification (SECCOMP_USER_NOTIF); macOS has no free option
//               (needs the Endpoint-Security entitlement + a signed system
//               extension) and is an ACCEPTED GAP. So this tier is UNAVAILABLE
//               here on macOS, and reported as such — never a silent no-op. We do
//               not build M2 in M3; M3 only reports its availability honestly.
//
// This mirrors how the cage detects write-enforcement availability above
// (`enforce && cfg!(target_os = "macos") && /usr/bin/sandbox-exec exists`): the
// authoritative read tier's availability is a platform + mechanism-presence
// probe, here `cfg!(target_os = "linux") && seccomp_unotify_present()`.

/// One tier of the read camera: whether the mechanism is available on this
/// platform/build, and (when available) whether it is currently enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierStatus {
    /// Does the mechanism exist on this platform/privilege/build at all?
    pub available: bool,
    /// When available, is it switched on? (Meaningless when `available` is
    /// false — an unavailable tier is never "on".)
    pub enabled: bool,
}

/// The full read-camera status surfaced on the trust/status surface (M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCameraStatus {
    /// M1 — the advisory tool-stream tier. Available everywhere; `enabled`
    /// reflects `sandbox.read_camera`.
    pub advisory: TierStatus,
    /// M2 — the authoritative cage/syscall tier. NOT BUILT; reported only.
    pub authoritative: TierStatus,
}

impl ReadCameraStatus {
    /// Can a READ-flavor subscription be honored right now? True when the
    /// advisory tier is ON (events flow today) OR the authoritative tier is
    /// available on this platform (M2 — not built, so this only becomes true on
    /// Linux and still publishes nothing today, but the predicate is honest about
    /// platform availability for when M2 lands). When this is FALSE, the broker
    /// fast-fails a read-flavor subscribe with SUBACK 0x87 rather than returning a
    /// silently-empty subscription (the history-503 lesson, read-provenance M3).
    pub fn read_flavor_honorable(&self) -> bool {
        self.advisory.enabled || self.authoritative.available
    }
}

/// Detect whether the authoritative (M2) read camera mechanism could run here.
///
/// M2 is NOT BUILT — this is purely the availability probe M3 reports. The only
/// unprivileged authoritative path is Linux seccomp user-notification, so the
/// mechanism is "present" only on Linux. (Even on Linux M2's code does not exist
/// yet, so `enabled` stays false; this reports the *platform capability*, which
/// is what "unavailable here" on macOS is honestly distinguishing.)
fn authoritative_read_available() -> bool {
    cfg!(target_os = "linux")
}

/// Compute the read-camera status from the active sandbox config.
///
/// - advisory:      available everywhere; enabled = `cfg.read_camera`.
/// - authoritative: available only where the mechanism exists (Linux);
///                  enabled = false always (M2 unbuilt — the deferred tier).
pub fn read_camera_status(cfg: &SandboxCfg) -> ReadCameraStatus {
    ReadCameraStatus {
        advisory: TierStatus {
            available: true,
            enabled: cfg.read_camera,
        },
        authoritative: TierStatus {
            available: authoritative_read_available(),
            // M2 is the deferred authoritative tier — not built here, so it can
            // never be on even where the platform could host it.
            enabled: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root() -> Root {
        let dir = std::env::temp_dir().join(format!("elanus-sbx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Root {
            dir: dir.canonicalize().unwrap(),
        }
    }

    fn cage_for(root: &Root) -> Cage {
        Cage {
            write_roots: vec![root.dir.clone()],
            exclude: vec!["run/".into(), "elanus.db".into()],
            sbpl: None,
        }
    }

    #[test]
    fn read_camera_status_two_tiers() {
        // The advisory tier (M1) is available on EVERY platform — it rides events
        // already on the bus — and its `enabled` mirrors the config toggle.
        let on = read_camera_status(&SandboxCfg {
            read_camera: true,
            ..Default::default()
        });
        assert!(on.advisory.available, "advisory tier available everywhere");
        assert!(on.advisory.enabled, "toggle ON ⇒ advisory enabled");

        let off = read_camera_status(&SandboxCfg {
            read_camera: false,
            ..Default::default()
        });
        assert!(off.advisory.available, "advisory still AVAILABLE when off");
        assert!(!off.advisory.enabled, "toggle OFF ⇒ advisory disabled");

        // The authoritative tier (M2) is platform-gated and NOT BUILT: never
        // enabled, and only "available" where the unprivileged mechanism could run
        // (Linux seccomp-unotify). On macOS — the dev machine, the accepted gap —
        // it is "unavailable here", reported honestly, never a silent no-op.
        assert!(!on.authoritative.enabled, "M2 unbuilt ⇒ never enabled");
        assert_eq!(
            on.authoritative.available,
            cfg!(target_os = "linux"),
            "authoritative available iff Linux"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            !on.authoritative.available,
            "non-Linux (e.g. macOS) ⇒ authoritative unavailable here"
        );
    }

    #[test]
    fn read_flavor_honorable_predicate() {
        // The exact broker fast-fail predicate (read-provenance M3). A read-flavor
        // subscribe is honorable when advisory is ON or authoritative is available.
        let advisory_on = ReadCameraStatus {
            advisory: TierStatus { available: true, enabled: true },
            authoritative: TierStatus { available: false, enabled: false },
        };
        assert!(advisory_on.read_flavor_honorable(), "advisory on ⇒ honorable");

        // Advisory OFF and authoritative unavailable here (the macOS-off case): NOT
        // honorable ⇒ the broker fast-fails rather than returning empty.
        let both_off = ReadCameraStatus {
            advisory: TierStatus { available: true, enabled: false },
            authoritative: TierStatus { available: false, enabled: false },
        };
        assert!(
            !both_off.read_flavor_honorable(),
            "advisory off + authoritative unavailable ⇒ NOT honorable (fast-fail)"
        );

        // Advisory off but authoritative AVAILABLE (a future Linux M2): honorable —
        // the platform can serve reads even with the advisory tier switched off.
        let auth_avail = ReadCameraStatus {
            advisory: TierStatus { available: true, enabled: false },
            authoritative: TierStatus { available: true, enabled: false },
        };
        assert!(
            auth_avail.read_flavor_honorable(),
            "authoritative available ⇒ honorable even with advisory off"
        );
    }

    #[test]
    fn camera_sees_create_modify_unlink() {
        let root = tmp_root();
        std::fs::write(root.dir.join("keep.txt"), "a").unwrap();
        std::fs::write(root.dir.join("gone.txt"), "b").unwrap();
        std::fs::write(root.dir.join("changed.txt"), "c").unwrap();
        let cage = cage_for(&root);
        let before = snapshot(&cage);
        std::fs::write(root.dir.join("new.txt"), "x").unwrap();
        std::fs::write(root.dir.join("changed.txt"), "longer content").unwrap();
        std::fs::remove_file(root.dir.join("gone.txt")).unwrap();
        let after = snapshot(&cage);
        let changes = diff(&before, &after);
        let ops: Vec<(&str, String)> = changes
            .iter()
            .map(|c| {
                (
                    c.op,
                    c.path.file_name().unwrap().to_string_lossy().to_string(),
                )
            })
            .collect();
        assert!(ops.contains(&("create", "new.txt".into())), "{ops:?}");
        assert!(ops.contains(&("modify", "changed.txt".into())), "{ops:?}");
        assert!(ops.contains(&("unlink", "gone.txt".into())), "{ops:?}");
        assert_eq!(changes.len(), 3, "keep.txt must not appear: {ops:?}");
    }

    #[test]
    fn camera_respects_excludes() {
        let root = tmp_root();
        std::fs::create_dir_all(root.dir.join("run")).unwrap();
        let cage = cage_for(&root);
        let before = snapshot(&cage);
        std::fs::write(root.dir.join("run/d1.out"), "noise").unwrap();
        std::fs::write(root.dir.join("elanus.db-wal"), "noise").unwrap();
        std::fs::write(root.dir.join("real.txt"), "signal").unwrap();
        let after = snapshot(&cage);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].path.file_name().unwrap(), "real.txt");
    }

    #[test]
    fn sbpl_contains_roots_and_denies_writes() {
        let protect = Protect {
            deny_write_files: vec![
                PathBuf::from("/r/elanus.db"),
                PathBuf::from("/r/elanus.db-wal"),
            ],
            deny_write_trees: vec![PathBuf::from("/r/profiles"), PathBuf::from("/r/config")],
            deny_all_trees: vec![PathBuf::from("/r/.secrets")],
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect);
        assert!(p.contains("(deny file-write*)"));
        assert!(p.contains("(subpath \"/tmp/ws\")"));
        assert!(p.contains("(subpath \"/dev\")"));
        // The ledger and its log are fenced; the -shm index is not.
        assert!(p.contains("(deny file-write* (literal \"/r/elanus.db\"))"));
        assert!(p.contains("(deny file-write* (literal \"/r/elanus.db-wal\"))"));
        assert!(!p.contains("elanus.db-shm"));
        // Profiles are write-fenced (a profile confers authority).
        assert!(p.contains("(deny file-write* (subpath \"/r/profiles\"))"));
        // The config repo is write-fenced (kernel-owned truth); still readable.
        assert!(p.contains("(deny file-write* (subpath \"/r/config\"))"));
        assert!(!p.contains("(deny file-read* (subpath \"/r/config\"))"));
        // Secrets are fenced both ways.
        assert!(p.contains("(deny file-read* (subpath \"/r/.secrets\"))"));
        // The fence comes AFTER the allow block (last-match-wins).
        let allow_at = p.find("(allow file-write*").unwrap();
        let deny_at = p.find("(deny file-write* (literal").unwrap();
        assert!(deny_at > allow_at, "fence must override the allow");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_actually_cages() {
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            return;
        }
        let root = tmp_root();
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        // Inside the cage: allowed.
        let ok = cage
            .shell_command(&format!("echo hi > {}/in.txt", root.dir.display()))
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "write inside roots must succeed: {ok:?}"
        );
        // Outside (home dir): denied. Process tree inheritance included.
        let target = std::env::temp_dir().join(format!("elanus-escape-{}", uuid::Uuid::new_v4()));
        // NB: temp is an allowed hole; use a path that is definitely outside:
        let home_target = format!(
            "{}/elanus-cage-escape-test.txt",
            std::env::var("HOME").unwrap()
        );
        let denied = cage
            .shell_command(&format!("sh -c 'echo escape > {home_target}'"))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&home_target);
        let _ = std::fs::remove_file(&target);
        assert!(!denied.status.success(), "write outside roots must fail");

        // The ledger is fenced even though it sits inside an allowed root: an
        // actor may read it (the history view does) but never write it, so it
        // cannot grant itself authority by editing the database.
        let db = root.db();
        std::fs::write(&db, "seed").unwrap(); // the uncaged test stands in for the kernel
        let db_write = cage
            .shell_command(&format!("echo x >> {}", db.display()))
            .output()
            .unwrap();
        assert!(
            !db_write.status.success(),
            "writing the ledger from the cage must fail"
        );
        let db_read = cage
            .shell_command(&format!("cat {} > /dev/null", db.display()))
            .output()
            .unwrap();
        assert!(
            db_read.status.success(),
            "reading the ledger from the cage must succeed"
        );

        // The secret store is unreadable from the cage.
        std::fs::create_dir_all(root.secrets()).unwrap();
        let tok = root.secrets().join("tok");
        std::fs::write(&tok, "s3cr3t").unwrap();
        let sec_read = cage
            .shell_command(&format!("cat {}", tok.display()))
            .output()
            .unwrap();
        assert!(
            !sec_read.status.success(),
            "reading a secret from the cage must fail"
        );

        // Profiles confer authority; a caged actor cannot write them.
        std::fs::create_dir_all(root.profiles()).unwrap();
        let prof = root.profiles().join("default");
        std::fs::create_dir_all(&prof).unwrap();
        let prof_write = cage
            .shell_command(&format!(
                "echo 'fs_write=[\"/\"]' >> {}",
                prof.join("profile.toml").display()
            ))
            .output()
            .unwrap();
        assert!(
            !prof_write.status.success(),
            "editing a profile from the cage must fail"
        );

        // The config repo is kernel-owned truth: a caged actor must NOT be able
        // to rewrite live config, but a daemon MUST be able to read its own
        // config (docs/config.md). Write-fenced, read-allowed — the increment-2
        // security property, asserted against the real seatbelt, not just the
        // SBPL string.
        std::fs::create_dir_all(root.config_packages()).unwrap();
        let cfgfile = root.config_packages().join("watcher.toml");
        std::fs::write(&cfgfile, "accounts = [\"alice\"]\n").unwrap(); // kernel-authored
        let cfg_write = cage
            .shell_command(&format!("echo x >> {}", cfgfile.display()))
            .output()
            .unwrap();
        assert!(
            !cfg_write.status.success(),
            "writing live config from the cage must fail"
        );
        let cfg_read = cage
            .shell_command(&format!("cat {} > /dev/null", cfgfile.display()))
            .output()
            .unwrap();
        assert!(
            cfg_read.status.success(),
            "reading own config from the cage must succeed"
        );
        // ...and the repo's history (.git) is unwritable too.
        std::fs::create_dir_all(root.config().join(".git")).unwrap();
        let git_write = cage
            .shell_command(&format!(
                "echo x >> {}",
                root.config().join(".git/HEAD").display()
            ))
            .output()
            .unwrap();
        assert!(
            !git_write.status.success(),
            "writing config/.git from the cage must fail"
        );
    }
}
