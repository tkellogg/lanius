use crate::envcompat::EnvDual;
use crate::paths::Root;
use crate::profile;
use crate::packages;
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

/// The same assembly as named parts — the seed of the context pipeline's
/// `system` array (docs/context.md): each profile block, provider output,
/// and the skills inventory is one (name, text) entry, in the order render()
/// joins them. With no stages declared the pipeline's output is these parts
/// joined — byte-identical to render() (the golden parity gate).
pub fn render_parts(
    root: &Root,
    _conn: &Connection,
    profile_name: &str,
    session: &str,
) -> Result<Vec<(String, String)>> {
    let (prof, pdir) = profile::load(root, profile_name)?;
    let mut parts: Vec<(String, String)> = Vec::new();

    // 1. Static blocks with computed-register substitution.
    let blocks_dir = pdir.join("blocks");
    if blocks_dir.exists() {
        let mut files: Vec<_> = std::fs::read_dir(&blocks_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            let raw = std::fs::read_to_string(&f)?;
            let name = f.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            parts.push((name, substitute(&raw, root, profile_name, session, &prof)));
        }
    }

    // 2. Render providers: the read seam of the memory contract. Any
    // discovered, visible package may contribute a block by declaring
    // [[provider]]. (Providers run at render time in the kernel's context —
    // hoisting them behind a grant is open in bus.md; today visibility is
    // profile-scoped, same as v1.)
    for skill in packages::discover(root)? {
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
    let visible: Vec<_> = packages::discover(root)?
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
        block.push_str("\nUse the shell tool to read a SKILL.md and to run any scripts it describes.");
        parts.push(("skills-inventory".into(), block));
    }

    Ok(parts)
}

fn substitute(raw: &str, root: &Root, profile_name: &str, session: &str, prof: &profile::Profile) -> String {
    let mut s = raw.to_string();
    s = s.replace("{{now}}", &trace::now_iso());
    s = s.replace("{{today}}", &chrono::Utc::now().format("%Y-%m-%d").to_string());
    s = s.replace("{{root}}", &root.dir.display().to_string());
    s = s.replace("{{profile}}", profile_name);
    s = s.replace("{{session}}", session);
    for (k, v) in &prof.vars {
        s = s.replace(&format!("{{{{{k}}}}}"), v);
    }
    s
}

fn run_provider(root: &Root, script: &std::path::Path, profile_name: &str, session: &str) -> Result<String> {
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
            json!({ "profile": profile_name, "session": session }).to_string().as_bytes(),
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
            let out = out_h.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
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
