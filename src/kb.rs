//! The knowledge-base substrate (docs/handoffs/kb-core.md). A KB is a `kb/`
//! subfolder inside a package that carries the `[kb]` manifest marker (D1/D6) —
//! plain, greppable markdown, one topic per file, file+line anchors. There is no
//! `kb` table and no new topic plane: the kernel gains only the manifest marker
//! (manifest.rs `KbDecl`); everything else composes from substrates that already
//! ship (packages, the cage, git).
//!
//! This module owns two things: enumerating the enabled KBs (`lanius kb list`)
//! and the write path (D2) — a plain file write into a package's `kb/` tree
//! followed by one git commit using the shared hardened-git discipline
//! (`git_hardened`), the same untrusted-content hardening `config_repo` uses.
//! Authority is the agent's ordinary sandbox `fs_write` grant on that tree (the
//! cage, `sandbox.rs`, nothing custom); provenance is the git commit log plus the
//! ordinary obs trail — no provenance footers (D2).

use crate::git_hardened;
use crate::packages::{self, Package};
use crate::paths::Root;
use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};

/// The conventional subfolder that holds a package's knowledge base.
pub const KB_DIR: &str = "kb";

/// The package that ships the default (FTS5) knowledge-search engine, and the
/// filename of the index its daemon builds inside its state dir. The CLI reads
/// the index straight from that well-known location; the tool surface
/// (`search_knowledge`) is an engine-swap seam, but `lanius kb search` is the
/// convenience verb for the default engine (docs/handoffs/kb-search.md M3).
pub const KB_SEARCH_PKG: &str = "kb-search";
pub const INDEX_DB: &str = "kb-index.sqlite";

/// The FTS5 index path the kb-search daemon writes and the CLI reads:
/// `<root>/run/pkg-kb-search/kb-index.sqlite` — the daemon's LANIUS_SCRATCH.
pub fn search_index_path(root: &Root) -> PathBuf {
    root.run_dir()
        .join(format!("pkg-{KB_SEARCH_PKG}"))
        .join(INDEX_DB)
}

/// One ranked search hit: a file + line range an agent can open, plus a snippet.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub package: String,
    pub path: String,
    pub lines: String,
    pub snippet: String,
}

/// Split a query into word tokens (lowercased, punctuation dropped). Mirrors
/// `scripts/search`'s `re.findall(r"\w+", q.lower())` so the CLI and the tool
/// build the SAME FTS5 MATCH expression and return the SAME hits.
pub fn query_tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in query.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Query the FTS5 index the kb-search daemon builds — READ-ONLY, so the query
/// path is physically unable to corrupt the index it reads. AND semantics first
/// (a hit contains every term), falling back to OR when AND finds nothing;
/// ranked by bm25. This is the exact shape `scripts/search` runs, so
/// `lanius kb search` returns the same hits as the `search_knowledge` tool.
pub fn search(index_db: &Path, query: &str, limit: usize) -> Result<Vec<Hit>> {
    if !index_db.exists() {
        bail!(
            "no knowledge index yet at {} — is the kb-search package enabled, and has its \
             daemon run an index pass?",
            index_db.display()
        );
    }
    let conn = rusqlite::Connection::open_with_flags(
        index_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening the kb index {}", index_db.display()))?;
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let quoted: Vec<String> = tokens.iter().map(|t| format!("\"{t}\"")).collect();
    let hits = run_match(&conn, &quoted.join(" "), limit)?;
    if !hits.is_empty() {
        return Ok(hits);
    }
    run_match(&conn, &quoted.join(" OR "), limit)
}

fn run_match(conn: &rusqlite::Connection, match_expr: &str, limit: usize) -> Result<Vec<Hit>> {
    let mut stmt = conn.prepare(
        "SELECT package, path, line_start, line_end, \
                snippet(chunks, 4, '', '', ' … ', 12) \
         FROM chunks WHERE chunks MATCH ?1 ORDER BY bm25(chunks) LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![match_expr, limit as i64], |r| {
            let ls: i64 = r.get(2)?;
            let le: i64 = r.get(3)?;
            Ok(Hit {
                package: r.get(0)?,
                path: r.get(1)?,
                lines: format!("{ls}-{le}"),
                snippet: r.get::<_, String>(4)?.trim().to_string(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One enabled knowledge base, for `lanius kb list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KbInfo {
    /// The package name (also the topic/grant/sql key).
    pub package: String,
    /// The `[kb] title`, if declared.
    pub title: Option<String>,
    /// The `[kb] description`, if declared.
    pub description: Option<String>,
    /// The resolved `kb/` directory on disk.
    pub path: PathBuf,
    /// How many files the `kb/` tree carries (recursive).
    pub files: usize,
}

/// A package's `kb/` directory (whether or not it exists on disk yet).
pub fn kb_dir(pkg: &Package) -> PathBuf {
    pkg.dir.join(KB_DIR)
}

/// Every KB visible to `profile`: the discovered packages that carry the `[kb]`
/// marker (presence of the marker, not merely a `kb/` dir on disk — D1 wonky bit
/// 1). Name-sorted (discovery already sorts).
pub fn enumerate(root: &Root, profile: &str) -> Result<Vec<KbInfo>> {
    let mut out = Vec::new();
    for pkg in packages::discover_for_profile(root, profile)? {
        let Some(lm) = &pkg.manifest else { continue };
        let Some(kb) = &lm.manifest.kb else { continue };
        let dir = kb_dir(&pkg);
        out.push(KbInfo {
            package: pkg.name.clone(),
            title: kb.title.clone(),
            description: kb.description.clone(),
            files: count_files(&dir),
            path: dir,
        });
    }
    Ok(out)
}

/// Count regular files under a `kb/` tree, recursively. A missing dir is 0.
fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in entries.filter_map(|e| e.ok()) {
        // symlink_metadata: never follow a link (a KB tree holds agent-written
        // content, and a followed link could count/escape outside the tree).
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            n += count_files(&e.path());
        } else if ft.is_file() {
            n += 1;
        }
    }
    n
}

/// Result of a KB write: the package's tip commit and whether it changed.
pub struct WriteOutcome {
    pub package: String,
    pub rel: String,
    pub commit: String,
    pub changed: bool,
}

/// Write `content` to `kb/<rel>` inside `pkg`'s tree and commit exactly that path
/// on the package-dir git repo (initializing the repo on first write — the
/// package directory IS the git boundary, kb-core.md wonky bit 2). The write is a
/// plain file write: the cage is what actually gates it (an agent without the
/// `fs_write` grant on this tree is refused by seatbelt before this ever runs).
/// The commit uses the fixed kernel identity via the shared hardened-git helper;
/// who-wrote-what-when is the commit log plus the obs trail (D2, no footers).
pub fn write(root: &Root, pkg: &str, rel: &str, content: &str) -> Result<WriteOutcome> {
    let package = packages::find(root, pkg)
        .with_context(|| format!("resolving package {pkg:?} for a kb write"))?;
    let safe_rel = safe_kb_rel(rel)?;
    let kb = kb_dir(&package);
    let target = kb.join(&safe_rel);

    // Refuse to write THROUGH a symlink anywhere on the resolved chain: a KB tree
    // holds agent-written content, and a planted link could redirect the write
    // outside the package. Any existing ancestor that is a symlink is rejected.
    reject_symlink_chain(&kb, &safe_rel)?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(&target, content)?;

    ensure_repo(&package.dir)?;
    // The committed path is relative to the package-dir repo root: `kb/<rel>`.
    let rel_in_repo = format!("{KB_DIR}/{}", safe_rel.to_string_lossy());
    git_hardened::run_in(&package.dir, &["add", "--", &rel_in_repo], "kb add")?;
    // A no-op write stages nothing for the path — nothing to commit.
    if git_hardened::ok_in(
        &package.dir,
        &["diff", "--cached", "--quiet", "--", &rel_in_repo],
    ) {
        let commit = head_sha(&package.dir).unwrap_or_default();
        return Ok(WriteOutcome {
            package: package.name,
            rel: rel_in_repo,
            commit,
            changed: false,
        });
    }
    let msg = format!("kb: write {rel_in_repo}");
    git_hardened::run_in(
        &package.dir,
        &["commit", "-m", &msg, "--", &rel_in_repo],
        "kb commit",
    )?;
    let commit = head_sha(&package.dir)?;
    Ok(WriteOutcome {
        package: package.name,
        rel: rel_in_repo,
        commit,
        changed: true,
    })
}

/// Apply a unified diff to a package's `kb/` tree and commit exactly the paths it
/// touches — the ratifier's apply path (docs/handoffs/kb-groundskeeper.md M3). The
/// compactor's deliverable is a unified diff; ratification is `git apply` + one
/// commit under the fixed kernel identity, so every applied consolidation is an
/// auditable, revertible commit (kb-core D2). Every path the diff touches MUST be
/// under `kb/` with no traversal — an agent-authored diff is untrusted, so a diff
/// that reaches for `lanius.toml`, a `..`, or an absolute path is refused before
/// any file is touched.
pub fn apply_diff(root: &Root, pkg: &str, diff: &str) -> Result<WriteOutcome> {
    let package = packages::find(root, pkg)
        .with_context(|| format!("resolving package {pkg:?} for a kb diff apply"))?;
    let touched = diff_touched_paths(diff)?;
    if touched.is_empty() {
        bail!("diff touches no files under kb/");
    }
    // Path-discipline on every target: under kb/, no '..', no absolute root. Reuse
    // safe_kb_rel, which already strips a leading kb/ and rejects traversal.
    for rel in &touched {
        let under_kb = rel
            .strip_prefix(&format!("{KB_DIR}/"))
            .ok_or_else(|| anyhow::anyhow!("diff path {rel:?} is not under kb/"))?;
        safe_kb_rel(under_kb)
            .with_context(|| format!("diff path {rel:?} fails kb path-discipline"))?;
        // No symlink anywhere on the chain (a planted link could redirect writes).
        reject_symlink_chain(&kb_dir(&package), Path::new(under_kb))?;
    }
    ensure_repo(&package.dir)?;
    // `git apply --index` applies to the working tree AND stages, so the subsequent
    // commit records exactly the applied hunks. -p1 strips the a/ b/ prefixes.
    let mut child = std::process::Command::new("git");
    git_hardened::harden(&mut child);
    child
        .arg("-C")
        .arg(&package.dir)
        .args(["apply", "--index", "--whitespace=nowarn", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut proc = child.spawn().context("spawning git apply")?;
    {
        use std::io::Write;
        proc.stdin
            .take()
            .context("git apply stdin")?
            .write_all(diff.as_bytes())?;
    }
    let out = proc.wait_with_output().context("git apply")?;
    if !out.status.success() {
        bail!(
            "git apply failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // Nothing staged (an empty/no-op diff) is not an error but not a commit either.
    if git_hardened::ok_in(&package.dir, &["diff", "--cached", "--quiet"]) {
        let commit = head_sha(&package.dir).unwrap_or_default();
        return Ok(WriteOutcome {
            package: package.name,
            rel: touched.join(", "),
            commit,
            changed: false,
        });
    }
    let msg = format!("kb: ratify diff ({})", touched.join(", "));
    git_hardened::run_in(&package.dir, &["commit", "-m", &msg], "kb ratify commit")?;
    let commit = head_sha(&package.dir)?;
    Ok(WriteOutcome {
        package: package.name,
        rel: touched.join(", "),
        commit,
        changed: true,
    })
}

/// The set of repo-relative paths a unified diff touches, read from its `+++ b/…`
/// (and `--- a/…`) headers. `/dev/null` (a pure add/delete side) is skipped. The
/// `a/`/`b/` prefixes are stripped so the returned paths are repo-relative.
fn diff_touched_paths(diff: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for line in diff.lines() {
        let rest = if let Some(r) = line.strip_prefix("+++ ") {
            r
        } else if let Some(r) = line.strip_prefix("--- ") {
            r
        } else {
            continue;
        };
        // Drop a trailing tab-delimited timestamp some diff tools append.
        let rest = rest.split('\t').next().unwrap_or(rest).trim();
        if rest == "/dev/null" {
            continue;
        }
        let path = rest
            .strip_prefix("a/")
            .or_else(|| rest.strip_prefix("b/"))
            .unwrap_or(rest);
        if path.is_empty() {
            bail!("diff header has an empty path");
        }
        if !out.contains(&path.to_string()) {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

/// Validate a KB-relative path: a normal relative path under `kb/`, no absolute
/// root, no `..`, no empty/`.` components. Returns the cleaned relative path,
/// which is ALWAYS relative to the `kb/` dir (not the package root). A leading
/// `kb/` is accepted and stripped, so both `role-verifier.md` and the
/// package-root form `kb/role-verifier.md` (the shape pointer `meta.path` uses)
/// resolve to the same file — no doubled `kb/kb/`.
fn safe_kb_rel(rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().trim_start_matches("./");
    let rel = rel
        .strip_prefix("kb/")
        .or_else(|| (rel == "kb").then_some(""))
        .unwrap_or(rel);
    if rel.is_empty() {
        bail!("kb path is empty");
    }
    let p = Path::new(rel);
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if s.is_empty() {
                    bail!("kb path {rel:?} has an empty segment");
                }
                clean.push(seg);
            }
            Component::CurDir => {}
            Component::ParentDir => bail!("kb path {rel:?} may not contain '..'"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("kb path {rel:?} must be relative to kb/")
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("kb path {rel:?} resolves to nothing");
    }
    Ok(clean)
}

/// Refuse if any existing ancestor of `kb/<rel>` (including `kb` itself) is a
/// symlink — a planted link could redirect the write outside the package tree.
fn reject_symlink_chain(kb: &Path, rel: &Path) -> Result<()> {
    if is_symlink(kb) {
        bail!(
            "kb directory {} is a symlink — refusing to write",
            kb.display()
        );
    }
    let mut cur = kb.to_path_buf();
    // Walk every intermediate directory of `rel` (not the final leaf file, which
    // may not exist yet, and which write_atomic replaces via rename).
    let comps: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    for seg in &comps[..comps.len().saturating_sub(1)] {
        cur = cur.join(seg);
        if is_symlink(&cur) {
            bail!(
                "kb path component {} is a symlink — refusing",
                cur.display()
            );
        }
    }
    // The leaf, if it already exists, must be a regular file we can replace.
    let leaf = kb.join(rel);
    if is_symlink(&leaf) {
        bail!(
            "kb target {} is a symlink — refusing to overwrite",
            leaf.display()
        );
    }
    Ok(())
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Ensure a git repo exists at the package dir (the kb git boundary). Idempotent:
/// a dir that already has `.git` is left untouched. Uses the shared hardened git.
fn ensure_repo(pkg_dir: &Path) -> Result<()> {
    if pkg_dir.join(".git").exists() {
        return Ok(());
    }
    git_hardened::run_in(pkg_dir, &["init", "-b", "main"], "kb repo init")?;
    Ok(())
}

fn head_sha(pkg_dir: &Path) -> Result<String> {
    git_hardened::run_in(pkg_dir, &["rev-parse", "HEAD"], "kb rev-parse")
}

/// Write a file atomically (sibling temp + rename), so a concurrent reader — a
/// grep or an indexer sweep — sees the whole old or whole new file, never a torn
/// one. Mirrors config_repo's write discipline.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("kbtmp");
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

// ───────────────── The KB entry format: the deterministic parser ─────────────────
//
// docs/handoffs/kb-formalization.md. A KB entry is markdown with an optional
// `---`-delimited YAML-ish frontmatter block (single-line scalars only) plus
// inline relative links. This is a plain, no-LLM parser a script can rely on. The
// frontmatter reader deliberately mirrors `manifest::skill_md` — the same minimal
// `---` single-line-scalar discipline, no YAML dependency (wonky bit 1).

/// The parsed frontmatter block of a KB entry. `title`/`description` are
/// single-line scalars; `tags` is one inline `[a, b, c]` list. Unknown keys are
/// ignored (forward-compatible), surfaced only so a caller could report them.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub unknown_keys: Vec<String>,
}

/// How a link target classifies. `classify_link` computes it from the raw target.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LinkClass {
    /// A real external link — the target carries a `scheme:` (http:, https:, mailto:).
    External,
    /// An in-page anchor — a `#fragment`-only target.
    Anchor,
    /// An absolute path (`/…`) — disallowed for an internal reference.
    Absolute,
    /// A relative internal reference: the path (fragment stripped) plus any fragment.
    Relative {
        path: String,
        fragment: Option<String>,
    },
}

/// One link extracted from an entry's body. `reference_style` marks the disallowed
/// `[text][id]` form (inline `[text](target)` is false) so a consumer can flag it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Link {
    pub text: String,
    pub target: String,
    pub line: usize,
    pub class: LinkClass,
    pub reference_style: bool,
}

/// A fully parsed KB entry: its frontmatter (if any), its links, and the 1-based
/// line at which the body begins (past the frontmatter) so a consumer can index
/// the body without the frontmatter.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedEntry {
    pub frontmatter: Option<Frontmatter>,
    pub links: Vec<Link>,
    pub body_start_line: usize,
}

/// Parse a KB entry: read the optional `---` frontmatter block, then extract every
/// inline `[text](target)` link (and flag the disallowed reference-style form) from
/// the body. Pure over its input, fully unit-testable. No LLM, no new dependency.
pub fn parse_kb_entry(text: &str) -> ParsedEntry {
    let (frontmatter, body_start_line) = parse_frontmatter(text);
    let links = extract_links(text, body_start_line);
    ParsedEntry {
        frontmatter,
        links,
        body_start_line,
    }
}

/// Read the leading `---`-delimited frontmatter block, mirroring
/// `manifest::skill_md`. Returns the parsed block (or `None` when the file does not
/// open with `---`) and the 1-based line where the body begins.
fn parse_frontmatter(text: &str) -> (Option<Frontmatter>, usize) {
    let mut lines = text.lines();
    // The very first line must be exactly `---` (single-line-scalar discipline).
    match lines.next() {
        Some(l) if l.trim() == "---" => {}
        _ => return (None, 1),
    }
    let mut fm = Frontmatter::default();
    // Line 1 was the opening `---`; count from there to find the body start.
    let mut consumed = 1usize;
    let mut closed = false;
    for line in lines {
        consumed += 1;
        let t = line.trim();
        if t == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "title" => fm.title = Some(value.to_string()),
            "description" => fm.description = Some(value.to_string()),
            "tags" => fm.tags = parse_tags(value),
            other if !other.is_empty() => fm.unknown_keys.push(other.to_string()),
            _ => {}
        }
    }
    if !closed {
        // An unterminated block is not a valid frontmatter block.
        return (None, 1);
    }
    // The body begins on the line after the closing `---` (1-based).
    (Some(fm), consumed + 1)
}

/// Parse an inline `[a, b, c]` tag list into trimmed, non-empty tags. A bare
/// `a, b` (no brackets) is accepted too; empties are dropped.
fn parse_tags(value: &str) -> Vec<String> {
    let inner = value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    inner
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Classify a raw link target: `External` (has a `scheme:`), `Anchor` (`#…`),
/// `Absolute` (`/…`), or `Relative { path, fragment }` (everything else, with the
/// `#fragment`/`?query` stripped from the resolved path).
pub fn classify_link(target: &str) -> LinkClass {
    let t = target.trim();
    if t.is_empty() {
        return LinkClass::Relative {
            path: String::new(),
            fragment: None,
        };
    }
    if has_scheme(t) {
        return LinkClass::External;
    }
    if t.starts_with('#') {
        return LinkClass::Anchor;
    }
    if t.starts_with('/') {
        return LinkClass::Absolute;
    }
    let (path, fragment) = split_fragment(t);
    LinkClass::Relative { path, fragment }
}

/// Whether a target begins with a URL scheme — `letters(+.-)*` then `:` (e.g.
/// `https:`, `mailto:`). A relative path like `role-verifier.md` has no such colon.
fn has_scheme(t: &str) -> bool {
    let mut seen_alpha = false;
    for (i, ch) in t.char_indices() {
        if i == 0 {
            if !ch.is_ascii_alphabetic() {
                return false;
            }
            seen_alpha = true;
            continue;
        }
        if ch == ':' {
            return seen_alpha;
        }
        if ch.is_ascii_alphanumeric() || ch == '+' || ch == '.' || ch == '-' {
            continue;
        }
        return false;
    }
    false
}

/// Split a relative target into its on-disk path and any trailing `#fragment`. A
/// `?query` (rare for a file link) is also stripped from the path.
fn split_fragment(t: &str) -> (String, Option<String>) {
    let (path, fragment) = match t.split_once('#') {
        Some((p, f)) => (p, Some(f.to_string())),
        None => (t, None),
    };
    let path = path.split('?').next().unwrap_or(path);
    (path.to_string(), fragment)
}

/// The outcome of resolving a relative internal link against the file's directory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkResolution {
    /// The lexically-normalized path the target resolves to (for diagnostics).
    pub path: PathBuf,
    /// True ONLY when the target exists on disk AND lies inside `package_root` — the
    /// portable unit. A target that escapes the package tree resolves to `false`,
    /// the same as a missing file (the one `dead_link` contract, wonky bit 4).
    pub resolves: bool,
}

/// Resolve a relative link target against `kb_file_dir` (the directory of the KB
/// file that carries the link — standard markdown/POSIX). `resolves` is true only
/// when the resolved path is a file that EXISTS and lies INSIDE `package_root`: an
/// escaping `../../../../docs/x.md` resolves to `false` exactly like a missing file,
/// because a package installs without the repo's `docs/` so the target does not
/// travel with the package.
pub fn resolve_relative(kb_file_dir: &Path, package_root: &Path, target: &str) -> LinkResolution {
    let (rel, _frag) = split_fragment(target.trim());
    let normalized = normalize_lexical(&kb_file_dir.join(&rel));
    let root = normalize_lexical(package_root);
    let inside = normalized.starts_with(&root);
    // symlink_metadata: never follow a link out of the tree; a real target is a file.
    let exists = std::fs::symlink_metadata(&normalized)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false);
    LinkResolution {
        path: normalized,
        resolves: inside && exists,
    }
}

/// Collapse `.` and `..` components lexically (no disk access), so an escaping
/// `../../../../docs/x.md` normalizes to a path OUTSIDE the package root — which is
/// exactly what the `starts_with` package-containment check keys on.
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Extract inline `[text](target)` links (and flag reference-style `[text][id]`)
/// from the body (lines at or after `body_start_line`). One left-to-right pass per
/// line; targets do not span lines in the corpus, so per-line scanning is exact.
fn extract_links(text: &str, body_start_line: usize) -> Vec<Link> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, line) in text.lines().enumerate() {
        let lineno = i + 1;
        // A ``` or ~~~ fence line toggles a fenced code block; links inside are code
        // samples (the format spec's own ✅/❌ examples), never real references.
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || lineno < body_start_line {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut j = 0;
        while j < chars.len() {
            if chars[j] != '[' {
                j += 1;
                continue;
            }
            let Some(close) = find_char(&chars, j + 1, ']') else {
                break; // no closing ] on this line
            };
            let link_text: String = chars[j + 1..close].iter().collect();
            let next = chars.get(close + 1).copied();
            if next == Some('(') {
                if let Some(pclose) = find_char(&chars, close + 2, ')') {
                    let target: String = chars[close + 2..pclose].iter().collect();
                    out.push(Link {
                        class: classify_link(&target),
                        text: link_text,
                        target,
                        line: lineno,
                        reference_style: false,
                    });
                    j = pclose + 1;
                    continue;
                }
            } else if next == Some('[') {
                if let Some(rclose) = find_char(&chars, close + 2, ']') {
                    let id: String = chars[close + 2..rclose].iter().collect();
                    out.push(Link {
                        class: classify_link(&id),
                        text: link_text,
                        target: id,
                        line: lineno,
                        reference_style: true,
                    });
                    j = rclose + 1;
                    continue;
                }
            }
            j = close + 1;
        }
    }
    out
}

/// Index of the first `needle` in `chars` at or after `from`, if any.
fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    chars.iter().skip(from).position(|&c| c == needle).map(|p| p + from)
}

/// Parse the KB entry at `kb/<rel>` inside `pkg` (path-disciplined, no traversal) —
/// the sharp edge behind `lanius kb parse`. The package's own tree is the resolution
/// root for its links.
pub fn parse_file(root: &Root, pkg: &str, rel: &str) -> Result<ParsedEntry> {
    let package = packages::find(root, pkg)
        .with_context(|| format!("resolving package {pkg:?} for a kb parse"))?;
    let safe = safe_kb_rel(rel)?;
    let file = kb_dir(&package).join(&safe);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("reading kb file {}", file.display()))?;
    Ok(parse_kb_entry(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-kb-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Root { dir }
    }

    /// Install a minimal `[kb]` package under <root>/packages/<name> with one
    /// seed kb file, matching what `discover_for_profile("default")` walks.
    fn install_kb_pkg(root: &Root, name: &str, title: &str) {
        let pdir = root.packages().join(name);
        std::fs::create_dir_all(pdir.join("kb")).unwrap();
        std::fs::write(
            pdir.join("lanius.toml"),
            format!("[kb]\ntitle = \"{title}\"\n"),
        )
        .unwrap();
        std::fs::write(pdir.join("kb/seed.md"), "# seed\n").unwrap();
    }

    #[test]
    fn enumerate_keys_on_the_marker_not_the_dir() {
        let root = scratch("enum");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        // A package with a kb/ dir but NO [kb] marker must NOT be listed.
        let plain = root.packages().join("plain");
        std::fs::create_dir_all(plain.join("kb")).unwrap();
        std::fs::write(plain.join("lanius.toml"), "[request]\nsubscribe=[]\n").unwrap();
        std::fs::write(plain.join("kb/private.md"), "x").unwrap();

        let kbs = enumerate(&root, "default").unwrap();
        assert_eq!(kbs.len(), 1, "only the marked package is a KB");
        assert_eq!(kbs[0].package, "kb-demo");
        assert_eq!(kbs[0].title.as_deref(), Some("Demo KB"));
        assert_eq!(kbs[0].files, 1);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_commits_with_kernel_committer_and_reconstructs() {
        let root = scratch("write");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        let out = write(&root, "kb-demo", "notes/topic.md", "first\n").unwrap();
        assert!(out.changed);
        assert_eq!(out.rel, "kb/notes/topic.md");
        let pkg_dir = root.packages().join("kb-demo");
        assert!(pkg_dir.join("kb/notes/topic.md").exists());
        // The commit records the change under the fixed kernel committer.
        let log = git_hardened::run_in(
            &pkg_dir,
            &["log", "--format=%cn <%ce>%n%s", "--", "kb/notes/topic.md"],
            "log",
        )
        .unwrap();
        assert!(
            log.contains("lanius <lanius@localhost>"),
            "kernel committer: {log}"
        );
        assert!(
            log.contains("kb: write kb/notes/topic.md"),
            "commit subject: {log}"
        );

        // A second write is a new commit; the git log reconstructs who-what-when.
        let out2 = write(&root, "kb-demo", "notes/topic.md", "second\n").unwrap();
        assert!(out2.changed);
        assert_ne!(out.commit, out2.commit);
        // An identical re-write is a no-op — no new commit.
        let out3 = write(&root, "kb-demo", "notes/topic.md", "second\n").unwrap();
        assert!(!out3.changed, "idempotent write makes no commit");
        assert_eq!(out2.commit, out3.commit);
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_accepts_optional_kb_prefix_without_doubling() {
        let root = scratch("prefix");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        // The package-root form (as pointer meta.path uses) must NOT double kb/.
        let a = write(&root, "kb-demo", "kb/role-verifier.md", "x\n").unwrap();
        assert_eq!(a.rel, "kb/role-verifier.md");
        // The kb-relative form resolves to the SAME file (idempotent second write).
        let b = write(&root, "kb-demo", "role-verifier.md", "x\n").unwrap();
        assert_eq!(b.rel, "kb/role-verifier.md");
        assert!(!b.changed, "same file, same content — no second commit");
        assert!(root.packages().join("kb-demo/kb/role-verifier.md").exists());
        assert!(!root.packages().join("kb-demo/kb/kb").exists(), "no kb/kb/");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn write_rejects_path_traversal() {
        let root = scratch("traverse");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        assert!(write(&root, "kb-demo", "../escape.md", "x").is_err());
        assert!(write(&root, "kb-demo", "/abs.md", "x").is_err());
        assert!(write(&root, "kb-demo", "a/../../b.md", "x").is_err());
        assert!(write(&root, "kb-demo", "", "x").is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    /// Recursively copy a directory tree (test helper — installs the shipped
    /// package onto a scratch root the way init materializes it).
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
    fn seeded_kb_llm_strengths_installs_and_lists() {
        // M2 acceptance: the shipped kb-llm-strengths package installs on a scratch
        // root and shows in `lanius kb list`; its role/model files exist and
        // cross-link; the verifier facts are grep-able with a file; the invariants
        // are encoded verbatim.
        let shipped =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("kits/stdlib/packages/kb-llm-strengths");
        assert!(
            shipped.join("lanius.toml").exists(),
            "package ships in stdlib"
        );

        let root = scratch("seed");
        copy_tree(&shipped, &root.packages().join("kb-llm-strengths"));

        // Shows in the list with its title and file count.
        let kbs = enumerate(&root, "default").unwrap();
        let e = kbs
            .iter()
            .find(|k| k.package == "kb-llm-strengths")
            .expect("kb-llm-strengths is listed");
        assert_eq!(e.title.as_deref(), Some("LLM strengths"));
        assert!(e.files >= 8, "one file per model + one per role");

        // One file per model and one per role, all present.
        let kb = e.path.clone();
        for f in [
            "claude.md",
            "fable.md",
            "opus.md",
            "gpt-5.5.md",
            "glm-5.2.md",
            "role-planner.md",
            "role-implementer.md",
            "role-verifier.md",
        ] {
            assert!(kb.join(f).exists(), "kb/{f} exists");
        }

        // Cross-linking: a role file links a model file by relative path.
        let planner = std::fs::read_to_string(kb.join("role-planner.md")).unwrap();
        assert!(
            planner.contains("(claude.md)"),
            "role links model by rel path"
        );
        assert!(planner.contains("(fable.md)"));
        let opus = std::fs::read_to_string(kb.join("opus.md")).unwrap();
        assert!(opus.contains("role-implementer.md") && opus.contains("role-verifier.md"));

        // The verifier facts are discoverable by grep ("who verifies") and encode
        // the invariant verbatim: verify = Opus/GPT-5.5 high, Fable for the hardest.
        let verifier = std::fs::read_to_string(kb.join("role-verifier.md")).unwrap();
        assert!(
            verifier.to_lowercase().contains("who verifies"),
            "grep -ri 'who verifies' finds the verifier facts"
        );
        assert!(verifier.contains("Opus on high"));
        assert!(verifier.contains("GPT-5.5 on high/xhigh"));
        assert!(verifier.contains("Fable for the hardest"));

        // Planning never flexes; plan = Claude/Fable only.
        assert!(planner.contains("Only Claude or Fable plan"));
        assert!(planner.to_lowercase().contains("never") && planner.contains("GPT-5.5"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn shipped_architect_pointer_meta_matches_the_kb_file() {
        // M3 acceptance: the seeded architect pointer block's meta resolves to a
        // real file + line range + a MATCHING sha (content-sha256 of the target
        // kb file). Staleness is acceptable (kb-core.md wonky bit 4), but the
        // shipped snapshot must be consistent at ship time so B5's checker starts
        // green — a wrong sha here is a real bug, not deferred drift.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let block = std::fs::read_to_string(
            manifest_dir.join("kits/core/profiles/architect/blocks/10-kb-llm-strengths.md"),
        )
        .unwrap();
        // Extract the JSON frontmatter (between the first two `---` lines).
        let front = block
            .strip_prefix("---\n")
            .and_then(|r| r.split("\n---\n").next())
            .expect("pointer block has JSON frontmatter");
        let meta: serde_json::Value = serde_json::from_str(front).unwrap();
        assert_eq!(meta["kb"], "kb-llm-strengths");
        let rel = meta["path"].as_str().unwrap();
        assert_eq!(rel, "kb/role-verifier.md");

        let target = manifest_dir
            .join("kits/stdlib/packages/kb-llm-strengths")
            .join(rel);
        let bytes = std::fs::read(&target).expect("the referenced kb file exists");
        let sha = crate::context_blocks::sha256_hex(&bytes);
        assert_eq!(
            meta["sha"].as_str().unwrap(),
            sha,
            "pointer sha must match the kb file's content-sha256"
        );
        // The line range is well-formed and within the file.
        let (lo, hi) = meta["lines"]
            .as_str()
            .unwrap()
            .split_once('-')
            .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
            .unwrap();
        let line_count = String::from_utf8_lossy(&bytes).lines().count();
        assert!(lo >= 1 && hi >= lo && hi <= line_count.max(1) + 1);
    }

    // M4 acceptance: the write is gated by the ordinary sandbox fs_write grant on
    // the package tree (the cage, nothing custom). A *copied* KB lives inside the
    // root (the agent's own writable world); a *linked* KB lives OUTSIDE the root,
    // so writing it needs an explicit fs_write grant — that is the gate. This test
    // exercises the linked case: without the grant the cage refuses the external
    // kb write; with it, the write succeeds. macOS-only (the enforcement
    // mechanism); a no-op elsewhere — the property under test is the cage's.
    #[test]
    #[cfg(target_os = "macos")]
    fn kb_write_is_gated_by_the_fs_write_grant() {
        use crate::profile::SandboxCfg;
        use crate::sandbox::Cage;
        if !std::path::Path::new("/usr/bin/sandbox-exec").exists() {
            return;
        }
        let root = scratch("cage");
        // canonicalize: seatbelt subpath rules match the real (inode) path.
        let root = Root {
            dir: root.dir.canonicalize().unwrap(),
        };
        std::fs::create_dir_all(root.packages()).unwrap();
        // A LINKED package's kb tree living OUTSIDE the harness root — and outside
        // the temp write-holes seatbelt always allows, so under $HOME (the same
        // "definitely outside" trick seatbelt_actually_cages uses).
        let home = std::env::var("HOME").unwrap();
        let linked_root =
            std::path::Path::new(&home).join(format!("el-kb-linked-{}", std::process::id()));
        let external_kb = linked_root.join("kb");
        std::fs::create_dir_all(&external_kb).unwrap();
        let external_kb = external_kb.canonicalize().unwrap();

        // NOT GRANTED: an enforcing cage whose only extra write root is an in-root
        // scratch dir — the external kb tree is outside every write root, so the
        // cage refuses the write. (A non-empty fs_write is what turns enforcement
        // on; naming an unrelated in-root path keeps the external tree ungranted.)
        let in_root_scratch = root.dir.join("scratch");
        std::fs::create_dir_all(&in_root_scratch).unwrap();
        let ungranted = Cage::from_profile(
            &root,
            &SandboxCfg {
                fs_write: vec![in_root_scratch.display().to_string()],
                ..Default::default()
            },
        );
        assert!(ungranted.enforcing());
        let denied = ungranted
            .shell_command(&format!(
                "echo sneak > {}",
                external_kb.join("x.md").display()
            ))
            .output()
            .unwrap();
        assert!(
            !denied.status.success(),
            "an external (linked) kb write without the grant must be refused by the cage"
        );

        // GRANTED: fs_write names the linked kb tree — the write now succeeds.
        let granted = Cage::from_profile(
            &root,
            &SandboxCfg {
                fs_write: vec![external_kb.display().to_string()],
                ..Default::default()
            },
        );
        assert!(granted.enforcing());
        let ok = granted
            .shell_command(&format!(
                "echo knowledge > {}",
                external_kb.join("x.md").display()
            ))
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "granted external kb write must succeed: {ok:?}"
        );

        std::fs::remove_dir_all(&linked_root).ok();
        std::fs::remove_dir_all(&root.dir).ok();
    }

    /// Build a fixture FTS5 index with the SAME schema the daemon writes, so the
    /// Rust query path (kb::search — the `lanius kb search` engine) is tested
    /// deterministically without invoking python.
    fn build_fixture_index(path: &Path, rows: &[(&str, &str, i64, i64, &str)]) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute(
            "CREATE VIRTUAL TABLE chunks USING fts5(\
               package UNINDEXED, path UNINDEXED, line_start UNINDEXED, \
               line_end UNINDEXED, chunk, tokenize = 'porter unicode61')",
            [],
        )
        .unwrap();
        for (pkg, p, ls, le, body) in rows {
            conn.execute(
                "INSERT INTO chunks(package, path, line_start, line_end, chunk) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![pkg, p, ls, le, body],
            )
            .unwrap();
        }
    }

    #[test]
    fn search_ranks_the_verifier_chunk_top_and_returns_file_lines() {
        // M1/M3 query correctness: "who verifies" returns kb/role-verifier.md with
        // its line range — the file + line a cold-start agent opens.
        let root = scratch("search");
        let db = root.dir.join("kb-index.sqlite");
        build_fixture_index(
            &db,
            &[
                ("kb-llm-strengths", "kb/role-planner.md", 1, 8, "Role: planner. Only Claude or Fable plan; planning never flexes."),
                ("kb-llm-strengths", "kb/role-verifier.md", 6, 14, "Who verifies\nOpus on high\nGPT-5.5 on high\nFable for the hardest verifications."),
                ("kb-llm-strengths", "kb/claude.md", 1, 5, "Claude is a planning model with taste."),
            ],
        );
        let hits = search(&db, "who verifies", 5).unwrap();
        assert!(!hits.is_empty(), "a well-formed query returns hits");
        assert_eq!(hits[0].package, "kb-llm-strengths");
        assert_eq!(hits[0].path, "kb/role-verifier.md");
        assert_eq!(
            hits[0].lines, "6-14",
            "the hit carries the openable line range"
        );
        assert!(hits[0].snippet.to_lowercase().contains("verif"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn search_falls_back_to_or_when_and_finds_nothing() {
        // porter stemming + AND-then-OR: a query mixing a present stem with an
        // absent term still returns the chunk carrying the present one.
        let root = scratch("search-or");
        let db = root.dir.join("kb-index.sqlite");
        build_fixture_index(
            &db,
            &[(
                "p",
                "kb/a.md",
                1,
                3,
                "verification is a stronger tier than implementation",
            )],
        );
        // "stronger" is present; "zznope" is absent → AND (both terms) is empty,
        // so the OR fallback (any term) recovers the chunk on "stronger".
        let hits = search(&db, "stronger zznope", 5).unwrap();
        assert_eq!(hits.len(), 1, "OR fallback recovers the chunk: {hits:?}");
        assert_eq!(hits[0].path, "kb/a.md");
        // A missing index is a legible error, not a panic.
        assert!(search(&root.dir.join("nope.sqlite"), "x", 5).is_err());
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn query_tokens_strips_punctuation_and_lowercases() {
        assert_eq!(query_tokens("Who verifies?"), vec!["who", "verifies"]);
        assert_eq!(
            query_tokens("  GPT-5.5 on high!  "),
            vec!["gpt", "5", "5", "on", "high"]
        );
        assert!(query_tokens("???").is_empty());
    }

    #[test]
    fn apply_diff_ratifies_a_change_and_refuses_off_tree_paths() {
        // M3: the ratifier's apply path — a unified diff into kb/ becomes one commit;
        // a diff reaching outside kb/ (or with '..') is refused before any write.
        let root = scratch("applydiff");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        // Seed an initial file (this also inits the package repo + first commit).
        write(&root, "kb-demo", "a.md", "line1\nline2\n").unwrap();
        let pkg_dir = root.packages().join("kb-demo");
        let before = head_sha(&pkg_dir).unwrap();

        // A well-formed unified diff editing line2 → applies + commits.
        let diff = "\
--- a/kb/a.md
+++ b/kb/a.md
@@ -1,2 +1,2 @@
 line1
-line2
+line2-edited
";
        let out = apply_diff(&root, "kb-demo", diff).unwrap();
        assert!(out.changed, "the diff produced a commit");
        assert_ne!(out.commit, before, "a new ratifier commit");
        let now = std::fs::read_to_string(pkg_dir.join("kb/a.md")).unwrap();
        assert!(
            now.contains("line2-edited"),
            "the diff was applied: {now:?}"
        );
        // The commit subject records ratification.
        let subj = git_hardened::run_in(&pkg_dir, &["log", "-1", "--format=%s"], "log").unwrap();
        assert!(subj.contains("ratify"), "commit subject: {subj}");

        // A diff that reaches OUTSIDE kb/ is refused before any file is touched.
        let escape = "\
--- a/lanius.toml
+++ b/lanius.toml
@@ -1 +1 @@
-x
+y
";
        assert!(
            apply_diff(&root, "kb-demo", escape).is_err(),
            "a diff touching lanius.toml must be refused"
        );
        // A traversal diff is refused too.
        let traverse = "\
--- a/kb/../escape.md
+++ b/kb/../escape.md
@@ -1 +1 @@
-x
+y
";
        assert!(
            apply_diff(&root, "kb-demo", traverse).is_err(),
            "traversal refused"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    /// Every `.md` file under a shipped package's `kb/` tree (recursive), as
    /// (repo-relative-to-kb path, absolute file path) pairs.
    fn shipped_kb_files(pkg_dir: &Path) -> Vec<(String, PathBuf)> {
        fn walk(base: &Path, rel: &Path, out: &mut Vec<(String, PathBuf)>) {
            let here = base.join(rel);
            for e in std::fs::read_dir(&here).unwrap().filter_map(|e| e.ok()) {
                let child = rel.join(e.file_name());
                if e.file_type().unwrap().is_dir() {
                    walk(base, &child, out);
                } else if child.extension().and_then(|x| x.to_str()) == Some("md") {
                    out.push((child.to_string_lossy().to_string(), base.join(&child)));
                }
            }
        }
        let kb = pkg_dir.join("kb");
        let mut out = Vec::new();
        walk(&kb, Path::new(""), &mut out);
        out
    }

    #[test]
    fn shipped_kb_files_conform_to_the_format() {
        // M4 acceptance: every shipped KB file (both packages) parses with valid
        // frontmatter (title + description) and carries only legitimate, resolving
        // relative-inline links — no absolute, no reference-style, no escaping link.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for pkg_dir in [
            manifest.join("kits/stdlib/packages/kb-llm-strengths"),
            manifest.join("kits/helper/packages/kb-lanius"),
        ] {
            let package_root = &pkg_dir;
            let files = shipped_kb_files(&pkg_dir);
            assert!(!files.is_empty(), "package ships kb files: {pkg_dir:?}");
            for (rel, file) in files {
                let text = std::fs::read_to_string(&file).unwrap();
                let parsed = parse_kb_entry(&text);
                let fm = parsed
                    .frontmatter
                    .unwrap_or_else(|| panic!("{rel} has frontmatter"));
                assert!(
                    fm.title.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false),
                    "{rel} has a non-empty title"
                );
                assert!(
                    fm.description
                        .as_deref()
                        .map(|d| !d.trim().is_empty())
                        .unwrap_or(false),
                    "{rel} has a non-empty description"
                );
                let kb_file_dir = file.parent().unwrap();
                for link in &parsed.links {
                    assert!(!link.reference_style, "{rel}: reference-style link {:?}", link.target);
                    match &link.class {
                        LinkClass::Absolute => {
                            panic!("{rel}: absolute link {:?}", link.target)
                        }
                        LinkClass::Relative { .. } => {
                            let r = resolve_relative(kb_file_dir, package_root, &link.target);
                            assert!(
                                r.resolves,
                                "{rel}: relative link {:?} must resolve inside the package tree",
                                link.target
                            );
                        }
                        LinkClass::External | LinkClass::Anchor => {}
                    }
                }
            }
        }
    }

    // ───────────── M1: the deterministic KB-entry parser ─────────────

    #[test]
    fn parse_frontmatter_present_absent_and_fields() {
        // Frontmatter present: title/description/tags parsed, unknown key ignored
        // (surfaced), body starts after the closing `---`.
        let with = "---\ntitle: Role: planner\ndescription: who plans\ntags: [roles, planning]\nowner: someone\n---\n# Role: planner\nbody line\n";
        let p = parse_kb_entry(with);
        let fm = p.frontmatter.expect("frontmatter present");
        assert_eq!(fm.title.as_deref(), Some("Role: planner"));
        assert_eq!(fm.description.as_deref(), Some("who plans"));
        assert_eq!(fm.tags, vec!["roles", "planning"]);
        assert_eq!(fm.unknown_keys, vec!["owner"], "unknown key surfaced, not fatal");
        assert_eq!(p.body_start_line, 7, "body begins after the closing ---");

        // Frontmatter absent: None, body starts at line 1.
        let without = "# Just a heading\nno frontmatter here\n";
        let p2 = parse_kb_entry(without);
        assert!(p2.frontmatter.is_none());
        assert_eq!(p2.body_start_line, 1);

        // Missing required field: title present, description absent → tags empty.
        let partial = "---\ntitle: Only a title\n---\n# body\n";
        let fm3 = parse_kb_entry(partial).frontmatter.unwrap();
        assert_eq!(fm3.title.as_deref(), Some("Only a title"));
        assert!(fm3.description.is_none(), "missing description is None, not an error");
        assert!(fm3.tags.is_empty(), "absent tags = no tags");

        // An unterminated block is not valid frontmatter.
        let unterminated = "---\ntitle: x\nno closing fence\n";
        assert!(parse_kb_entry(unterminated).frontmatter.is_none());
    }

    #[test]
    fn classify_each_link_class() {
        assert!(matches!(
            classify_link("role-verifier.md"),
            LinkClass::Relative { .. }
        ));
        assert!(matches!(classify_link("../x/y.md"), LinkClass::Relative { .. }));
        assert_eq!(classify_link("/abs/path.md"), LinkClass::Absolute);
        assert_eq!(classify_link("https://example.com"), LinkClass::External);
        assert_eq!(classify_link("mailto:a@b.c"), LinkClass::External);
        assert_eq!(classify_link("#section"), LinkClass::Anchor);
        // A relative target's #fragment is split off the resolved path.
        match classify_link("opus.md#who") {
            LinkClass::Relative { path, fragment } => {
                assert_eq!(path, "opus.md");
                assert_eq!(fragment.as_deref(), Some("who"));
            }
            other => panic!("expected Relative, got {other:?}"),
        }
    }

    #[test]
    fn extract_inline_and_flags_reference_style() {
        let body = "see [role-verifier.md](role-verifier.md) and [the doc][ref] plus [abs](/x.md)\n";
        let links = parse_kb_entry(body).links;
        let inline = links.iter().find(|l| l.target == "role-verifier.md").unwrap();
        assert!(!inline.reference_style);
        assert!(matches!(inline.class, LinkClass::Relative { .. }));
        let refstyle = links.iter().find(|l| l.reference_style).unwrap();
        assert_eq!(refstyle.text, "the doc");
        assert_eq!(refstyle.target, "ref");
        let abs = links.iter().find(|l| l.target == "/x.md").unwrap();
        assert_eq!(abs.class, LinkClass::Absolute);
        // Every link carries a 1-based line number.
        assert!(links.iter().all(|l| l.line == 1));
    }

    #[test]
    fn resolve_relative_in_package_missing_and_escaping() {
        // A scratch package tree: <pkg>/kb/a.md and <pkg>/kb/sub/b.md exist.
        let root = scratch("resolve");
        let pkg = root.dir.join("pkg");
        let kb = pkg.join("kb");
        std::fs::create_dir_all(kb.join("sub")).unwrap();
        std::fs::write(kb.join("a.md"), "x").unwrap();
        std::fs::write(kb.join("sub/b.md"), "y").unwrap();

        // In-package target that exists → resolves true.
        let r = resolve_relative(&kb, &pkg, "a.md");
        assert!(r.resolves, "existing in-package file resolves: {r:?}");
        // Cross-dir but still in-package → resolves true.
        let r2 = resolve_relative(&kb.join("sub"), &pkg, "../a.md");
        assert!(r2.resolves, "../a.md from sub/ is still in-package");
        // Missing file → false.
        assert!(!resolve_relative(&kb, &pkg, "gone.md").resolves);
        // Escaping target (the ../../../../docs/x.md shape) → false, same as missing,
        // EVEN if that path happens to exist on disk (it escapes the package tree).
        let outside = root.dir.join("docs");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.md"), "z").unwrap();
        let escape = resolve_relative(&kb, &pkg, "../docs/x.md");
        assert!(
            !escape.resolves,
            "an existing-but-escaping target must NOT resolve: {escape:?}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn configured_remote_is_pushable() {
        let root = scratch("remote");
        install_kb_pkg(&root, "kb-demo", "Demo KB");
        write(&root, "kb-demo", "topic.md", "content\n").unwrap();
        let pkg_dir = root.packages().join("kb-demo");
        // A bare remote (the remote-backup property, D2): the KB repo pushes.
        let remote = root.dir.join("backup.git");
        git_hardened::run_in(
            &remote.parent().unwrap().to_path_buf(),
            &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
            "init bare",
        )
        .unwrap();
        git_hardened::run_in(
            &pkg_dir,
            &["remote", "add", "backup", remote.to_str().unwrap()],
            "remote add",
        )
        .unwrap();
        let pushed = git_hardened::run_in(&pkg_dir, &["push", "backup", "main"], "push");
        assert!(
            pushed.is_ok(),
            "kb repo must push to a configured remote: {pushed:?}"
        );
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
