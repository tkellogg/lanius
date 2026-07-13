//! Build-time freshening of the embedded web UI.
//!
//! `src/web.rs` embeds `ui/web/dist` into the binary via `include_dir!` at
//! COMPILE time. Without this build script, `cargo build` / `cargo install`
//! never rebuilds `ui/web/dist`, so a UI source change followed by a rebuild
//! silently ships the OLD interface (the "web embed staleness" trap). This
//! script runs the vite build into `ui/web/dist` before the crate compiles, so
//! the embed is always fresh — while keeping the runtime Node-free (Node is
//! only ever a *build-time* convenience; a pre-built dist on disk is the
//! fallback). NOTE: in this git repo `ui/web/dist` is gitignored, so a fresh
//! clone has NO dist — the fallback is real for published tarballs (dist ships
//! via Cargo.toml's include list) and already-built trees, but a no-Node fresh
//! clone cannot build; the fallback paths below say so clearly instead of
//! letting include_dir! fail with an opaque compile error.
//!
//! Decision table (see docs/handoffs/web-embed-freshness.md):
//!   - LANIUS_SKIP_UI_BUILD=1        -> skip npm, warn loudly, succeed.
//!   - ui/web/src present + npm on PATH:
//!       * node_modules missing      -> `npm --prefix ui/web install` (warn);
//!                                       a FAILED install (airgapped / registry
//!                                       down) warns loudly and falls back to
//!                                       the committed dist.
//!       * then `npm --prefix ui/web run build`; a non-zero BUILD exit PANICS
//!         with the npm output (a present-but-broken UI build is a real error;
//!         falling back silently would re-create the staleness bug).
//!   - otherwise (no npm / no ui source) -> warn loudly, embed committed dist.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let web = Path::new(&manifest_dir).join("ui").join("web");

    // Watch the SOURCE tree, never the OUTPUT tree. build.rs writes into
    // ui/web/dist, so a rerun-if-changed on dist would rebuild every time.
    // include_dir! re-reads dist whenever the crate recompiles, and build.rs
    // rerunning (because a watched source changed) is what forces that
    // recompile — so the chain closes without watching dist.
    //
    // Only emit rerun-if-changed for paths that EXIST: cargo re-runs the build
    // script on every build if a watched path is missing, which would defeat
    // the "skip the npm run when nothing changed" requirement.
    let mut watch: Vec<PathBuf> = vec![
        web.join("src"),
        web.join("package.json"),
        web.join("index.html"),
    ];
    // Any vite.config.* (ts, js, mjs, …).
    if let Ok(entries) = std::fs::read_dir(&web) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("vite.config.") {
                watch.push(entry.path());
            }
        }
    }
    for path in &watch {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-env-changed=LANIUS_SKIP_UI_BUILD");

    // Escape hatch: broken local Node, CI oddities, or an intentional
    // fast-rebuild. Embed the committed dist as-is.
    if env::var("LANIUS_SKIP_UI_BUILD").as_deref() == Ok("1") {
        fallback(
            &web,
            "web UI: LANIUS_SKIP_UI_BUILD=1 set — skipping the npm build and \
             embedding the existing ui/web/dist as-is.",
        );
        return;
    }

    // No UI source (published source tarball ships only dist) or no npm on the
    // machine (a plain `cargo install` box) — a pre-built dist is a valid
    // artifact. Warn loudly and move on; the runtime stays Node-free.
    if !web.join("src").is_dir() || !npm_available() {
        fallback(
            &web,
            "web UI: embedding the existing ui/web/dist as-is (npm not found / \
             no ui source); install Node and rebuild to refresh the interface",
        );
        return;
    }

    // Ensure deps. A FAILED install is a FETCH problem (airgapped machine,
    // registry down) — warn and fall back to the committed dist rather than
    // hard-failing `cargo install` on a network hiccup.
    if !web.join("node_modules").is_dir() {
        warn("web UI: node_modules missing — running `npm --prefix ui/web install`");
        let installed = npm()
            .arg("--prefix")
            .arg(&web)
            .arg("install")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !installed {
            fallback(
                &web,
                "web UI: `npm --prefix ui/web install` FAILED (airgapped / registry \
                 down?) — embedding the existing ui/web/dist as-is. Fix networking \
                 and rebuild, or set LANIUS_SKIP_UI_BUILD=1 to skip the UI build.",
            );
            return;
        }
    }

    // Build the SPA into ui/web/dist. A non-zero BUILD exit with deps present is
    // a real error (a broken UI build) — panic with the output. Silently
    // falling back here would re-create the staleness bug with extra steps.
    let output = npm()
        .arg("--prefix")
        .arg(&web)
        .arg("run")
        .arg("build")
        .stdin(Stdio::null())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // Cargo replays this warning even on no-op builds (where the script
            // did not rerun), so keep the wording a provenance statement rather
            // than an action ("rebuilt").
            warn("web UI: embedded ui/web/dist built from source by build.rs (vite build)");
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            panic!(
                "web UI build failed: `npm --prefix ui/web run build` exited {}.\n\
                 --- npm stdout ---\n{stdout}\n--- npm stderr ---\n{stderr}\n\
                 Fix the UI build, or set LANIUS_SKIP_UI_BUILD=1 to skip it and \
                 embed the existing ui/web/dist as-is.",
                out.status
            );
        }
        Err(e) => {
            panic!(
                "web UI build failed: could not run `npm --prefix ui/web run build`: {e}. \
                 Set LANIUS_SKIP_UI_BUILD=1 to skip the UI build and embed the existing \
                 ui/web/dist as-is."
            );
        }
    }
}

/// A fallback path embeds whatever `ui/web/dist` already holds. If there is no
/// dist at all (dist is gitignored — a fresh no-Node clone has none), the crate
/// is GUARANTEED to fail at `include_dir!` with an opaque error; say what is
/// actually wrong instead.
fn fallback(web: &Path, msg: &str) {
    if web.join("dist").join("index.html").is_file() {
        warn(msg);
    } else {
        panic!(
            "the web UI needs Node to build from a fresh clone (ui/web/dist is \
             gitignored and none exists to embed) — install Node, or set \
             LANIUS_SKIP_UI_BUILD=1 only if you have a prebuilt ui/web/dist. \
             (fallback reason: {msg})"
        );
    }
}

/// Emit a loud, always-visible cargo warning (shown even on a successful build).
fn warn(msg: &str) {
    println!("cargo:warning={msg}");
}

/// `npm` on unix; on Windows the npm CLI is a `.cmd` shim that CreateProcess
/// cannot exec by its bare name, so name it explicitly. build.rs compiles for
/// the HOST, so cfg!(windows) is the machine running this script.
fn npm() -> Command {
    Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
}

/// Does `npm` resolve on PATH? (`npm --version` succeeds.)
fn npm_available() -> bool {
    npm()
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
