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
    /// The read/network posture this cage runs under (docs/sandbox.md, the
    /// single-cage increment). Default = today's write-only cage. Stored so a
    /// narrowing rebuild (leases, `narrowed_cage`) carries it through unchanged.
    pub policy: CagePolicy,
    /// Seatbelt profile, when enforcement is on (fs_write nonempty + macOS).
    sbpl: Option<String>,
}

/// Network egress posture (docs/sandbox.md, wonky bit 3). Default `Open` is
/// today's behavior — no network rule at all, so the SBPL stays byte-identical.
/// `Loopback` keeps caged actors on the bus and the local HTTP read planes but
/// cuts external egress; `None` is for pure-compute spawns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    #[default]
    Open,
    Loopback,
    None,
}

impl NetworkPolicy {
    /// Parse the profile's `network` value. Unknown values warn and fall back to
    /// `Open` (the default posture) rather than silently over-restricting.
    pub fn parse(s: &str) -> NetworkPolicy {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => NetworkPolicy::Open,
            "loopback" => NetworkPolicy::Loopback,
            "none" => NetworkPolicy::None,
            other => {
                eprintln!("[sandbox] unknown network policy {other:?}; using open");
                NetworkPolicy::Open
            }
        }
    }
}

/// The read + network policy layered on top of the write cage (docs/sandbox.md,
/// the single-cage increment). Absent everywhere = `Default`, which emits no new
/// SBPL arms and keeps the profile byte-identical to the write-only cage.
///
/// Reads ship deny-list-first (`fs_read_deny`): baseline reads stay open, the
/// listed trees become unreadable on top of the secrets fence. `fs_read_allow`
/// is the experimental allow-list mode: when nonempty it flips to
/// `(deny file-read*)` with these roots plus the always-needed holes — whoever
/// sets it owns the baseline (interpreters, /usr, /System, the repo).
#[derive(Debug, Clone, Default)]
pub struct CagePolicy {
    pub network: NetworkPolicy,
    /// Deny-list mode: these canonical trees become unreadable.
    pub fs_read_deny: Vec<PathBuf>,
    /// Experimental allow-list mode: nonempty flips reads to deny-by-default
    /// with these canonical trees (plus write roots + the fixed holes) allowed.
    pub fs_read_allow: Vec<PathBuf>,
    /// Punch a NARROW outbound HTTPS hole through a non-`Open` network policy: a
    /// hosted-model client (e.g. `codex`/`codex app-server`) MUST reach its model
    /// API over TLS or it cannot run at all, but the rest of the loopback fence
    /// (arbitrary ports, plaintext egress) stays shut. Emits `(allow
    /// network-outbound (remote tcp "*:443"))` plus the resolver-socket allowance
    /// DNS needs (mDNSResponder is reached over a unix socket, so getaddrinfo
    /// works without opening raw :53 egress). No-op under `Open` (already
    /// allow-all) and irrelevant off macOS (camera-only). This is the network
    /// analogue of the sandbox "floor" a headless codex worker requires
    /// (docs/security.md entry 24): the minimum egress the tool needs to
    /// function, recorded — not a discretionary widening. The residual (a caged
    /// worker can reach ANY host on 443, since SBPL cannot pin the model host by
    /// name) is noted in docs/security.md.
    pub allow_https_egress: bool,
}

impl CagePolicy {
    fn is_default(&self) -> bool {
        self.network == NetworkPolicy::Open
            && self.fs_read_deny.is_empty()
            && self.fs_read_allow.is_empty()
            && !self.allow_https_egress
    }
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
///   write-denied. `bus.toml` is fenced here too (docs/handoffs/platform-
///   trust.md): it carries the platform trust level, so a caged agent that
///   could write it would raise its own trust. Readable (the platform stage
///   reads it), never writable.
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
            // bus.toml carries the platform trust level (M2) — write-fenced so a
            // caged agent cannot raise its own trust; still readable.
            deny_write_files: vec![db, wal, root.bus_file()],
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
        Cage::from_roots_with_policy(
            roots,
            cfg.exclude_or_default(),
            !cfg.fs_write.is_empty(),
            &Protect::for_root(root),
            policy_from_cfg(root, cfg),
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
        // The do-nothing default policy: write-only cage, byte-identical SBPL.
        // Package actors and MCP servers spawn on this in this increment; only
        // the agent shell path (from_profile) reads the read/network keys.
        Cage::from_roots_with_policy(roots, exclude, enforce, protect, CagePolicy::default())
    }

    /// `from_roots` plus an explicit read/network policy — the agent shell path
    /// and lease narrowing use this so the posture rides through. A default
    /// policy produces an SBPL byte-identical to the write-only cage.
    pub fn from_roots_with_policy(
        roots: Vec<PathBuf>,
        exclude: Vec<String>,
        enforce: bool,
        protect: &Protect,
        policy: CagePolicy,
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
        let can_enforce = enforce && enforcement_available();
        if enforce && !can_enforce {
            eprintln!(
                "[sandbox] enforcement requested but no mechanism on this platform; camera only"
            );
        }
        let sbpl = can_enforce.then(|| sbpl(&top, protect, &policy));
        Cage {
            write_roots: top,
            exclude,
            policy,
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

/// Canonicalize a profile path list the way `from_profile` does the write set:
/// absolute as given, else relative to the harness root; drop entries that do
/// not resolve (warned, never silent — a typo must not silently widen reads).
fn canon_read_list(root: &Root, kind: &str, list: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for w in list {
        let p = if Path::new(w).is_absolute() {
            PathBuf::from(w)
        } else {
            root.dir.join(w)
        };
        match p.canonicalize() {
            Ok(c) => out.push(c),
            Err(e) => eprintln!("[sandbox] {kind} {w:?} ignored: {e}"),
        }
    }
    out
}

/// Build the read/network policy from a profile's `[sandbox]` block. Absent
/// keys (network unset, empty read lists) yield the default policy, so a profile
/// that opts into nothing produces a byte-identical SBPL to the write-only cage.
fn policy_from_cfg(root: &Root, cfg: &SandboxCfg) -> CagePolicy {
    CagePolicy {
        network: cfg
            .network
            .as_deref()
            .map(NetworkPolicy::parse)
            .unwrap_or_default(),
        fs_read_deny: canon_read_list(root, "fs_read_deny", &cfg.fs_read_deny),
        fs_read_allow: canon_read_list(root, "fs_read_allow", &cfg.fs_read_allow),
        // Not a profile-`[sandbox]` key: the model-API egress hole is set in code
        // only where a hosted-model client is caged (codex_headless_cage), never
        // via generic profile config. A profile-driven cage keeps the tight fence.
        allow_https_egress: false,
    }
}

/// Seatbelt: allow everything except file writes; allow writes only inside
/// the write roots, system temp, and /dev. Temp dirs are an accepted hole
/// (staging is visible-by-absence; exfil needs network, the cage's other
/// half — docs/sandbox.md).
fn sbpl(write_roots: &[PathBuf], protect: &Protect, policy: &CagePolicy) -> String {
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
    let base = format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n{allow}\
         \x20 (subpath \"/private/tmp\")\n  (subpath \"/private/var/folders\")\n  (subpath \"/dev\")\n)\n{fence}"
    );
    // The read/network arms are APPENDED after the write cage + fence. The
    // default policy appends nothing, so the string stays byte-identical to the
    // write-only cage (M3's non-negotiable regression). Every arm is
    // last-match-wins, so the secrets fence is re-asserted after an allow-list.
    if policy.is_default() {
        return base;
    }
    format!("{base}{}", read_network_arms(write_roots, protect, policy))
}

/// The opt-in read + network SBPL, appended after the write cage. Empty when
/// the policy is default (never reached — `sbpl` short-circuits). Verified
/// against a real `sandbox-exec` in the M1 spike: `(deny network*)` cuts egress,
/// the loopback allow keeps 127.0.0.1 reachable, and an allow-list read mode
/// needs `(literal "/")` for root-path traversal or the process aborts.
fn read_network_arms(write_roots: &[PathBuf], protect: &Protect, policy: &CagePolicy) -> String {
    let mut arms = String::new();
    // Network. Open emits nothing; loopback denies all then re-allows the local
    // planes (bus + local HTTP read planes) so caged actors stay on the bus.
    match policy.network {
        NetworkPolicy::Open => {}
        NetworkPolicy::None => arms.push_str("(deny network*)\n"),
        NetworkPolicy::Loopback => arms.push_str(
            // Deny all, then re-allow ONLY the loopback plane, split by
            // operation: OUTBOUND is gated on the REMOTE address (a connect to
            // an external host is denied), while INBOUND/BIND are gated on the
            // LOCAL address (a caged actor can still stand up a loopback
            // listener). A single combined `(allow network* (local ip …)
            // (remote ip …))` does NOT block egress: the `local ip
            // "localhost:*"` filter matches an unbound outbound socket and
            // re-opens ALL external egress — verified live against sandbox-exec
            // on macOS 26 (see seatbelt_network_loopback_blocks_external).
            "(deny network*)\n\
             (allow network-outbound (remote ip \"localhost:*\"))\n\
             (allow network-inbound (local ip \"localhost:*\"))\n\
             (allow network-bind (local ip \"localhost:*\"))\n",
        ),
    }
    // Narrow model-API egress hole (docs/security.md entry 24, network floor).
    // Re-allow ONLY outbound TLS (:443) + the resolver unix socket so a hosted-
    // model client can reach its API and resolve hostnames, while every other
    // port/protocol stays denied by the loopback fence above. Last-match-wins, so
    // these allows sit AFTER `(deny network*)`. Verified live against sandbox-exec
    // on macOS 26 (see seatbelt_https_egress_allows_tls_blocks_other). No-op under
    // Open (nothing was denied to re-open).
    if policy.allow_https_egress && policy.network != NetworkPolicy::Open {
        arms.push_str(
            "(allow network-outbound (remote tcp \"*:443\"))\n\
             (allow network-outbound (remote unix-socket))\n",
        );
    }
    // Read allow-list (experimental): flip to deny-by-default reads. The allow
    // set is the write roots (an agent must read what it writes) + the fixed
    // holes + the caller's fs_read_allow trees. `(literal "/")` is mechanism,
    // not policy: without read on the root inode, path resolution aborts.
    if !policy.fs_read_allow.is_empty() {
        arms.push_str("(deny file-read*)\n(allow file-read*\n  (literal \"/\")\n");
        for r in write_roots {
            arms.push_str(&format!(
                "  (subpath \"{}\")\n",
                sbpl_escape(&r.display().to_string())
            ));
        }
        arms.push_str(
            "  (subpath \"/private/tmp\")\n  (subpath \"/private/var/folders\")\n  (subpath \"/dev\")\n",
        );
        for r in &policy.fs_read_allow {
            arms.push_str(&format!(
                "  (subpath \"{}\")\n",
                sbpl_escape(&r.display().to_string())
            ));
        }
        arms.push_str(")\n");
    }
    // Read deny-list: the listed trees become unreadable, on top of the secrets
    // fence. Emitted AFTER any allow-list so a deny always wins (last-match).
    for p in &policy.fs_read_deny {
        arms.push_str(&format!(
            "(deny file-read* (subpath \"{}\"))\n",
            sbpl_escape(&p.display().to_string())
        ));
    }
    // Re-assert the secrets fence after an allow-list re-opened reads: an allow
    // root that is a parent of the secret store would otherwise re-grant it.
    if !policy.fs_read_allow.is_empty() {
        for p in &protect.deny_all_trees {
            arms.push_str(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                sbpl_escape(&p.display().to_string())
            ));
        }
    }
    arms
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

// ── Cage posture status (M4, docs/handoffs/single-cage-macos.md) ─────────────
//
// The honest surface for "what posture is this cage actually in?", mirroring the
// read-camera two-tier status above. Enforcement availability is the SAME probe
// the cage uses to decide whether to build an SBPL at all — macOS + sandbox-exec
// present. Off macOS every dimension reports UNAVAILABLE: the policy may be
// configured, but nothing enforces it, and that must never read as a silent
// "on". Product words live in `web.rs`; here we carry the machine states.

/// Whether the OS write/read/network enforcement mechanism exists on this
/// platform/build: macOS with `sandbox-exec` present. The single availability
/// probe the cage and the status surface share.
pub fn enforcement_available() -> bool {
    cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists()
}

/// The read dimension's active posture (M4 status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadScope {
    /// Baseline reads open (today's default) — only the secrets fence hides.
    Open,
    /// Deny-list mode: some trees hidden on top of the secrets fence.
    SomeHidden,
    /// Experimental allow-list mode: reads are deny-by-default.
    AllowList,
}

/// What posture this agent's cage is actually in, per dimension (M4). Each
/// dimension is meaningful only when `available` — off macOS the policy is
/// inert and the surface says so rather than implying enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CageStatus {
    /// Is the enforcement mechanism present here (macOS + sandbox-exec)?
    pub available: bool,
    /// Is a cage actually built for this profile (fs_write declared)? When
    /// false, writes are unfenced even where enforcement is available — an
    /// empty grant means "no cage", v1 behavior.
    pub enforcing: bool,
    /// Writes fenced to the declared roots? (= `enforcing`, surfaced per
    /// dimension so the three dimensions read uniformly.)
    pub write_fenced: bool,
    pub read: ReadScope,
    pub network: NetworkPolicy,
}

/// Compute the cage posture from a profile's `[sandbox]` block (M4). Availability
/// is the platform probe; the policy dimensions are read straight off the config
/// the agent shell path would enforce.
pub fn cage_status(cfg: &SandboxCfg) -> CageStatus {
    let enforcing = !cfg.fs_write.is_empty();
    let read = if !cfg.fs_read_allow.is_empty() {
        ReadScope::AllowList
    } else if !cfg.fs_read_deny.is_empty() {
        ReadScope::SomeHidden
    } else {
        ReadScope::Open
    };
    let network = cfg
        .network
        .as_deref()
        .map(NetworkPolicy::parse)
        .unwrap_or_default();
    CageStatus {
        available: enforcement_available(),
        enforcing,
        write_fenced: enforcing,
        read,
        network,
    }
}

impl CageStatus {
    // Product-word posture strings — the ONE place the enum→product-word mapping
    // lives (docs/handoffs/sandbox-config-ui.md M1). Both `/api/status` and
    // `elanus profile get` render through these so the words never drift, and no
    // client re-implements them. Never "SBPL"/"Seatbelt"/"cage"; off macOS every
    // dimension reads "unavailable here" rather than implying a silent "on".
    pub fn write_word(&self) -> &'static str {
        if !self.available {
            "unavailable here"
        } else if self.write_fenced {
            "writes fenced"
        } else {
            "writes open"
        }
    }
    pub fn read_word(&self) -> &'static str {
        if !self.available {
            "unavailable here"
        } else {
            match self.read {
                ReadScope::Open => "reads open",
                ReadScope::SomeHidden => "some folders hidden",
                ReadScope::AllowList => "allow-list",
            }
        }
    }
    pub fn network_word(&self) -> &'static str {
        if !self.available {
            "unavailable here"
        } else {
            match self.network {
                NetworkPolicy::Open => "network open",
                NetworkPolicy::Loopback => "this machine only",
                NetworkPolicy::None => "network off",
            }
        }
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
            policy: CagePolicy::default(),
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
            advisory: TierStatus {
                available: true,
                enabled: true,
            },
            authoritative: TierStatus {
                available: false,
                enabled: false,
            },
        };
        assert!(
            advisory_on.read_flavor_honorable(),
            "advisory on ⇒ honorable"
        );

        // Advisory OFF and authoritative unavailable here (the macOS-off case): NOT
        // honorable ⇒ the broker fast-fails rather than returning empty.
        let both_off = ReadCameraStatus {
            advisory: TierStatus {
                available: true,
                enabled: false,
            },
            authoritative: TierStatus {
                available: false,
                enabled: false,
            },
        };
        assert!(
            !both_off.read_flavor_honorable(),
            "advisory off + authoritative unavailable ⇒ NOT honorable (fast-fail)"
        );

        // Advisory off but authoritative AVAILABLE (a future Linux M2): honorable —
        // the platform can serve reads even with the advisory tier switched off.
        let auth_avail = ReadCameraStatus {
            advisory: TierStatus {
                available: true,
                enabled: false,
            },
            authoritative: TierStatus {
                available: true,
                enabled: false,
            },
        };
        assert!(
            auth_avail.read_flavor_honorable(),
            "authoritative available ⇒ honorable even with advisory off"
        );
    }

    #[test]
    fn cage_status_product_words() {
        // The enum→product-word mapping (sandbox-config-ui M1), tested portably by
        // constructing a CageStatus with enforcement present so the words are the
        // policy words rather than "unavailable here".
        let s = CageStatus {
            available: true,
            enforcing: true,
            write_fenced: true,
            read: ReadScope::Open,
            network: NetworkPolicy::None,
        };
        assert_eq!(s.write_word(), "writes fenced");
        assert_eq!(s.read_word(), "reads open");
        assert_eq!(s.network_word(), "network off");

        let hidden = CageStatus {
            read: ReadScope::SomeHidden,
            network: NetworkPolicy::Loopback,
            write_fenced: false,
            enforcing: false,
            ..s
        };
        assert_eq!(hidden.write_word(), "writes open");
        assert_eq!(hidden.read_word(), "some folders hidden");
        assert_eq!(hidden.network_word(), "this machine only");

        let allow = CageStatus {
            read: ReadScope::AllowList,
            network: NetworkPolicy::Open,
            ..s
        };
        assert_eq!(allow.read_word(), "allow-list");
        assert_eq!(allow.network_word(), "network open");

        // Off-platform: every dimension reads "unavailable here", never a silent on.
        let inert = CageStatus {
            available: false,
            ..s
        };
        assert_eq!(inert.write_word(), "unavailable here");
        assert_eq!(inert.read_word(), "unavailable here");
        assert_eq!(inert.network_word(), "unavailable here");

        // cage_status() picks the right variants off a config.
        let cfg = SandboxCfg {
            network: Some("none".into()),
            ..Default::default()
        };
        assert_eq!(cage_status(&cfg).network, NetworkPolicy::None);
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

    fn test_protect() -> Protect {
        Protect {
            deny_write_files: vec![
                PathBuf::from("/r/elanus.db"),
                PathBuf::from("/r/elanus.db-wal"),
            ],
            deny_write_trees: vec![PathBuf::from("/r/profiles"), PathBuf::from("/r/config")],
            deny_all_trees: vec![PathBuf::from("/r/.secrets")],
        }
    }

    #[test]
    fn sbpl_contains_roots_and_denies_writes() {
        let protect = test_protect();
        let p = sbpl(
            &[PathBuf::from("/tmp/ws")],
            &protect,
            &CagePolicy::default(),
        );
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

    // ── M1: the read + network arms (docs/handoffs/single-cage-macos.md) ──────

    #[test]
    fn sbpl_default_policy_byte_identical() {
        // THE one rule above everything: a default install must behave
        // bit-for-bit as before. The default policy appends no arm, so the SBPL
        // string is identical to the write-only cage. This is the M3 regression
        // in string form; the profile-level version rides SandboxCfg::default.
        let protect = test_protect();
        let roots = [PathBuf::from("/tmp/ws")];
        let today = sbpl(&roots, &protect, &CagePolicy::default());
        // No network rule, no read scoping anywhere in a default profile.
        assert!(!today.contains("network"), "default has no network arm");
        assert!(
            !today.contains("(deny file-read*)"),
            "default never flips reads to deny-by-default"
        );
        // Only the secrets fence carries a file-read deny — nothing else.
        assert_eq!(
            today.matches("(deny file-read*").count(),
            1,
            "default reads = secrets fence only"
        );
    }

    #[test]
    fn sbpl_network_none_denies_all() {
        let protect = test_protect();
        let policy = CagePolicy {
            network: NetworkPolicy::None,
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        assert!(p.contains("(deny network*)"), "{p}");
        assert!(!p.contains("(allow network*"), "none never re-allows: {p}");
        // The network arm is appended AFTER the write cage (last-match-wins).
        let write_at = p.find("(allow file-write*").unwrap();
        let net_at = p.find("(deny network*)").unwrap();
        assert!(net_at > write_at, "network arm after the write cage");
    }

    #[test]
    fn sbpl_network_loopback_denies_then_allows_local() {
        let protect = test_protect();
        let policy = CagePolicy {
            network: NetworkPolicy::Loopback,
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        // Deny all, then re-allow the loopback plane — order matters (allow wins).
        let deny_at = p.find("(deny network*)").unwrap();
        let allow_at = p.find("(allow network-outbound").unwrap();
        assert!(deny_at < allow_at, "deny then allow loopback: {p}");
        // Outbound is gated on the REMOTE address so external egress stays denied;
        // inbound/bind on the LOCAL address so a loopback listener still works. A
        // combined `local+remote` rule would re-open all egress (see the live test
        // seatbelt_network_loopback_blocks_external).
        assert!(
            p.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
            "{p}"
        );
        assert!(
            p.contains("(allow network-inbound (local ip \"localhost:*\"))"),
            "{p}"
        );
        assert!(
            p.contains("(allow network-bind (local ip \"localhost:*\"))"),
            "{p}"
        );
        // The old too-broad combined rule must be gone (it re-opened egress).
        assert!(
            !p.contains("(allow network* (local ip"),
            "combined local+remote allow re-opens external egress: {p}"
        );
    }

    #[test]
    fn sbpl_https_egress_reopens_only_tls_over_loopback() {
        let protect = test_protect();
        let policy = CagePolicy {
            network: NetworkPolicy::Loopback,
            allow_https_egress: true,
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        // The loopback fence is still there (deny all, re-allow local)...
        assert!(p.contains("(deny network*)"), "{p}");
        assert!(
            p.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
            "{p}"
        );
        // ...plus the narrow model-API hole: outbound TLS (:443) + resolver socket.
        assert!(
            p.contains("(allow network-outbound (remote tcp \"*:443\"))"),
            "https egress re-allows outbound :443: {p}"
        );
        assert!(
            p.contains("(allow network-outbound (remote unix-socket))"),
            "https egress re-allows the resolver unix socket for DNS: {p}"
        );
        // The egress arms come AFTER `(deny network*)` (last-match-wins).
        let deny_at = p.find("(deny network*)").unwrap();
        let tls_at = p
            .find("(allow network-outbound (remote tcp \"*:443\"))")
            .unwrap();
        assert!(deny_at < tls_at, "the :443 allow must follow the deny: {p}");
        // No blanket egress re-open: only :443 (not `remote ip "*"` / other ports).
        assert!(
            !p.contains("(allow network-outbound (remote ip \"*"),
            "https egress must NOT open all egress: {p}"
        );
    }

    #[test]
    fn sbpl_https_egress_noop_under_open() {
        // Open already allows everything, so there is nothing to re-open: the flag
        // emits NO :443 arm and the SBPL stays byte-identical to a default cage.
        let protect = test_protect();
        let roots = vec![PathBuf::from("/tmp/ws")];
        let with_flag = CagePolicy {
            network: NetworkPolicy::Open,
            allow_https_egress: true,
            ..Default::default()
        };
        let p = sbpl(&roots, &protect, &with_flag);
        assert!(!p.contains("*:443"), "no :443 arm under Open: {p}");
        assert_eq!(
            p,
            sbpl(&roots, &protect, &CagePolicy::default()),
            "https-egress under Open must not perturb the SBPL"
        );
    }

    #[test]
    fn sbpl_read_deny_list_adds_denies_keeps_reads_open() {
        let protect = test_protect();
        let policy = CagePolicy {
            fs_read_deny: vec![PathBuf::from("/home/.ssh"), PathBuf::from("/home/.aws")],
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        // Deny-list mode NEVER flips to deny-by-default: baseline reads stay open.
        assert!(
            !p.contains("(deny file-read*)\n"),
            "no blanket read deny: {p}"
        );
        assert!(
            p.contains("(deny file-read* (subpath \"/home/.ssh\"))"),
            "{p}"
        );
        assert!(
            p.contains("(deny file-read* (subpath \"/home/.aws\"))"),
            "{p}"
        );
        // The secrets fence survives alongside the new denies.
        assert!(
            p.contains("(deny file-read* (subpath \"/r/.secrets\"))"),
            "{p}"
        );
    }

    #[test]
    fn sbpl_read_allow_list_flips_and_keeps_holes_and_secrets() {
        let protect = test_protect();
        let policy = CagePolicy {
            fs_read_allow: vec![PathBuf::from("/usr"), PathBuf::from("/System")],
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        // Allow-list mode flips reads to deny-by-default.
        assert!(
            p.contains("(deny file-read*)\n"),
            "flips to deny-by-default: {p}"
        );
        // `(literal "/")` is required for root-path traversal (M1 spike).
        assert!(p.contains("(allow file-read*\n  (literal \"/\")"), "{p}");
        // The write roots are readable (an agent must read what it writes).
        let allow_read_at = p.find("(allow file-read*").unwrap();
        let ws_read = p[allow_read_at..].find("(subpath \"/tmp/ws\")");
        assert!(ws_read.is_some(), "write root is read-allowed: {p}");
        // The always-needed holes mirror the write side.
        assert!(
            p[allow_read_at..].contains("(subpath \"/private/tmp\")"),
            "{p}"
        );
        assert!(p[allow_read_at..].contains("(subpath \"/dev\")"), "{p}");
        // The caller's allow trees are present.
        assert!(p[allow_read_at..].contains("(subpath \"/usr\")"), "{p}");
        // The secrets fence is RE-ASSERTED after the allow-list so it still wins
        // even if an allow root is a parent of the secret store (last-match).
        let deny_flip = p.find("(deny file-read*)\n").unwrap();
        let last_secret_deny = p
            .rfind("(deny file-read* (subpath \"/r/.secrets\"))")
            .unwrap();
        assert!(
            last_secret_deny > deny_flip && last_secret_deny > allow_read_at,
            "secrets fence re-asserted after the allow-list: {p}"
        );
    }

    #[test]
    fn sbpl_combined_policy_orders_denies_last() {
        // Network none + allow-list reads + a deny-list entry together: the
        // deny-list entry and the secrets fence must both win over the
        // allow-list (last-match), and the network arm is present.
        let protect = test_protect();
        let policy = CagePolicy {
            network: NetworkPolicy::None,
            fs_read_deny: vec![PathBuf::from("/tmp/ws/private")],
            fs_read_allow: vec![PathBuf::from("/usr")],
            ..Default::default()
        };
        let p = sbpl(&[PathBuf::from("/tmp/ws")], &protect, &policy);
        assert!(p.contains("(deny network*)"), "{p}");
        let allow_read_at = p.find("(allow file-read*").unwrap();
        let deny_entry_at = p
            .find("(deny file-read* (subpath \"/tmp/ws/private\"))")
            .unwrap();
        assert!(
            deny_entry_at > allow_read_at,
            "deny-list wins over allow-list: {p}"
        );
    }

    #[test]
    fn network_policy_parse() {
        assert_eq!(NetworkPolicy::parse("open"), NetworkPolicy::Open);
        assert_eq!(NetworkPolicy::parse("loopback"), NetworkPolicy::Loopback);
        assert_eq!(NetworkPolicy::parse("none"), NetworkPolicy::None);
        assert_eq!(NetworkPolicy::parse("NONE"), NetworkPolicy::None);
        // Unknown falls back to open (never silently over-restrict).
        assert_eq!(NetworkPolicy::parse("garbage"), NetworkPolicy::Open);
    }

    #[test]
    fn cage_status_reports_each_dimension() {
        // A default profile: no cage (no fs_write), reads open, network open.
        let base = cage_status(&SandboxCfg::default());
        assert!(!base.enforcing, "no fs_write ⇒ no cage");
        assert!(!base.write_fenced);
        assert_eq!(base.read, ReadScope::Open);
        assert_eq!(base.network, NetworkPolicy::Open);

        // A profile that opts into every dimension.
        let s = cage_status(&SandboxCfg {
            fs_write: vec!["/tmp/ws".into()],
            network: Some("loopback".into()),
            fs_read_deny: vec!["/home/.ssh".into()],
            fs_read_allow: vec![],
            ..Default::default()
        });
        assert!(s.enforcing && s.write_fenced, "fs_write ⇒ writes fenced");
        assert_eq!(s.read, ReadScope::SomeHidden, "deny-list ⇒ some hidden");
        assert_eq!(s.network, NetworkPolicy::Loopback);

        // Allow-list wins over deny-list for the read dimension label.
        let allow = cage_status(&SandboxCfg {
            fs_read_allow: vec!["/usr".into()],
            fs_read_deny: vec!["/home/.ssh".into()],
            ..Default::default()
        });
        assert_eq!(allow.read, ReadScope::AllowList);

        // Availability is the platform probe: on macOS with sandbox-exec it is
        // true; OFF macOS every enforcement dimension is UNAVAILABLE — the policy
        // is inert and the surface must say so, never a silent "on".
        assert_eq!(base.available, enforcement_available());
        #[cfg(not(target_os = "macos"))]
        assert!(
            !base.available,
            "off macOS the cage enforcement mechanism is unavailable here"
        );
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

        // bus.toml carries the platform trust level (M2, platform-trust.md):
        // fenced even though it sits in the write root — a caged actor may read
        // it (the platform stage does) but never write it, so it cannot raise
        // its own trust. Mirrors the ledger case above.
        let bus = root.bus_file();
        std::fs::write(&bus, "trust = \"full\"\n").unwrap(); // the human/kernel writes it uncaged
        let bus_write = cage
            .shell_command(&format!("echo x >> {}", bus.display()))
            .output()
            .unwrap();
        assert!(
            !bus_write.status.success(),
            "writing bus.toml from the cage must fail (it holds the trust level)"
        );
        let bus_read = cage
            .shell_command(&format!("cat {} > /dev/null", bus.display()))
            .output()
            .unwrap();
        assert!(
            bus_read.status.success(),
            "reading bus.toml from the cage must succeed"
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

    // ── M2: live proof the read + network arms cage (and do not over-cage) ────
    //
    // The live test is the arbiter (handoff wonky bit 4), not the string tests.
    // Each function is macOS + `sandbox-exec`-gated exactly like
    // seatbelt_actually_cages: skipped (not failed) where the mechanism is
    // absent. The SBPL grammar these exercise was first verified against a real
    // sandbox-exec in the M1 spike.

    #[cfg(target_os = "macos")]
    fn have_sandbox_exec() -> bool {
        Path::new("/usr/bin/sandbox-exec").exists()
    }

    /// Bind a loopback listener that accepts connections and writes one line,
    /// so a caged `nc` either connects (loopback/open) or is blocked (none).
    /// Returns the bound port; the accept loop runs on a detached thread until
    /// the process exits. Only 127.0.0.1 is touched — no external network.
    #[cfg(target_os = "macos")]
    fn spawn_loopback_listener() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if let Ok(mut s) = conn {
                    use std::io::Write;
                    let _ = s.write_all(b"HELLO\n");
                }
            }
        });
        port
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_network_none_blocks_loopback() {
        if !have_sandbox_exec() {
            return;
        }
        let root = tmp_root();
        let port = spawn_loopback_listener();
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            network: Some("none".into()),
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        // A caged connect to the loopback listener must FAIL under network=none.
        let out = cage
            .shell_command(&format!("nc -w 2 127.0.0.1 {port}"))
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "network=none must block a loopback connect: {out:?}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_network_loopback_allows_local() {
        if !have_sandbox_exec() {
            return;
        }
        let root = tmp_root();
        let port = spawn_loopback_listener();
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            network: Some("loopback".into()),
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        // The SAME local request SUCCEEDS under network=loopback — the bus and
        // local read planes stay reachable. Asserts loopback only; no external
        // network is touched, so the test does not depend on internet access.
        let out = cage
            .shell_command(&format!("nc -w 2 127.0.0.1 {port}"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "network=loopback must allow a 127.0.0.1 connect: {out:?}"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("HELLO"),
            "loopback connect must read the listener's line: {out:?}"
        );
    }

    /// The egress half of the loopback fence: a caged connect to an EXTERNAL
    /// (non-loopback) address must FAIL while loopback stays reachable. This is
    /// the property the codex cage leans on (docs/handoffs/codex-cage.md M2) and
    /// the one the original combined `(local ip …)(remote ip …)` loopback rule
    /// silently did NOT provide — that rule re-opened all egress. A self-hosted
    /// listener cannot stand in for "external" (macOS loops a connect to the
    /// machine's own IP back internally), so this needs a genuine external host;
    /// it SKIPS (never fails) when sandbox-exec is absent or the host has no
    /// outbound route, so it is not internet-flaky.
    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_network_loopback_blocks_external() {
        if !have_sandbox_exec() {
            return;
        }
        // Baseline: is an external host reachable at all right now? If not (no
        // internet), skip honestly rather than assert a block the network gives
        // for free. 1.1.1.1:80 is a stable, always-on anycast endpoint.
        let reachable = std::process::Command::new("nc")
            .args(["-w", "3", "-z", "1.1.1.1", "80"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !reachable {
            return;
        }
        let root = tmp_root();
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            network: Some("loopback".into()),
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        // The SAME connect that succeeds uncaged must FAIL under network=loopback:
        // outbound is gated on the remote address, and 1.1.1.1 is not loopback.
        let out = cage
            .shell_command("nc -w 3 -z 1.1.1.1 80")
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "network=loopback must BLOCK an external connect (egress control): {out:?}"
        );
    }

    /// Live (macOS): the model-API egress hole (`allow_https_egress`) re-opens
    /// outbound TLS (:443) through the loopback fence — so a hosted-model client
    /// like codex can reach its API — while a NON-443 external port stays blocked.
    /// This is the leg that lets a headless codex turn actually complete under the
    /// cage. Skipped honestly with no internet / no sandbox-exec.
    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_https_egress_allows_tls_blocks_other() {
        if !have_sandbox_exec() {
            return;
        }
        // Baseline: is external TLS reachable right now? If not, skip (don't assert
        // an allow the network can't demonstrate). chatgpt.com is codex's own API
        // host; a plain unauthenticated GET returns quickly (403) but PROVES reach.
        let reachable = std::process::Command::new("nc")
            .args(["-w", "3", "-z", "1.1.1.1", "443"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !reachable {
            return;
        }
        let root = tmp_root();
        // A loopback cage WITH the model-API egress hole (what codex_headless_cage
        // builds): write = the root, network = loopback, plus allow_https_egress.
        let cage = Cage::from_roots_with_policy(
            vec![root.dir.clone()],
            vec![],
            true,
            &Protect::for_root(&root),
            CagePolicy {
                network: NetworkPolicy::Loopback,
                allow_https_egress: true,
                ..Default::default()
            },
        );
        assert!(cage.enforcing());
        // (a) outbound :443 to an external host now SUCCEEDS (the model-API reach).
        let tls = cage
            .shell_command("curl -sS -m 12 -o /dev/null -w '%{http_code}' https://chatgpt.com/")
            .output()
            .unwrap();
        let code = String::from_utf8_lossy(&tls.stdout);
        assert!(
            tls.status.success() && code.trim() != "000",
            "allow_https_egress must let outbound :443 reach the model API (got {code:?}): {tls:?}"
        );
        // (b) a NON-443 external port stays BLOCKED — the hole is TLS-only, not
        // blanket egress.
        let other = cage
            .shell_command("nc -w 3 -z 1.1.1.1 80")
            .output()
            .unwrap();
        assert!(
            !other.status.success(),
            "allow_https_egress must NOT open non-443 egress: {other:?}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_read_deny_hides_tree() {
        if !have_sandbox_exec() {
            return;
        }
        let root = tmp_root();
        // A scratch tree to hide, and a sibling file left readable. Both live
        // inside the write root (reads stay open by default; only the deny tree
        // is hidden, on top of the secrets fence).
        let hidden = root.dir.join("hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        let secret_file = hidden.join("f.txt");
        std::fs::write(&secret_file, "classified").unwrap();
        let open_file = root.dir.join("open.txt");
        std::fs::write(&open_file, "public").unwrap();
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            fs_read_deny: vec![hidden.display().to_string()],
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        // A caged read INSIDE the deny tree fails...
        let denied = cage
            .shell_command(&format!("cat {}", secret_file.display()))
            .output()
            .unwrap();
        assert!(
            !denied.status.success(),
            "reading a fs_read_deny tree from the cage must fail: {denied:?}"
        );
        // ...a caged read OUTSIDE it (still open) succeeds.
        let ok = cage
            .shell_command(&format!("cat {} > /dev/null", open_file.display()))
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "reading outside the deny tree must still succeed: {ok:?}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn seatbelt_read_allow_list_still_runs_shell() {
        if !have_sandbox_exec() {
            return;
        }
        let root = tmp_root();
        let inside = root.dir.join("f.txt");
        std::fs::write(&inside, "inside").unwrap();
        // The anti-catastrophe case: allow-list mode flips reads to
        // deny-by-default. Whoever sets it owns the baseline, so the test
        // supplies a sane interpreter baseline (verified sufficient for a shell
        // in the M1 spike). A caged `sh -c 'echo hi'` must STILL run and read
        // its allow roots — a too-tight list would break every spawn.
        let cfg = SandboxCfg {
            fs_write: vec![root.dir.display().to_string()],
            capture_exclude: vec![],
            workdir: None,
            fs_read_allow: vec![
                "/usr".into(),
                "/bin".into(),
                "/System".into(),
                "/Library".into(),
            ],
            ..Default::default()
        };
        let cage = Cage::from_profile(&root, &cfg);
        assert!(cage.enforcing());
        let hi = cage.shell_command("echo hi").output().unwrap();
        assert!(
            hi.status.success() && String::from_utf8_lossy(&hi.stdout).contains("hi"),
            "allow-list mode must still run a shell: {hi:?}"
        );
        // The write root is an allow root — an agent must read what it writes.
        let read_own = cage
            .shell_command(&format!("cat {} > /dev/null", inside.display()))
            .output()
            .unwrap();
        assert!(
            read_own.status.success(),
            "allow-list mode must let the agent read its own write root: {read_own:?}"
        );
    }
}
