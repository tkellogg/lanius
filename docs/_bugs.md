I open my laptop, UI doesn't reconnect to MQTT. An error in the console says that connection to /api/stream was interrupted.

FIXED in `b40dc7a` (SSE ping keepalive + client-side watchdog/backoff reopen, ui/web/src/live.ts + src/web.rs; regression: ui/web/test/sse-reconnect.mjs).



Walkthrough 2026-07-08: routing "missing" from the web UI even though the merge (31e7547) landed the night before. Root cause: `ui/web/dist` was last built Jul 7 23:08, before the merge; `cargo build` at Jul 8 20:35 embedded the stale bundle (installed binary has zero `pushState`, source has it at App.tsx:681). Second time this class has bitten (first: web-ui-polish). Real fix is build-system, not discipline: make build.rs emit `cargo:rerun-if-changed=ui/web/dist` (or hash the dist into OUT_DIR) so a dist change forces re-embed — and ideally have `cargo build` warn when dist is older than ui/web/src. NOTE: the rest of the walkthrough (history pane unreachable, chat dead air, helper spawn-on-tab-open) was observed against this stale SPA and needs re-verification after `npm run build && touch src/web.rs && cargo build --release`.
