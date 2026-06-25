use crate::paths::Root;
use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn request_shutdown(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

pub fn run(root: &Root, interval_ms: u64, web_port: u16, vite_port: u16) -> Result<()> {
    install_signal_handlers();

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = repo.join("ui/web");
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"));
    let target_debug = repo.join("target/debug");
    let log_path = repo.join("target/elanus-dev.log");
    let log = Log::create(&log_path)?;
    let path = prepend_path(&target_debug)?;
    let root_s = root.dir.display().to_string();
    let web_port_s = web_port.to_string();
    let vite_port_s = vite_port.to_string();
    let backend = format!("http://127.0.0.1:{web_port}");

    let mut services = vec![
        Service::new(
            "daemon",
            CommandSpec::new(cargo.clone(), &repo)
                .arg("run")
                .arg("--quiet")
                .arg("--")
                .arg("daemon")
                .arg("--interval-ms")
                .arg(interval_ms.to_string())
                .env("ELANUS_ROOT", &root_s)
                .env("PATH", &path),
        )
        .with_watch(RustInputs::new(&repo)?),
        // The web relay is the same Rust server `serve` ships (`elanus web`,
        // src/web.rs), built via `cargo run` so a change to src/web.rs hot-restarts
        // it in the dev loop. ui/web/server.mjs is kept on disk as a fallback (M4
        // — retiring it — is DEFERRED) but is no longer wired into dev/serve. Vite
        // still serves the SPA with HMR and proxies /api here (ELANUS_WEB_BACKEND).
        Service::new(
            "web",
            CommandSpec::new(cargo, &repo)
                .arg("run")
                .arg("--quiet")
                .arg("--")
                .arg("-C")
                .arg(&root_s)
                .arg("web")
                .arg("--port")
                .arg(&web_port_s)
                .env("ELANUS_ROOT", &root_s)
                .env("ELANUS_WEB_PORT", &web_port_s)
                .env("PATH", &path),
        )
        .with_watch(RustInputs::new(&repo)?),
        Service::new(
            "vite",
            CommandSpec::new("npm", &web_dir)
                .arg("run")
                .arg("dev")
                .env("ELANUS_ROOT", &root_s)
                .env("ELANUS_WEB_BACKEND", &backend)
                .env("ELANUS_VITE_PORT", &vite_port_s)
                .env("PATH", &path),
        ),
    ];

    log.line(format!("[dev] root={}", root.dir.display()));
    log.line(format!("[dev] log={}", log_path.display()));
    log.line(format!("[dev] web relay: http://127.0.0.1:{web_port}"));
    log.line(format!("[dev] vite UI:   http://127.0.0.1:{vite_port}"));
    log.line("[dev] ctrl-c stops the whole stack");

    for service in &mut services {
        service.start(&log)?;
    }

    while !SHUTDOWN.load(Ordering::SeqCst) {
        let now = Instant::now();
        for service in &mut services {
            if service.watch_changed()? {
                log.line(format!("[dev] {} inputs changed; restarting", service.name));
                service.stop(Duration::from_secs(3));
                service.next_start = Some(Instant::now() + Duration::from_millis(250));
            }

            if let Some(status) = service.try_wait()? {
                log.line(format!(
                    "[dev] {} exited ({status}); restarting",
                    service.name
                ));
                service.next_start = Some(Instant::now() + service.restart_delay(status));
            }

            if service.child.is_none() && service.next_start.is_some_and(|t| now >= t) {
                service.start(&log)?;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    log.line("[dev] shutting down");
    for service in services.iter_mut().rev() {
        service.stop(Duration::from_secs(3));
    }
    cleanup_root_processes(root, &log);
    Ok(())
}

/// `elanus serve` — the PACKAGED counterpart of `elanus dev`. Where `dev`
/// supervises three DEV services (a `cargo run` debug daemon, `node server.mjs
/// --watch`, and the Vite dev server), `serve` supervises the PROD stack with no
/// dev toolchain:
///
/// - **The daemon** is the CURRENTLY RUNNING binary (`current_exe`), re-invoked as
///   `<self> -C <root> daemon --interval-ms <n>`. `serve` is itself launched from a
///   built binary, so there is no `cargo` and nothing to compile — just run it.
/// - **The web server** is THIS binary again, `<self> -C <root> web --port
///   <web-port>` (src/web.rs): an in-process ntex server serving the SPA that is
///   **embedded in the binary** (`include_dir!` over ui/web/dist). No Node, no npm,
///   no `ui/web` source tree at runtime — the whole point of the packaging work
///   (docs/handoffs/web-packaging.md). `elanus dev` keeps Vite + npm for hot reload.
///   (ui/web/server.mjs stays on disk as a fallback; M4 — retiring it — is DEFERRED.)
///
/// Supervision (signals, restart-with-backoff, combined logging, group teardown,
/// root-scoped cleanup) reuses the same Service/CommandSpec/Log machinery as `dev`.
/// There is no file-watch and no restart-on-source-change: a packaged service runs
/// the artifact as-is. Everything is rooted at `<root>` — no build tree
/// (CARGO_MANIFEST_DIR) is consulted, so an installed binary with no checkout works.
pub fn serve(root: &Root, interval_ms: u64, web_port: u16, rebuild: bool) -> Result<()> {
    install_signal_handlers();

    // Everything is rooted at <root>: no build tree is consulted, so an installed
    // binary with no checkout serves fine. The web UI is embedded in the binary
    // (src/web.rs), so there is no dist/ to locate or build.
    let log_path = root.dir.join("elanus-serve.log");
    let log = Log::create(&log_path)?;
    let root_s = root.dir.display().to_string();
    let web_port_s = web_port.to_string();

    // The packaged daemon is THIS binary re-invoked (not `cargo run`): serve is
    // launched from a built binary, so current_exe IS the elanus binary. Run it as
    // `<self> -C <root> daemon` so the daemon targets the same root explicitly.
    let self_exe = std::env::current_exe().context("locating the running elanus binary")?;

    if rebuild {
        log.line("[serve] --rebuild ignored: the web UI is embedded in the binary (nothing to npm-build at serve time; use `elanus dev` for the Vite hot-reload loop)");
    }

    let mut services = vec![
        Service::new(
            "daemon",
            CommandSpec::new(self_exe.clone(), &root.dir)
                .arg("-C")
                .arg(&root_s)
                .arg("daemon")
                .arg("--interval-ms")
                .arg(interval_ms.to_string())
                .env("ELANUS_ROOT", &root_s),
        ),
        // The web server is THIS binary again (`elanus web`, src/web.rs): the SPA
        // is embedded via include_dir!, so no Node, no npm, no ui/web checkout is
        // needed at runtime. ui/web/server.mjs remains on disk as a fallback only
        // (M4 deferred).
        Service::new(
            "web",
            CommandSpec::new(self_exe.clone(), &root.dir)
                .arg("-C")
                .arg(&root_s)
                .arg("web")
                .arg("--port")
                .arg(&web_port_s)
                .env("ELANUS_ROOT", &root_s)
                .env("ELANUS_WEB_PORT", &web_port_s),
        ),
    ];

    log.line(format!("[serve] root={}", root.dir.display()));
    log.line(format!("[serve] log={}", log_path.display()));
    log.line(format!("[serve] daemon={}", self_exe.display()));
    log.line(format!(
        "[serve] web UI: http://127.0.0.1:{web_port} (embedded SPA, served in-process)"
    ));
    log.line("[serve] ctrl-c stops the whole stack");

    for service in &mut services {
        service.start(&log)?;
    }

    while !SHUTDOWN.load(Ordering::SeqCst) {
        let now = Instant::now();
        for service in &mut services {
            if let Some(status) = service.try_wait()? {
                log.line(format!(
                    "[serve] {} exited ({status}); restarting",
                    service.name
                ));
                service.next_start = Some(Instant::now() + service.restart_delay(status));
            }

            if service.child.is_none() && service.next_start.is_some_and(|t| now >= t) {
                service.start(&log)?;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    log.line("[serve] shutting down");
    for service in services.iter_mut().rev() {
        service.stop(Duration::from_secs(3));
    }
    cleanup_root_processes(root, &log);
    Ok(())
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            request_shutdown as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            request_shutdown as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            request_shutdown as *const () as libc::sighandler_t,
        );
    }
}

#[derive(Clone)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

impl CommandSpec {
    fn new(program: impl Into<PathBuf>, cwd: &Path) -> CommandSpec {
        CommandSpec {
            program: program.into(),
            args: vec![],
            cwd: cwd.to_path_buf(),
            env: vec![],
        }
    }

    fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
        self
    }

    fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn spawn(&self, name: &str, log: &Log) -> Result<RunningChild> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        set_process_group(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {name}: {}", render_command(self)))?;
        let mut threads = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            threads.push(tee_stream(stdout, StreamKind::Stdout, log.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            threads.push(tee_stream(stderr, StreamKind::Stderr, log.clone()));
        }
        Ok(RunningChild { child, threads })
    }
}

struct RunningChild {
    child: Child,
    threads: Vec<JoinHandle<()>>,
}

impl RunningChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn wait(mut self) -> std::io::Result<ExitStatus> {
        let status = self.child.wait();
        self.join_threads();
        status
    }

    fn join_threads(&mut self) {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

struct Service {
    name: &'static str,
    command: CommandSpec,
    child: Option<RunningChild>,
    started_at: Option<Instant>,
    next_start: Option<Instant>,
    watch: Option<RustInputs>,
}

impl Service {
    fn new(name: &'static str, command: CommandSpec) -> Service {
        Service {
            name,
            command,
            child: None,
            started_at: None,
            next_start: None,
            watch: None,
        }
    }

    fn with_watch(mut self, watch: RustInputs) -> Service {
        self.watch = Some(watch);
        self
    }

    fn start(&mut self, log: &Log) -> Result<()> {
        log.line(format!("[dev] starting {}", self.name));
        self.child = Some(self.command.spawn(self.name, log)?);
        self.started_at = Some(Instant::now());
        self.next_start = None;
        Ok(())
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        match child.try_wait()? {
            Some(status) => {
                child.join_threads();
                self.child = None;
                Ok(Some(status))
            }
            None => Ok(None),
        }
    }

    fn watch_changed(&mut self) -> Result<bool> {
        let Some(watch) = self.watch.as_mut() else {
            return Ok(false);
        };
        watch.changed()
    }

    fn restart_delay(&self, status: ExitStatus) -> Duration {
        if status.success() {
            return Duration::from_millis(500);
        }
        let stable = self
            .started_at
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(5));
        if stable {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(3)
        }
    }

    fn stop(&mut self, grace: Duration) {
        let Some(child) = self.child.take() else {
            return;
        };
        let mut child = child;
        terminate_child_group(&child);
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    child.join_threads();
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(None) => {
                    kill_child_group(&child);
                    let _ = child.wait();
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

struct RustInputs {
    repo: PathBuf,
    last: u64,
}

impl RustInputs {
    fn new(repo: &Path) -> Result<RustInputs> {
        Ok(RustInputs {
            repo: repo.to_path_buf(),
            last: rust_inputs_fingerprint(repo)?,
        })
    }

    fn changed(&mut self) -> Result<bool> {
        let next = rust_inputs_fingerprint(&self.repo)?;
        if next == self.last {
            return Ok(false);
        }
        self.last = next;
        Ok(true)
    }
}

fn rust_inputs_fingerprint(repo: &Path) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    hash_file(repo.join("Cargo.toml"), repo, &mut hasher)?;
    hash_file(repo.join("Cargo.lock"), repo, &mut hasher)?;
    hash_rust_dir(&repo.join("src"), repo, &mut hasher)?;
    Ok(hasher.finish())
}

fn hash_rust_dir(dir: &Path, repo: &Path, hasher: &mut DefaultHasher) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            hash_rust_dir(&path, repo, hasher)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            hash_file(path, repo, hasher)?;
        }
    }
    Ok(())
}

fn hash_file(path: PathBuf, repo: &Path, hasher: &mut DefaultHasher) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let rel = path.strip_prefix(repo).unwrap_or(&path);
    rel.hash(hasher);
    let meta = std::fs::metadata(&path)?;
    meta.len().hash(hasher);
    if let Ok(modified) = meta.modified() {
        modified.hash(hasher);
    }
    Ok(())
}

fn prepend_path(dir: &Path) -> Result<String> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths)
        .context("building PATH")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("PATH contains non-UTF-8 data"))
}

fn render_command(spec: &CommandSpec) -> String {
    let mut parts = vec![spec.program.display().to_string()];
    parts.extend(spec.args.clone());
    parts.join(" ")
}

#[derive(Clone)]
struct Log {
    file: Arc<Mutex<std::fs::File>>,
}

impl Log {
    fn create(path: &Path) -> Result<Log> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Log {
            file: Arc::new(Mutex::new(file)),
        })
    }

    fn line(&self, msg: impl AsRef<str>) {
        let line = msg.as_ref();
        eprintln!("{line}");
        self.write_bytes(line.as_bytes());
        self.write_bytes(b"\n");
    }

    fn write_bytes(&self, bytes: &[u8]) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
    }
}

enum StreamKind {
    Stdout,
    Stderr,
}

fn tee_stream<R>(mut stream: R, kind: StreamKind, log: Log) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            let Ok(n) = stream.read(&mut buf) else {
                return;
            };
            if n == 0 {
                return;
            }
            let bytes = &buf[..n];
            match kind {
                StreamKind::Stdout => {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(bytes);
                    let _ = out.flush();
                }
                StreamKind::Stderr => {
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(bytes);
                    let _ = err.flush();
                }
            }
            log.write_bytes(bytes);
        }
    })
}

#[cfg(unix)]
fn cleanup_root_processes(root: &Root, log: &Log) {
    let needle = root.dir.to_string_lossy();
    if needle.is_empty() {
        return;
    }
    let pids = root_processes(&needle);
    if pids.is_empty() {
        return;
    }
    log.line(format!(
        "[dev] stopping {} root-scoped child process(es)",
        pids.len()
    ));
    signal_pids(&pids, libc::SIGTERM);
    std::thread::sleep(Duration::from_secs(1));
    let remaining = root_processes(&needle);
    if !remaining.is_empty() {
        signal_pids(&remaining, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn cleanup_root_processes(_: &Root, _: &Log) {}

#[cfg(unix)]
fn root_processes(needle: &str) -> Vec<i32> {
    let self_pid = std::process::id() as i32;
    let parent_pid = unsafe { libc::getppid() };
    let Ok(out) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid_s, command) = trimmed.split_once(char::is_whitespace)?;
            let pid = pid_s.parse::<i32>().ok()?;
            if pid == self_pid || pid == parent_pid || !command.contains(needle) {
                return None;
            }
            Some(pid)
        })
        .collect()
}

#[cfg(unix)]
fn signal_pids(pids: &[i32], signal: libc::c_int) {
    for pid in pids {
        unsafe {
            if libc::kill(-*pid, signal) == -1 {
                libc::kill(*pid, signal);
            }
        }
    }
}

#[cfg(unix)]
fn set_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn set_process_group(_: &mut Command) {}

#[cfg(unix)]
fn terminate_child_group(child: &RunningChild) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate_child_group(child: &RunningChild) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
}

#[cfg(unix)]
fn kill_child_group(child: &RunningChild) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_child_group(child: &RunningChild) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
}
