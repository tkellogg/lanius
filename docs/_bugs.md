I open my laptop, UI doesn't reconnect to MQTT. An error in the console says that connection to /api/stream was interrupted.

FIXED in `b40dc7a` (SSE ping keepalive + client-side watchdog/backoff reopen, ui/web/src/live.ts + src/web.rs; regression: ui/web/test/sse-reconnect.mjs).



