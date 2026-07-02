//! `elanus kb list` / `elanus kb write` (docs/handoffs/kb-core.md). The CLI is the
//! API, the way `elanus config` and `elanus block` are: a harness shells out to it.
//! `list` names the enabled knowledge bases (packages carrying the `[kb]` marker);
//! `write` is the convenience verb that writes a file into a KB's `kb/` tree and
//! commits it atomically with the shared hardened-git discipline. The taught
//! pattern (so a default agent "just knows" how to write knowledge) lives in the
//! stdlib `knowledge` skill; this verb is the sharp edge behind it.

use crate::kb;
use crate::paths::Root;
use anyhow::Result;
use serde_json::json;

/// `elanus kb list [--json]`: the enabled KBs visible to `profile`, human table
/// or one JSON line each (for a harness to consume).
pub fn list(root: &Root, profile: &str, json_out: bool) -> Result<()> {
    let kbs = kb::enumerate(root, profile)?;
    if json_out {
        for k in &kbs {
            println!(
                "{}",
                json!({
                    "package": k.package,
                    "title": k.title,
                    "description": k.description,
                    "files": k.files,
                    "path": k.path.display().to_string(),
                })
            );
        }
        return Ok(());
    }
    if kbs.is_empty() {
        println!("no knowledge bases enabled (a package declares one with a [kb] marker)");
        return Ok(());
    }
    for k in &kbs {
        let title = k.title.as_deref().unwrap_or("(untitled)");
        println!(
            "{}  {}  [{} file{}]  {}",
            k.package,
            title,
            k.files,
            if k.files == 1 { "" } else { "s" },
            k.path.display()
        );
    }
    Ok(())
}

/// `elanus kb write <pkg> <path>`: write stdin (or `--content`) into `kb/<path>`
/// and commit it. Write-then-commit is atomic with the KB's hardened git.
pub fn write(root: &Root, pkg: &str, path: &str, content: &str) -> Result<()> {
    let out = kb::write(root, pkg, path, content)?;
    if out.changed {
        println!(
            "wrote {} in {} — committed {}",
            out.rel,
            out.package,
            short(&out.commit)
        );
    } else {
        println!("{}/{} unchanged — no commit", out.package, out.rel);
    }
    Ok(())
}

fn short(sha: &str) -> String {
    sha.chars().take(10).collect()
}
