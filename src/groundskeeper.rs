//! The KB groundskeeper (docs/handoffs/kb-groundskeeper.md). Two rungs on the
//! variety ladder over the knowledge base that `kb-core` and `kb-search` built:
//!
//!   - **The script rung (M1), no LLM.** A sweep that validates every pointer
//!     block's `meta.{path,lines,sha}` (kb-core M3), finds orphan `kb/` files, and
//!     flags staleness (a file changed since the recorded sha), then mails the
//!     owner a report. This ships first and works with ZERO LLM configuration.
//!   - **The setup gate (M2).** The diff pipeline is lanius's first auto-approve
//!     pipeline, so it is absolutely setup-gated: nothing runs — no cron fire, no
//!     LLM call — until the human has set the two model choices (informed by the
//!     llm-strengths KB), a cadence, and a token budget, AND approved the package.
//!   - **The diff pipeline (M3).** A cheap compactor drafts unified diffs; an
//!     expensive ratifier applies-or-bounces them WITH feedback. This module owns
//!     the kernel-side, deterministic primitives — the setup gate and the spawn
//!     dispatch shape (model + budget threaded); the agents' own reasoning (what to
//!     consolidate, whether to bounce) is their live work. Per-pass cost is carried
//!     by the general `llm_usage` substrate (each spawned run records its tokens),
//!     and bounces are recorded by the ratifier via `lanius block append` — neither
//!     needs a bespoke kernel helper here.
//!
//! There is no groundskeeper data model: the report reads pointer blocks
//! (`context_blocks`) and the corpus on disk; the gate reads package config
//! (`config_repo`) and the grants ledger; the pipeline rides `spawn_core` and the
//! kb write path. Everything composes from substrates that already ship (D6).

use crate::agentcli::SpawnRequest;
use crate::kb;
use crate::packages;
use crate::paths::Root;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The package name (also its config/topic/grant key).
pub const PKG: &str = "kb-groundskeeper";
/// The compactor and ratifier profiles the pipeline spawns.
pub const COMPACTOR_PROFILE: &str = "kb-compactor";
pub const RATIFIER_PROFILE: &str = "kb-ratifier";
/// The agent-scope memory block on the compactor that carries ratifier bounces
/// (docs/handoffs/kb-groundskeeper.md M3): the feedback the next pass learns from.
pub const BOUNCE_BLOCK: &str = "ratifier-bounces";

// ─────────────────────────── M1: the script checker ───────────────────────────

/// One pointer block's verdict against the file it points into.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PointerStatus {
    /// The pointer resolves: file present, lines in range, sha matches.
    Ok,
    /// The referenced file does not exist (or is not a plain file).
    MissingPath,
    /// The file exists but the `lines` range falls outside it.
    BadLines,
    /// The file exists but its content sha no longer matches (stale — the file
    /// changed since the pointer recorded it).
    StaleSha,
    /// The pointer meta is malformed (missing kb/path/sha, unresolvable package).
    Malformed { reason: String },
}

/// One reported pointer issue (a block + where it points + what is wrong).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PointerIssue {
    pub owner: String,
    pub block: String,
    pub kb: String,
    pub path: String,
    pub lines: String,
    pub status: PointerStatus,
}

/// One orphan `kb/` file — a file no pointer block references, within a KB that IS
/// pointed at by at least one pointer (informational; M1(b)). A KB reached only by
/// search carries no pointers and so contributes no orphans (it is not neglect).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanFile {
    pub kb: String,
    pub path: String,
}

/// One KB-format finding over a `kb/` file (docs/handoffs/kb-formalization.md M2).
/// All WARN-level — a finding in the sweep, never a hard error or a write block.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "finding")]
pub enum FormatKind {
    /// The file has no `---` frontmatter block at all.
    MissingFrontmatter,
    /// The frontmatter is present but a required field (`title`/`description`) is
    /// missing or empty.
    MissingField { field: String },
    /// A disallowed internal-reference link: an absolute path or a reference-style
    /// `[text][id]` link.
    BadLink {
        target: String,
        reason: String,
        line: usize,
    },
    /// A relative internal link that does not resolve inside the package's own tree
    /// — either missing on disk OR escaping the package (one class, wonky bit 4).
    DeadLink { target: String, line: usize },
}

/// One reported KB-format finding: the KB, the repo-relative `kb/` path, the kind.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FormatFinding {
    pub kb: String,
    pub path: String,
    pub kind: FormatKind,
}

/// The owner-facing sweep report (M1). Zero LLM calls produce it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Report {
    /// Pointers whose target file is missing or whose line range is out of range.
    pub broken: Vec<PointerIssue>,
    /// Pointers whose target changed since the recorded sha (staleness).
    pub stale: Vec<PointerIssue>,
    /// `kb/` files no pointer references, within a pointed-at KB (informational).
    pub orphans: Vec<OrphanFile>,
    /// KB-entry format findings (frontmatter/link contract, formalization M2).
    pub format: Vec<FormatFinding>,
    /// How many pointer blocks were checked.
    pub checked_pointers: usize,
    /// How many `kb/` files were enumerated across the pointed-at KBs.
    pub checked_files: usize,
}

impl Report {
    /// Whether the sweep found anything worth mailing the owner about.
    pub fn has_findings(&self) -> bool {
        !self.broken.is_empty()
            || !self.stale.is_empty()
            || !self.orphans.is_empty()
            || !self.format.is_empty()
    }
}

/// Check one KB file's text against the format contract (frontmatter present, both
/// required fields, and every internal link legitimate + resolving inside the
/// package). Pure over its inputs — the text, the file's own directory, and the
/// package root the links must resolve within — so it is directly unit-testable and
/// makes ZERO LLM calls. `package_root` is the portable-unit boundary (wonky bit 4).
pub fn check_kb_format(text: &str, kb_file_dir: &Path, package_root: &Path) -> Vec<FormatKind> {
    let parsed = kb::parse_kb_entry(text);
    let mut out = Vec::new();
    match &parsed.frontmatter {
        None => out.push(FormatKind::MissingFrontmatter),
        Some(fm) => {
            if fm.title.as_deref().unwrap_or("").trim().is_empty() {
                out.push(FormatKind::MissingField {
                    field: "title".into(),
                });
            }
            if fm.description.as_deref().unwrap_or("").trim().is_empty() {
                out.push(FormatKind::MissingField {
                    field: "description".into(),
                });
            }
        }
    }
    for link in &parsed.links {
        if link.reference_style {
            out.push(FormatKind::BadLink {
                target: link.target.clone(),
                reason: "reference-style link".into(),
                line: link.line,
            });
            continue;
        }
        match &link.class {
            kb::LinkClass::Absolute => out.push(FormatKind::BadLink {
                target: link.target.clone(),
                reason: "absolute path".into(),
                line: link.line,
            }),
            kb::LinkClass::Relative { .. } => {
                if !kb::resolve_relative(kb_file_dir, package_root, &link.target).resolves {
                    out.push(FormatKind::DeadLink {
                        target: link.target.clone(),
                        line: link.line,
                    });
                }
            }
            // A real external URL and an in-page anchor are both fine.
            kb::LinkClass::External | kb::LinkClass::Anchor => {}
        }
    }
    out
}

/// A raw pointer block read from the store: its owner, name, and parsed meta.
struct Pointer {
    owner: String,
    block: String,
    kb: String,
    path: String,
    lines: String,
    sha: String,
}

/// Read every pointer block from `context_blocks` — a block whose `meta` carries a
/// `"kb"` string (kb-core M3). Malformed metas (a `kb` key but missing path/sha)
/// are surfaced as `Malformed` issues rather than silently dropped.
fn read_pointers(conn: &Connection) -> Result<(Vec<Pointer>, Vec<PointerIssue>)> {
    let mut stmt = conn.prepare("SELECT owner, name, meta FROM context_blocks")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut pointers = Vec::new();
    let mut malformed = Vec::new();
    for row in rows {
        let (owner, block, meta_raw) = row?;
        let Some(meta_raw) = meta_raw else { continue };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_raw) else {
            continue;
        };
        let Some(kb) = meta.get("kb").and_then(|v| v.as_str()) else {
            continue; // not a pointer block
        };
        let path = meta.get("path").and_then(|v| v.as_str());
        let sha = meta.get("sha").and_then(|v| v.as_str());
        let lines = meta
            .get("lines")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match (path, sha) {
            (Some(path), Some(sha)) => pointers.push(Pointer {
                owner,
                block,
                kb: kb.to_string(),
                path: path.to_string(),
                lines,
                sha: sha.to_string(),
            }),
            _ => malformed.push(PointerIssue {
                owner,
                block,
                kb: kb.to_string(),
                path: path.unwrap_or("").to_string(),
                lines,
                status: PointerStatus::Malformed {
                    reason: "pointer meta is missing 'path' or 'sha'".into(),
                },
            }),
        }
    }
    Ok((pointers, malformed))
}

/// Classify one pointer against the corpus on disk. Pure over its inputs (the
/// resolved kb-file path + the recorded lines/sha), so it is directly unit-tested.
pub fn classify_pointer(target: &Path, lines: &str, sha: &str) -> PointerStatus {
    // symlink_metadata: a KB tree holds agent-written content; never follow a link.
    let md = match std::fs::symlink_metadata(target) {
        Ok(md) if md.file_type().is_file() => md,
        _ => return PointerStatus::MissingPath,
    };
    let _ = md;
    let bytes = match std::fs::read(target) {
        Ok(b) => b,
        Err(_) => return PointerStatus::MissingPath,
    };
    if !lines_in_range(lines, &bytes) {
        return PointerStatus::BadLines;
    }
    if crate::context_blocks::sha256_hex(&bytes) != sha {
        return PointerStatus::StaleSha;
    }
    PointerStatus::Ok
}

/// Whether a `"lo-hi"` (or `"n"`) range lies within a file's line count. An empty
/// or unparseable range is treated as in-range (the pointer carried no line span).
fn lines_in_range(lines: &str, bytes: &[u8]) -> bool {
    let lines = lines.trim();
    if lines.is_empty() {
        return true;
    }
    let count = String::from_utf8_lossy(bytes).lines().count().max(1);
    let (lo, hi) = match lines.split_once('-') {
        Some((a, b)) => (a.trim().parse::<usize>(), b.trim().parse::<usize>()),
        None => (lines.parse::<usize>(), lines.parse::<usize>()),
    };
    match (lo, hi) {
        (Ok(lo), Ok(hi)) => lo >= 1 && hi >= lo && hi <= count + 1,
        _ => true, // unparseable → not our job to flag here
    }
}

/// Run the M1 sweep: validate pointer blocks, find orphans, flag staleness. No
/// LLM. Reads the store (`conn`) and the corpus on disk (via `root`, `profile`).
pub fn sweep(root: &Root, conn: &Connection, profile: &str) -> Result<Report> {
    let (pointers, malformed) = read_pointers(conn)?;
    let mut report = Report {
        checked_pointers: pointers.len() + malformed.len(),
        ..Default::default()
    };
    // Malformed pointers count as broken.
    report.broken.extend(malformed);

    // Which packages have at least one pointer (the KBs orphan-detection applies
    // to), and which (kb, repo-relative path) pairs are referenced.
    let mut pointed_kbs: BTreeSet<String> = BTreeSet::new();
    let mut referenced: BTreeSet<(String, String)> = BTreeSet::new();

    for p in &pointers {
        pointed_kbs.insert(p.kb.clone());
        // Resolve the package's kb/ dir; the meta.path already includes the kb/
        // prefix (e.g. "kb/role-verifier.md"), relative to the package root.
        let pkg = match packages::find(root, &p.kb) {
            Ok(pkg) => pkg,
            Err(e) => {
                report.broken.push(PointerIssue {
                    owner: p.owner.clone(),
                    block: p.block.clone(),
                    kb: p.kb.clone(),
                    path: p.path.clone(),
                    lines: p.lines.clone(),
                    status: PointerStatus::Malformed {
                        reason: format!("unresolvable kb package: {e}"),
                    },
                });
                continue;
            }
        };
        referenced.insert((p.kb.clone(), normalize_rel(&p.path)));
        let target = pkg.dir.join(&p.path);
        let status = classify_pointer(&target, &p.lines, &p.sha);
        let issue = PointerIssue {
            owner: p.owner.clone(),
            block: p.block.clone(),
            kb: p.kb.clone(),
            path: p.path.clone(),
            lines: p.lines.clone(),
            status: status.clone(),
        };
        match status {
            PointerStatus::Ok => {}
            PointerStatus::StaleSha => report.stale.push(issue),
            _ => report.broken.push(issue),
        }
    }

    // Orphans: enumerate the files of every pointed-at KB and report those no
    // pointer references. A KB with no pointer at all contributes nothing.
    for kbi in kb::enumerate(root, profile)? {
        if !pointed_kbs.contains(&kbi.package) {
            continue;
        }
        for rel in list_kb_files(&kbi.path) {
            report.checked_files += 1;
            let repo_rel = format!("{}/{}", kb::KB_DIR, rel);
            if !referenced.contains(&(kbi.package.clone(), repo_rel.clone())) {
                report.orphans.push(OrphanFile {
                    kb: kbi.package.clone(),
                    path: repo_rel,
                });
            }
        }
    }

    // Format sweep (docs/handoffs/kb-formalization.md M2): validate the frontmatter
    // + link contract over EVERY file in EVERY enabled [kb] package (not only the
    // pointed-at ones — the format applies to the whole corpus). Read-only; no LLM.
    for kbi in kb::enumerate(root, profile)? {
        // The package root is the parent of the `kb/` dir — the portable-unit tree a
        // relative link must resolve inside (wonky bit 4).
        let package_root = kbi.path.parent().unwrap_or(&kbi.path).to_path_buf();
        for rel in list_kb_files(&kbi.path) {
            let file = kbi.path.join(&rel);
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let kb_file_dir = file.parent().unwrap_or(&kbi.path).to_path_buf();
            let repo_rel = format!("{}/{}", kb::KB_DIR, rel);
            for kind in check_kb_format(&text, &kb_file_dir, &package_root) {
                report.format.push(FormatFinding {
                    kb: kbi.package.clone(),
                    path: repo_rel.clone(),
                    kind,
                });
            }
        }
    }
    Ok(report)
}

/// Normalize a pointer's repo-relative path for set comparison (trim, forward
/// slashes). The path already carries the `kb/` prefix.
fn normalize_rel(path: &str) -> String {
    path.trim().replace('\\', "/")
}

/// List the regular files under a `kb/` tree, as forward-slash paths RELATIVE to
/// that tree (so `kb/role-verifier.md`'s rel is `role-verifier.md`). Never follows
/// a symlink. A missing tree yields nothing.
fn list_kb_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(dir, &PathBuf::new(), &mut out);
    out.sort();
    out
}

fn walk(base: &Path, rel: &Path, out: &mut Vec<String>) {
    let here = base.join(rel);
    let Ok(entries) = std::fs::read_dir(&here) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let Ok(ft) = e.file_type() else { continue };
        let child = rel.join(e.file_name());
        if ft.is_dir() {
            walk(base, &child, out);
        } else if ft.is_file() {
            out.push(child.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// A one-line-per-finding human summary of a sweep report (the owner-report body,
/// M1). Deterministic and pure over the report, so it is what both the CLI prints
/// and the mailed payload carries.
pub fn report_summary(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "kb groundskeeper: {} pointer(s), {} file(s) checked — \
         {} broken, {} stale, {} orphan, {} format\n",
        report.checked_pointers,
        report.checked_files,
        report.broken.len(),
        report.stale.len(),
        report.orphans.len(),
        report.format.len(),
    ));
    for i in &report.broken {
        out.push_str(&format!(
            "  BROKEN  {}/{} (block {} owned by {}): {}\n",
            i.kb,
            i.path,
            i.block,
            i.owner,
            status_word(&i.status),
        ));
    }
    for i in &report.stale {
        out.push_str(&format!(
            "  STALE   {}/{} (block {} owned by {}): file changed since recorded sha\n",
            i.kb, i.path, i.block, i.owner,
        ));
    }
    for o in &report.orphans {
        out.push_str(&format!(
            "  ORPHAN  {}/{} (no pointer references it)\n",
            o.kb, o.path
        ));
    }
    for f in &report.format {
        out.push_str(&format!("  FORMAT  {}/{}: {}\n", f.kb, f.path, format_word(&f.kind)));
    }
    out
}

/// A one-line human reason for a KB-format finding.
fn format_word(k: &FormatKind) -> String {
    match k {
        FormatKind::MissingFrontmatter => "no frontmatter block (--- title/description)".into(),
        FormatKind::MissingField { field } => format!("frontmatter is missing '{field}'"),
        FormatKind::BadLink {
            target,
            reason,
            line,
        } => format!("bad link ({reason}) at line {line}: {target}"),
        FormatKind::DeadLink { target, line } => {
            format!("dead link (does not resolve inside the package) at line {line}: {target}")
        }
    }
}

fn status_word(s: &PointerStatus) -> &'static str {
    match s {
        PointerStatus::Ok => "ok",
        PointerStatus::MissingPath => "referenced file is missing",
        PointerStatus::BadLines => "line range is outside the file",
        PointerStatus::StaleSha => "file changed since recorded sha",
        PointerStatus::Malformed { .. } => "pointer meta is malformed",
    }
}

/// A brief corpus digest (KB names + their `kb/` file paths) for the compactor's
/// prompt — enough to orient the sweep; the compactor digs with `search_knowledge`.
pub fn corpus_digest(root: &Root, profile: &str) -> Result<String> {
    let mut out = String::new();
    for kbi in kb::enumerate(root, profile)? {
        out.push_str(&format!(
            "- {} ({} file{}):\n",
            kbi.package,
            kbi.files,
            if kbi.files == 1 { "" } else { "s" }
        ));
        for rel in list_kb_files(&kbi.path) {
            out.push_str(&format!("    {}/{}\n", kb::KB_DIR, rel));
        }
    }
    Ok(out)
}

// ─────────────────────────── M2: the setup gate ───────────────────────────

/// The concrete pipeline configuration (docs/handoffs/kb-groundskeeper.md M2), the
/// human's committed decision (wonky bit 2): which cheap/expensive model, cadence,
/// per-pass token budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineConfig {
    pub compactor_model: String,
    pub ratifier_model: String,
    pub cadence: String,
    pub token_budget: i64,
}

/// The config keys the package declares `required` (its manifest `[config] keys`).
/// The setup gate treats these as the ones that must all carry a value. Falls back
/// to the canonical four if the manifest is unreadable (defense in depth).
pub fn required_keys(root: &Root) -> Vec<String> {
    let declared = packages::find(root, PKG)
        .ok()
        .and_then(|p| p.manifest)
        .map(|lm| {
            lm.manifest
                .config
                .keys
                .iter()
                .filter(|k| k.required)
                .map(|k| k.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if declared.is_empty() {
        return vec![
            "compactor_model".into(),
            "ratifier_model".into(),
            "cadence".into(),
            "token_budget".into(),
        ];
    }
    declared
}

/// Load the persisted pipeline config from the config repo, or `Err(reason)` if any
/// required key is unset — the setup gate's read side. `Ok(None)`-style inertness
/// is expressed as the `Err(reason)` arm so the caller can log WHY it stayed inert.
pub fn load_config(root: &Root) -> Result<std::result::Result<PipelineConfig, String>> {
    // A required key is missing → inert, with the reason.
    for key in required_keys(root) {
        if crate::config_repo::get_key(root, PKG, &key)?.is_none() {
            return Ok(Err(format!(
                "kb-groundskeeper is not set up: {PKG}.{key} is unset \
                 (run `lanius config set {PKG}.{key} ...`)"
            )));
        }
    }
    let compactor_model = cfg_str(root, "compactor_model")?;
    let ratifier_model = cfg_str(root, "ratifier_model")?;
    let cadence = cfg_str(root, "cadence")?;
    let token_budget = crate::config_repo::get_key(root, PKG, "token_budget")?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .ok_or_else(|| anyhow::anyhow!("{PKG}.token_budget is not an integer"))?;
    Ok(Ok(PipelineConfig {
        compactor_model,
        ratifier_model,
        cadence,
        token_budget,
    }))
}

/// Read a string config value, unquoting the TOML fragment `get_key` returns
/// (e.g. `"claude-fable-5"` → `claude-fable-5`).
fn cfg_str(root: &Root, key: &str) -> Result<String> {
    let raw = crate::config_repo::get_key(root, PKG, key)?
        .ok_or_else(|| anyhow::anyhow!("{PKG}.{key} is unset"))?;
    Ok(raw.trim().trim_matches('"').to_string())
}

/// The absolute setup gate (docs/handoffs/kb-groundskeeper.md M2): the pipeline is
/// live ONLY when every required config key is set AND the package is approved.
/// Nothing — no cron fire, no compactor/ratifier spawn — happens before both.
pub fn is_setup_complete(root: &Root, conn: &Connection) -> bool {
    if !matches!(load_config(root), Ok(Ok(_))) {
        return false;
    }
    packages::is_granted(conn, PKG).unwrap_or(false)
}

/// The exec-handler package that makes the pipeline's agent mailboxes
/// daemon-drivable (docs/handoffs/kb-groundskeeper.md M3): it subscribes to
/// in/agent/kb-compactor + in/agent/kb-ratifier and turns each into an
/// `lanius handle-exec` run. Ships in core, approved as part of setup.
pub const HANDLER_PKG: &str = "kb-pipeline";

/// Is there an approved exec handler for the compactor and ratifier mailboxes?
/// `spawn_core` (src/agentcli.rs) refuses to launch a profile whose agent mailbox
/// has no approved exec package subscribing to it, so the pipeline cannot spawn a
/// thing without one. Returns `Some(reason)` naming the gap (an inert condition to
/// print, not an error) when either mailbox is undriveable, `None` when both are
/// covered. Assumes `packages::sync` already ran on `conn`.
pub fn handler_gap(root: &Root, conn: &Connection) -> Result<Option<String>> {
    for profile in [COMPACTOR_PROFILE, RATIFIER_PROFILE] {
        let mailbox = crate::topic::agent_mailbox(profile);
        if packages::matching_exec_handlers(root, conn, &mailbox)?.is_empty() {
            return Ok(Some(format!(
                "the pipeline exec handler {HANDLER_PKG} is not approved — no approved \
                 exec package subscribes to {mailbox} (run `lanius approve {HANDLER_PKG}`)"
            )));
        }
    }
    Ok(None)
}

// ─────────────────────────── M3: the diff pipeline ───────────────────────────

/// Build the compactor's spawn request (docs/handoffs/kb-groundskeeper.md M3):
/// the configured CHEAP model + per-pass token budget threaded onto the spawn, a
/// prompt naming the corpus to sweep. Dispatch-shape — the live sweep is the
/// compactor agent's own work; this is the deterministic launch envelope.
pub fn compactor_request(cfg: &PipelineConfig, corpus: &str) -> SpawnRequest {
    let prompt = format!(
        "You are the KB compactor. Sweep the knowledge corpus and propose \
         consolidations, link fixes, and conflict annotations as UNIFIED DIFFS only \
         — do not apply anything. Corpus:\n{corpus}\n\nEmit one unified diff per \
         proposed change. Stay within your token budget."
    );
    SpawnRequest {
        profile: COMPACTOR_PROFILE.to_string(),
        prompt,
        session: None,
        priority: 0,
        with_packages: Vec::new(),
        provider: None,
        created_by: Some(PKG.to_string()),
        model: Some(cfg.compactor_model.clone()),
        budget: Some(cfg.token_budget),
    }
}

/// Build the ratifier's spawn request (docs/handoffs/kb-groundskeeper.md M3): the
/// configured EXPENSIVE model + budget threaded, a prompt carrying the one diff to
/// judge. The ratifier either applies the diff (`lanius kb apply-diff`, one commit)
/// or bounces it WITH feedback that lands in the compactor's memory block.
pub fn ratifier_request(cfg: &PipelineConfig, diff: &str) -> SpawnRequest {
    let prompt = format!(
        "You are the KB ratifier — the trust boundary of an auto-approve pipeline. \
         Judge this ONE unified diff. If it is a correct, safe consolidation, apply \
         it with `lanius kb apply-diff <pkg>` (one commit). If not, BOUNCE it: append \
         concrete feedback to the compactor's `{BOUNCE_BLOCK}` memory block so the \
         next pass learns. Diff:\n{diff}"
    );
    SpawnRequest {
        profile: RATIFIER_PROFILE.to_string(),
        prompt,
        session: None,
        priority: 0,
        with_packages: Vec::new(),
        provider: None,
        created_by: Some(PKG.to_string()),
        model: Some(cfg.ratifier_model.clone()),
        budget: Some(cfg.token_budget),
    }
}

// A ratifier bounce is recorded by the ratifier agent itself via `lanius block
// append` (its prompt names the `ratifier-bounces` block), not a kernel helper —
// the CLI already covers this, so no `record_bounce` primitive lives here.
//
// Per-pass cost is not logged by a bespoke helper either: every compactor/ratifier
// run records its tokens into the `llm_usage` substrate (keyed by root_type,
// src/exec.rs), which is the general cost trail the human reads. The dispatch here
// is spawn-only (the passes run async under the daemon), so there is no synchronous
// point at which a "pass total" could be tallied — the llm_usage rows are the truth.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_blocks::{ContextBlock, Placement, Scope};
    use crate::db;
    use serde_json::json;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-gk-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    /// Install a [kb] package with the given files under <root>/packages/<name>/kb/.
    fn install_kb(root: &Root, name: &str, files: &[(&str, &str)]) {
        let pdir = root.packages().join(name);
        std::fs::create_dir_all(pdir.join("kb")).unwrap();
        std::fs::write(pdir.join("lanius.toml"), "[kb]\ntitle = \"t\"\n").unwrap();
        for (rel, body) in files {
            let p = pdir.join("kb").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    /// Seed a pointer block into the store with the given meta.
    fn seed_pointer(conn: &Connection, owner: &str, name: &str, meta: serde_json::Value) {
        let mut b = ContextBlock::new(name, "summary", owner);
        b.scope = Scope::Agent;
        b.placement = Placement::System;
        b.meta = meta;
        crate::context_store::upsert_block(conn, "default", &b, "", None).unwrap();
    }

    fn sha_of(body: &str) -> String {
        crate::context_blocks::sha256_hex(body.as_bytes())
    }

    #[test]
    fn sweep_reports_each_breakage_class() {
        // M1 acceptance: a corpus with one broken-path pointer, one stale-sha
        // pointer, and one orphan file — each class appears in the report.
        let root = scratch("sweep");
        let good = "# Verifier\nline two\nline three\n";
        install_kb(
            &root,
            "kb-demo",
            &[
                ("role-verifier.md", good),
                ("role-planner.md", "# Planner\nonly claude or fable\n"),
                ("orphan.md", "# Orphan\nnobody points here\n"),
            ],
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();

        // A healthy pointer (must NOT appear anywhere).
        seed_pointer(
            &conn,
            "architect",
            "ptr-ok",
            json!({"kb":"kb-demo","path":"kb/role-verifier.md","lines":"1-3","sha":sha_of(good)}),
        );
        // A broken-path pointer: the file does not exist.
        seed_pointer(
            &conn,
            "architect",
            "ptr-broken",
            json!({"kb":"kb-demo","path":"kb/gone.md","lines":"1-2","sha":sha_of("x")}),
        );
        // A stale-sha pointer: role-planner.md exists but its sha differs.
        seed_pointer(
            &conn,
            "architect",
            "ptr-stale",
            json!({"kb":"kb-demo","path":"kb/role-planner.md","lines":"1-2","sha":sha_of("STALE")}),
        );

        let report = sweep(&root, &conn, "default").unwrap();

        // Broken class: the missing-path pointer.
        assert!(
            report
                .broken
                .iter()
                .any(|i| i.path == "kb/gone.md" && matches!(i.status, PointerStatus::MissingPath)),
            "broken-path pointer must appear: {:?}",
            report.broken
        );
        // Stale class: the sha mismatch.
        assert!(
            report
                .stale
                .iter()
                .any(|i| i.path == "kb/role-planner.md"
                    && matches!(i.status, PointerStatus::StaleSha)),
            "stale-sha pointer must appear: {:?}",
            report.stale
        );
        // Orphan class: orphan.md is referenced by no pointer.
        assert!(
            report.orphans.iter().any(|o| o.path == "kb/orphan.md"),
            "orphan file must appear: {:?}",
            report.orphans
        );
        // The healthy pointer's file is NOT an orphan (it is referenced) and NOT
        // in broken/stale.
        assert!(!report
            .orphans
            .iter()
            .any(|o| o.path == "kb/role-verifier.md"));
        assert!(report.has_findings());
        assert_eq!(report.checked_pointers, 3);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn classify_pointer_pure_cases() {
        let root = scratch("classify");
        let f = root.dir.join("file.md");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let sha = sha_of("a\nb\nc\n");
        assert_eq!(classify_pointer(&f, "1-3", &sha), PointerStatus::Ok);
        assert_eq!(
            classify_pointer(&f, "1-3", "wrong"),
            PointerStatus::StaleSha
        );
        assert_eq!(classify_pointer(&f, "1-99", &sha), PointerStatus::BadLines);
        assert_eq!(
            classify_pointer(&root.dir.join("nope.md"), "1-3", &sha),
            PointerStatus::MissingPath
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    /// Recursively copy a directory tree (installs the shipped package the way a
    /// copy-mode kit install materializes it under <root>/packages).
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().filter_map(|e| e.ok()) {
            let src = e.path();
            let dst = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_tree(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    #[test]
    fn setup_gate_is_inert_until_config_and_approval() {
        // M2 acceptance: with no config and no approval the pipeline is inert; the
        // gate flips live ONLY when every required key is set AND the package is
        // approved. Drives the REAL shipped package on a scratch root.
        let shipped =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("kits/core/packages/kb-groundskeeper");
        let root = scratch("gate");
        copy_tree(&shipped, &root.packages().join(PKG));
        crate::config_repo::init(&root).unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();

        // The required keys are the manifest-declared ones (all four).
        let req = required_keys(&root);
        for k in [
            "compactor_model",
            "ratifier_model",
            "cadence",
            "token_budget",
        ] {
            assert!(
                req.contains(&k.to_string()),
                "{k} is a declared required key"
            );
        }

        // No config → load_config is inert (Err with a reason), and the gate is off.
        assert!(
            matches!(load_config(&root), Ok(Err(_))),
            "inert before config"
        );
        assert!(!is_setup_complete(&root, &conn), "gate off with no config");

        // Set every required key.
        crate::config_repo::set_key(&root, PKG, "compactor_model", "\"claude-haiku\"").unwrap();
        crate::config_repo::set_key(&root, PKG, "ratifier_model", "\"claude-fable-5\"").unwrap();
        crate::config_repo::set_key(&root, PKG, "cadence", "\"0 3 * * *\"").unwrap();
        crate::config_repo::set_key(&root, PKG, "token_budget", "20000").unwrap();

        // Config now loads to a concrete decision.
        let cfg = match load_config(&root).unwrap() {
            Ok(cfg) => cfg,
            Err(e) => panic!("config should load: {e}"),
        };
        assert_eq!(cfg.compactor_model, "claude-haiku");
        assert_eq!(cfg.ratifier_model, "claude-fable-5");
        assert_eq!(cfg.cadence, "0 3 * * *");
        assert_eq!(cfg.token_budget, 20000);

        // Config set but NOT approved → STILL inert (both gates required).
        assert!(
            !is_setup_complete(&root, &conn),
            "config alone does not turn the pipeline on — approval is the other gate"
        );

        // Approve → the gate finally flips live.
        packages::decide(&root, &conn, PKG, true, "test").unwrap();
        assert!(
            is_setup_complete(&root, &conn),
            "set up = config keys set AND approved"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn pipeline_is_daemon_drivable_after_setup_and_both_approvals() {
        // Regression (docs/handoffs/kb-groundskeeper.md M3): the reported bug was
        // that after a complete, correct setup `lanius kb groundskeep` still could
        // not spawn the compactor — spawn_core bailed "not daemon-drivable" because
        // NO approved exec package subscribed to in/agent/kb-compactor. The fix ships
        // the `kb-pipeline` exec handler. This drives the REAL shipped packages +
        // profiles on a scratch root and asserts the mailbox becomes drivable and
        // spawn_core actually launches.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = scratch("drivable");
        copy_tree(
            &manifest.join("kits/core/packages/kb-groundskeeper"),
            &root.packages().join(PKG),
        );
        copy_tree(
            &manifest.join("kits/core/packages/kb-pipeline"),
            &root.packages().join(HANDLER_PKG),
        );
        copy_tree(
            &manifest.join("kits/core/profiles/kb-compactor"),
            &root.profiles().join(COMPACTOR_PROFILE),
        );
        copy_tree(
            &manifest.join("kits/core/profiles/kb-ratifier"),
            &root.profiles().join(RATIFIER_PROFILE),
        );
        crate::config_repo::init(&root).unwrap();
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        packages::sync(&root, &conn).unwrap();

        // The full M2 gate: every config key set AND kb-groundskeeper approved.
        for (k, v) in [
            ("compactor_model", "\"claude-haiku\""),
            ("ratifier_model", "\"claude-fable-5\""),
            ("cadence", "\"0 3 * * *\""),
            ("token_budget", "20000"),
        ] {
            crate::config_repo::set_key(&root, PKG, k, v).unwrap();
        }
        packages::decide(&root, &conn, PKG, true, "test").unwrap();
        assert!(is_setup_complete(&root, &conn), "M2 gate is satisfied");

        // kb-pipeline NOT yet approved → the compactor mailbox has no approved exec
        // handler, so the pipeline is INERT with a reason that names the handler
        // (not a hard bail). This is exactly what groundskeep prints.
        let gap = handler_gap(&root, &conn).unwrap();
        assert!(
            gap.as_deref().is_some_and(|r| r.contains(HANDLER_PKG)),
            "before kb-pipeline is approved the gap must name it: {gap:?}"
        );

        // Approve the handler → both agent mailboxes become daemon-drivable.
        packages::decide(&root, &conn, HANDLER_PKG, true, "test").unwrap();
        packages::sync(&root, &conn).unwrap();
        assert!(
            handler_gap(&root, &conn).unwrap().is_none(),
            "both agent mailboxes must be drivable once kb-pipeline is approved"
        );

        // The reported bug is fixed: spawn_core now LAUNCHES the compactor.
        let cfg = match load_config(&root).unwrap() {
            Ok(c) => c,
            Err(e) => panic!("config should load: {e}"),
        };
        let corpus = corpus_digest(&root, "default").unwrap();
        let req = compactor_request(&cfg, &corpus);
        let descriptor = crate::agentcli::spawn_core(&root, &conn, req)
            .expect("spawn_core must launch the compactor once kb-pipeline is approved");
        assert_eq!(descriptor["mailbox"], "in/agent/kb-compactor");
        assert_eq!(descriptor["profile"], COMPACTOR_PROFILE);
        // The launch really emitted work onto the mailbox (not a no-op).
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type = 'in/agent/kb-compactor'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "the compactor launch emitted exactly one mailbox event"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn compactor_and_ratifier_requests_thread_model_and_budget() {
        // M3 dispatch-shape: the configured cheap/expensive model and per-pass token
        // budget thread onto each spawn request (the LLM cells verified here, live
        // run deferred to a smoke step).
        let cfg = PipelineConfig {
            compactor_model: "cheap-model".into(),
            ratifier_model: "strong-model".into(),
            cadence: "0 3 * * *".into(),
            token_budget: 12345,
        };
        let c = compactor_request(&cfg, "- kb-demo (1 file):\n    kb/a.md\n");
        assert_eq!(c.profile, COMPACTOR_PROFILE);
        assert_eq!(c.model.as_deref(), Some("cheap-model"));
        assert_eq!(c.budget, Some(12345));
        assert_eq!(c.created_by.as_deref(), Some(PKG));
        assert!(
            c.prompt.contains("kb/a.md"),
            "the corpus digest rides the prompt"
        );
        assert!(c.prompt.to_lowercase().contains("unified diff"));

        let r = ratifier_request(&cfg, "--- a/kb/a.md\n+++ b/kb/a.md\n");
        assert_eq!(r.profile, RATIFIER_PROFILE);
        assert_eq!(
            r.model.as_deref(),
            Some("strong-model"),
            "the EXPENSIVE model"
        );
        assert_eq!(r.budget, Some(12345));
        assert!(
            r.prompt.contains("+++ b/kb/a.md"),
            "the one diff rides the prompt"
        );
        assert!(r.prompt.contains("apply-diff") && r.prompt.contains(BOUNCE_BLOCK));
    }

    #[test]
    fn check_kb_format_classes_are_pure() {
        // M2 acceptance (pure finding logic): frontmatter-less, absolute-link,
        // escaping-relative, and a clean in-package cross-link.
        let root = scratch("fmtpure");
        let pkg = root.dir.join("pkg");
        let kb = pkg.join("kb");
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::write(kb.join("target.md"), "# target\n").unwrap();

        // 1) No frontmatter → MissingFrontmatter.
        let none = check_kb_format("# heading\nno frontmatter\n", &kb, &pkg);
        assert!(matches!(none.as_slice(), [FormatKind::MissingFrontmatter]));

        // 2) Absolute link → BadLink (frontmatter is fine here).
        let abs = check_kb_format(
            "---\ntitle: T\ndescription: d\n---\nsee [x](/abs/path.md)\n",
            &kb,
            &pkg,
        );
        assert!(abs
            .iter()
            .any(|k| matches!(k, FormatKind::BadLink { reason, .. } if reason == "absolute path")));

        // 3) Escaping relative link (../../../../docs/x.md shape) → DeadLink.
        let escape = check_kb_format(
            "---\ntitle: T\ndescription: d\n---\nsee [d](../../../../docs/x.md)\n",
            &kb,
            &pkg,
        );
        assert!(escape
            .iter()
            .any(|k| matches!(k, FormatKind::DeadLink { .. })));

        // 4) Clean: frontmatter complete, an in-package resolving cross-file link.
        let clean = check_kb_format(
            "---\ntitle: T\ndescription: d\ntags: [a]\n---\nsee [t](target.md)\n",
            &kb,
            &pkg,
        );
        assert!(clean.is_empty(), "a conforming entry has no findings: {clean:?}");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn sweep_reports_format_findings_over_the_corpus() {
        // M2 acceptance: a seeded corpus with one frontmatter-less file, one with an
        // absolute link, one with an escaping relative link, and one clean file →
        // exactly the first three are format findings; the fourth is clean.
        let root = scratch("fmtsweep");
        install_kb(
            &root,
            "kb-demo",
            &[
                ("bare.md", "# bare\nno frontmatter\n"),
                (
                    "abs.md",
                    "---\ntitle: Abs\ndescription: d\n---\nsee [x](/abs.md)\n",
                ),
                (
                    "escape.md",
                    "---\ntitle: Esc\ndescription: d\n---\nsee [d](../../../../docs/x.md)\n",
                ),
                (
                    "good.md",
                    "---\ntitle: Good\ndescription: d\n---\nsee [b](bare.md)\n",
                ),
            ],
        );
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        let report = sweep(&root, &conn, "default").unwrap();

        let path_has =
            |p: &str| report.format.iter().filter(|f| f.path == format!("kb/{p}")).count();
        assert_eq!(path_has("bare.md"), 1, "frontmatter-less file flagged once");
        assert!(report.format.iter().any(|f| f.path == "kb/bare.md"
            && matches!(f.kind, FormatKind::MissingFrontmatter)));
        assert!(report.format.iter().any(|f| f.path == "kb/abs.md"
            && matches!(f.kind, FormatKind::BadLink { .. })));
        assert!(report.format.iter().any(|f| f.path == "kb/escape.md"
            && matches!(f.kind, FormatKind::DeadLink { .. })));
        assert_eq!(path_has("good.md"), 0, "the conforming file is clean");
        assert!(report.has_findings());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn sweep_is_clean_on_a_healthy_corpus() {
        let root = scratch("clean");
        let body = "---\ntitle: ok\ndescription: a healthy entry\n---\n# ok\nx\n";
        install_kb(&root, "kb-demo", &[("a.md", body)]);
        let conn = db::open(&root).unwrap();
        db::init_schema(&conn).unwrap();
        seed_pointer(
            &conn,
            "architect",
            "ptr",
            json!({"kb":"kb-demo","path":"kb/a.md","lines":"1-2","sha":sha_of(body)}),
        );
        let report = sweep(&root, &conn, "default").unwrap();
        assert!(
            !report.has_findings(),
            "clean corpus has no findings: {report:?}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
