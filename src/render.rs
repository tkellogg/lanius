use crate::context_store;
use crate::envcompat::EnvDual;
use crate::packages;
use crate::paths::Root;
use crate::profile;
use crate::trace;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Context assembly: (blocks + render providers + skills inventory) -> the
/// prompt's system context, per profile. Blocks exist only as a render
/// target, never as a mutable store.
pub fn render(root: &Root, conn: &Connection, profile_name: &str, session: &str) -> Result<String> {
    Ok(render_parts(root, conn, profile_name, session)?
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// The seed of the context pipeline's `user` array. This is intentionally only
/// durable user-placement blocks in this cut: static profile block files,
/// providers, and skills inventory remain system concepts.
pub fn render_user_parts(
    root: &Root,
    conn: &Connection,
    profile_name: &str,
    session: &str,
) -> Result<Vec<(String, String)>> {
    let (prof, _) = profile::load(root, profile_name)?;
    Ok(context_store::load_user_blocks(conn, &prof, session)?
        .into_iter()
        .map(|b| (b.name, b.content))
        .collect())
}

/// The same assembly as named parts — the seed of the context pipeline's
/// `system` array (docs/context.md): each profile block, provider output,
/// and the skills inventory is one (name, text) entry, in the order render()
/// joins them. With no stages declared the pipeline's output is these parts
/// joined — byte-identical to render() (the golden parity gate).
pub fn render_parts(
    root: &Root,
    conn: &Connection,
    profile_name: &str,
    session: &str,
) -> Result<Vec<(String, String)>> {
    let (prof, pdir) = profile::load(root, profile_name)?;
    let mut parts: Vec<(String, String)> = Vec::new();

    // 1. Static + durable system blocks, merged by priority (memory-blocks
    // M1/M2). Static `blocks/*.md` files carry an IMPLICIT priority 0 and sort
    // by filename (the long-standing order); durable `context_blocks` rows
    // (scope agent/global, this owner/agent, bound to no session or this one)
    // carry an EXPLICIT priority, so a block at priority -10 lands before the
    // static blocks and one at +10 after. The renderer stays
    // Doc::system_text() — this only decides seed ORDER, not rendering.
    let blocks_dir = pdir.join("blocks");
    let mut static_files: Vec<std::path::PathBuf> = if blocks_dir.exists() {
        std::fs::read_dir(&blocks_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
            .collect()
    } else {
        Vec::new()
    };
    static_files.sort();

    // M2 "default that evolves": a profile's `blocks/<name>.md` is BOTH the
    // legacy static block AND a seed-once fallback for a durable block of the
    // same stem (e.g. `00-identity.md` -> block `identity`). On first render,
    // if no stored row exists, seed one from the file; thereafter the stored
    // (possibly agent-edited) row wins and the file is no longer rendered for
    // that stem. A file with no matching durable stem renders as before.
    let mut defaults: Vec<(String, String, i32, serde_json::Value)> = Vec::new();
    for f in &static_files {
        if let Some(stem) = block_stem(f) {
            let raw = std::fs::read_to_string(f)?;
            // Optional JSON frontmatter carries a KB pointer block's `meta`
            // (kb-core.md M3: {kb,path,lines,sha}); a plain block file has none.
            let (meta, body) = parse_block_front(&raw);
            let text = substitute(body, root, profile_name, session, &prof);
            defaults.push((stem, text, file_priority(f), meta));
        }
    }
    let seeded = match context_store::seed_defaults(conn, &prof, &defaults, profile_name, session) {
        Ok(s) => s,
        Err(e) => {
            // Best-effort seeding, but a persistent DB failure here would silently
            // render defaults from the file forever (looking like it works) without
            // ever evolving — make it observable instead of masking it as "nothing
            // to seed".
            eprintln!("warning: seeding default memory blocks failed: {e:#}");
            Vec::new()
        }
    };

    // The set of durable block names that now exist for this profile — their
    // stem-matched static files defer to the stored row (stored-wins).
    let durable = context_store::load_system_blocks(conn, &prof, session).unwrap_or_default();
    let durable_names: std::collections::HashSet<String> =
        durable.iter().map(|b| b.name.clone()).collect();

    // Build the merged, priority-ordered system seed. Each entry is
    // (priority, sub_order, name, text); sub_order keeps stable order within a
    // priority (durable rows already sorted by the store; static files keep
    // filename order).
    let mut ordered: Vec<(i32, usize, String, String)> = Vec::new();
    let mut sub = 0usize;
    for f in &static_files {
        let stem = block_stem(f);
        // A static file whose stem is now a durable block defers to that row,
        // UNLESS we just seeded it this very call (then the durable row IS the
        // file's content and we render it via the durable list, not twice).
        if let Some(s) = &stem {
            if durable_names.contains(s) {
                continue;
            }
        }
        let raw = std::fs::read_to_string(f)?;
        let name = f
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // Strip any JSON frontmatter here too — a pointer-block file with no
        // durable counterpart must still render only its body, not the meta.
        let (_, body) = parse_block_front(&raw);
        let text = substitute(body, root, profile_name, session, &prof);
        ordered.push((file_priority(f), sub, name, text));
        sub += 1;
    }
    let _ = seeded; // seeded names already surface via the durable list below
    for b in &durable {
        ordered.push((b.priority, sub, b.name.clone(), b.content.clone()));
        sub += 1;
    }
    ordered.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    for (_, _, name, text) in ordered {
        parts.push((name, text));
    }

    // 2. Render providers: the read seam of the memory contract. Any
    // discovered, visible package may contribute a block by declaring
    // [[provider]]. (Providers run at render time in the kernel's context —
    // hoisting them behind a grant is open in bus.md; today visibility is
    // profile-scoped, same as v1.)
    for skill in packages::discover_for_profile(root, profile_name)? {
        if !profile::skill_visible(&prof, &skill.name) {
            continue;
        }
        let Some(lm) = &skill.manifest else { continue };
        let mut provs: Vec<_> = lm.manifest.provider.iter().collect();
        provs.sort_by_key(|p| p.order);
        for p in provs {
            let script = skill.dir.join(&p.run);
            if !script.exists() {
                continue;
            }
            match run_provider(root, &script, profile_name, session) {
                Ok(out) if !out.trim().is_empty() => {
                    parts.push((
                        format!("provider:{}", skill.name),
                        format!("## {} (provider)\n\n{}", skill.name, out.trim()),
                    ));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[render] provider {} failed: {e:#}", script.display()),
            }
        }
    }

    // 3. Skills inventory: name + description only — progressive disclosure;
    // the agent reads SKILL.md on demand.
    let visible: Vec<_> = packages::discover_for_profile(root, profile_name)?
        .into_iter()
        .filter(|s| s.meta.is_some() && profile::skill_visible(&prof, &s.name))
        .collect();
    if !visible.is_empty() {
        let mut block = String::from("## Skills\n");
        for s in &visible {
            let meta = s.meta.as_ref().unwrap();
            block.push_str(&format!(
                "- **{}** — {} (read {} before first use)\n",
                meta.name,
                meta.description,
                s.dir.join("SKILL.md").display()
            ));
        }
        block.push_str(
            "\nUse the shell tool to read a SKILL.md and to run any scripts it describes.",
        );
        parts.push(("skills-inventory".into(), block));
    }

    Ok(parts)
}

/// Optional JSON frontmatter on a profile block file (kb-core.md M3). A KB
/// pointer block ships its `meta` (`{kb,path,lines,sha}`) as a JSON object between
/// a leading `---` line and a closing `---` line; the rest is the concept summary
/// (the block content). Returns `(meta, body)`. A file that does NOT open with a
/// `---` delimiter whose fence encloses a valid JSON object is treated as pure
/// content (empty meta) — so every existing plain block file is unchanged.
fn parse_block_front(raw: &str) -> (serde_json::Value, &str) {
    let empty = serde_json::Value::Object(Default::default());
    // Must start with a `---` line (allow a leading BOM-free exact match).
    let rest = match raw.strip_prefix("---\n") {
        Some(r) => r,
        None => match raw.strip_prefix("---\r\n") {
            Some(r) => r,
            None => return (empty, raw),
        },
    };
    // Find the closing `---` line and split there.
    let mut idx = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let front = &rest[..idx];
            let body = &rest[idx + line.len()..];
            // Strip one leading newline from the body for a clean summary.
            let body = body.strip_prefix('\n').unwrap_or(body);
            match serde_json::from_str::<serde_json::Value>(front) {
                Ok(v) if v.is_object() => return (v, body),
                // Malformed/non-object frontmatter: treat the whole file as
                // content rather than silently dropping it.
                _ => return (empty, raw),
            }
        }
        idx += line.len();
    }
    (empty, raw)
}

/// The durable block name a static `blocks/<file>.md` seeds: the file stem with
/// any leading `NN-` numeric prefix stripped (`00-identity.md` -> `identity`,
/// `ctx.md` -> `ctx`). Returns None if the resulting name is not a valid block
/// name (whitespace/slash/empty) — that file is a pure static block with no
/// durable counterpart.
fn block_stem(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    let trimmed = match stem.split_once('-') {
        Some((prefix, rest))
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) =>
        {
            rest.to_string()
        }
        _ => stem.to_string(),
    };
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains(char::is_whitespace) {
        return None;
    }
    Some(trimmed)
}

/// The implicit priority of a static block file: its leading `NN-` numeric
/// prefix if present (so `10-ctx.md` -> 10), else 0. This makes the historical
/// filename sort equal the priority sort, so a durable block with an explicit
/// priority slots into the same order relative to the static blocks.
fn file_priority(path: &std::path::Path) -> i32 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split_once('-'))
        .filter(|(p, _)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .and_then(|(p, _)| p.parse::<i32>().ok())
        .unwrap_or(0)
}

fn substitute(
    raw: &str,
    root: &Root,
    profile_name: &str,
    session: &str,
    prof: &profile::Profile,
) -> String {
    let mut s = raw.to_string();
    s = s.replace("{{now}}", &trace::now_iso());
    s = s.replace(
        "{{today}}",
        &chrono::Utc::now().format("%Y-%m-%d").to_string(),
    );
    s = s.replace("{{root}}", &root.dir.display().to_string());
    s = s.replace("{{profile}}", profile_name);
    s = s.replace("{{session}}", session);
    for (k, v) in &prof.vars {
        s = s.replace(&format!("{{{{{k}}}}}"), v);
    }
    s
}

fn run_provider(
    root: &Root,
    script: &std::path::Path,
    profile_name: &str,
    session: &str,
) -> Result<String> {
    let mut child = Command::new(script)
        .current_dir(&root.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_dual("ROOT", &root.dir)
        .env_dual("DB", root.db())
        .env_dual("TRACE", root.trace_file())
        .env_dual("PROFILE", root.profile_dir(profile_name))
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(
            json!({ "profile": profile_name, "session": session })
                .to_string()
                .as_bytes(),
        );
    }
    // Drain stdout concurrently: a provider writing more than the pipe buffer
    // would otherwise block forever. Providers run at prompt-render time; a
    // hung one must not hang the exec.
    let out_h = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        })
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait()?.is_some() {
            let out = out_h
                .map(|h| h.join().unwrap_or_default())
                .unwrap_or_default();
            return Ok(trace::clip(&out, 16_000));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait(); // reap; no zombies
            anyhow::bail!("provider timed out after 10s");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_blocks::{ContextBlock, Placement, Scope};
    use crate::context_store;

    fn scratch(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("el-render-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("profiles/default/blocks")).unwrap();
        std::fs::write(
            dir.join("profiles/default/profile.toml"),
            "agent = \"lily\"\nowner = \"owner\"\n",
        )
        .unwrap();
        Root { dir }
    }

    fn names(root: &Root, conn: &Connection, session: &str) -> Vec<String> {
        render_parts(root, conn, "default", session)
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn parse_block_front_reads_json_meta() {
        // A pointer block file: JSON frontmatter meta + a body.
        let raw = "---\n{ \"kb\": \"kb-llm-strengths\", \"path\": \"kb/role-verifier.md\" }\n---\nsummary body\n";
        let (meta, body) = parse_block_front(raw);
        assert_eq!(meta["kb"], "kb-llm-strengths");
        assert_eq!(meta["path"], "kb/role-verifier.md");
        assert_eq!(body, "summary body\n");
        // A plain file with no frontmatter is unchanged (empty meta).
        let plain = "just content\nmore\n";
        let (m2, b2) = parse_block_front(plain);
        assert!(m2.as_object().unwrap().is_empty());
        assert_eq!(b2, plain);
        // A markdown file that merely starts with a `---` rule (no closing fence
        // enclosing JSON) stays pure content.
        let rule = "---\nnot json\nstill text\n";
        let (m3, b3) = parse_block_front(rule);
        assert!(m3.as_object().unwrap().is_empty());
        assert_eq!(b3, rule);
    }

    // M3 acceptance: a profile blocks/ file with JSON frontmatter seeds a durable
    // pointer block whose content renders AND whose meta resolves to file/line/sha.
    #[test]
    fn m3_pointer_block_seeds_with_meta() {
        let root = scratch("m3");
        std::fs::write(
            root.dir.join("profiles/default/blocks/10-kb-ptr.md"),
            "---\n{ \"kb\": \"kb-llm-strengths\", \"path\": \"kb/role-verifier.md\", \"lines\": \"1-26\", \"sha\": \"deadbeef\" }\n---\nPick the model by role.\n",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();
        // Render seeds + shows the block content.
        let text = render(&root, &conn, "default", "s1").unwrap();
        assert!(text.contains("Pick the model by role."));
        // The meta resolves to machine-readable fields.
        let mut b = ContextBlock::new("kb-ptr", "", "lily");
        b.scope = Scope::Agent;
        let meta = context_store::get_block_meta(&conn, &b, "s1", None)
            .unwrap()
            .expect("pointer block exists");
        assert_eq!(meta["kb"], "kb-llm-strengths");
        assert_eq!(meta["path"], "kb/role-verifier.md");
        assert_eq!(meta["lines"], "1-26");
        assert_eq!(meta["sha"], "deadbeef");
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn block_stem_strips_numeric_prefix() {
        assert_eq!(
            block_stem(std::path::Path::new("00-identity.md")).as_deref(),
            Some("identity")
        );
        assert_eq!(
            block_stem(std::path::Path::new("ctx.md")).as_deref(),
            Some("ctx")
        );
        // A non-numeric prefix is kept whole (it is part of the name).
        assert_eq!(
            block_stem(std::path::Path::new("agent-card.md")).as_deref(),
            Some("agent-card")
        );
        assert_eq!(file_priority(std::path::Path::new("10-ctx.md")), 10);
        assert_eq!(file_priority(std::path::Path::new("ctx.md")), 0);
    }

    // M1 acceptance: a durable context_blocks row (scope=agent, owner=<agent>,
    // name=identity, placement=system) shows in the rendered system text,
    // positioned by priority relative to the profile's static blocks.
    #[test]
    fn m1_durable_block_seeds_system_by_priority() {
        let root = scratch("m1");
        std::fs::write(
            root.dir.join("profiles/default/blocks/50-body.md"),
            "Body block.",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();

        // A block at priority -10 must land BEFORE the static block (file
        // priority 50); a block at +99 must land after.
        for (name, content, prio) in [("identity", "I am Lily.", -10), ("footer", "bye", 99)] {
            let mut b = ContextBlock::new(name, content, "lily");
            b.scope = Scope::Agent;
            b.priority = prio;
            context_store::upsert_block(&conn, "default", &b, "s1", None).unwrap();
        }
        let ns = names(&root, &conn, "s1");
        let id = ns.iter().position(|n| n == "identity").unwrap();
        let body = ns.iter().position(|n| n == "body").unwrap();
        let foot = ns.iter().position(|n| n == "footer").unwrap();
        assert!(id < body, "priority -10 block precedes the static block");
        assert!(body < foot, "priority +99 block follows the static block");

        let text = render(&root, &conn, "default", "s1").unwrap();
        assert!(text.contains("I am Lily."));
        assert!(text.contains("Body block."));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    #[test]
    fn user_parts_load_only_durable_user_blocks() {
        let root = scratch("user-parts");
        std::fs::write(
            root.dir.join("profiles/default/blocks/50-body.md"),
            "Body block.",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();

        let mut user = ContextBlock::new("scratch", "hot notes", "lily");
        user.scope = Scope::Agent;
        user.placement = Placement::User;
        context_store::upsert_block(&conn, "default", &user, "s1", None).unwrap();

        let mut system = ContextBlock::new("identity", "stable system", "lily");
        system.scope = Scope::Agent;
        system.placement = Placement::System;
        context_store::upsert_block(&conn, "default", &system, "s1", None).unwrap();

        let user_parts = render_user_parts(&root, &conn, "default", "s1").unwrap();
        assert_eq!(user_parts, vec![("scratch".into(), "hot notes".into())]);

        let system_text = render(&root, &conn, "default", "s1").unwrap();
        assert!(system_text.contains("stable system"));
        assert!(system_text.contains("Body block."));
        assert!(!system_text.contains("hot notes"));
        std::fs::remove_dir_all(&root.dir).ok();
    }

    // M2 acceptance: a shipped default (blocks/<name>.md) seeds a row on first
    // render; a subsequent `set` overrides it AND survives a re-render (it
    // evolved).
    #[test]
    fn m2_default_seeds_then_evolves() {
        let root = scratch("m2");
        std::fs::write(
            root.dir.join("profiles/default/blocks/00-identity.md"),
            "default identity",
        )
        .unwrap();
        let conn = crate::db::open(&root).unwrap();
        crate::db::init_schema(&conn).unwrap();

        // First render seeds the default into the store and shows it.
        let t1 = render(&root, &conn, "default", "s1").unwrap();
        assert!(t1.contains("default identity"));
        let mut b = ContextBlock::new("identity", "x", "lily");
        b.scope = Scope::Agent;
        assert!(
            context_store::get_block(&conn, &b, "s1", None)
                .unwrap()
                .is_some(),
            "the default seeded a durable row"
        );

        // A `set` wins over the default.
        b.content = "evolved identity".into();
        context_store::upsert_block(&conn, "default", &b, "s1", None).unwrap();
        let t2 = render(&root, &conn, "default", "s1").unwrap();
        assert!(t2.contains("evolved identity"));
        assert!(
            !t2.contains("default identity"),
            "the file no longer wins once the row evolved"
        );

        // And it survives a re-render — the default never overwrites.
        let t3 = render(&root, &conn, "default", "s1").unwrap();
        assert!(t3.contains("evolved identity"));
        assert!(!t3.contains("default identity"));
        std::fs::remove_dir_all(&root.dir).ok();
    }
}
