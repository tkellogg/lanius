export type LiveStatus = {
  kind: 'status';
  connected?: boolean;
  broker?: string;
  agent?: string;
};

export type LiveMessage = {
  kind: 'message';
  // Server-side monotonic sequence number, stable across ring-buffer replay on
  // reconnect (the Rust `elanus web` server sets it on every formed message).
  // Lets consumers apply each delivery at most once even under at-least-once /
  // replayed SSE delivery.
  seq?: number;
  topic: string;
  env: any;
};

export type LiveEvent = LiveStatus | LiveMessage;
export type LiveStream = { close: () => void };

type LiveStreamOptions = {
  url?: string;
  retryMs?: number;
  maxRetryMs?: number;
  staleMs?: number;
  watchdogMs?: number;
};

const DEFAULT_RETRY_MS = 1000;
const DEFAULT_MAX_RETRY_MS = 10000;
const DEFAULT_STALE_MS = 45000;
const DEFAULT_WATCHDOG_MS = 5000;

export function openLiveStream(onEvent: (event: LiveEvent) => void, onError: () => void, opts: LiveStreamOptions = {}): LiveStream {
  const url = opts.url ?? '/api/stream';
  const baseRetryMs = opts.retryMs ?? DEFAULT_RETRY_MS;
  const maxRetryMs = opts.maxRetryMs ?? DEFAULT_MAX_RETRY_MS;
  const staleMs = opts.staleMs ?? DEFAULT_STALE_MS;
  const watchdogMs = opts.watchdogMs ?? DEFAULT_WATCHDOG_MS;
  let closed = false;
  let stream: EventSource | null = null;
  let retryMs = baseRetryMs;
  let lastSeen = Date.now();
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  const markSeen = () => {
    lastSeen = Date.now();
    retryMs = baseRetryMs;
  };

  const clearReconnect = () => {
    if (reconnectTimer) clearTimeout(reconnectTimer);
    reconnectTimer = null;
  };

  const closeCurrent = () => {
    if (!stream) return;
    stream.onopen = null;
    stream.onmessage = null;
    stream.onerror = null;
    stream.close();
    stream = null;
  };

  const connect = () => {
    if (closed) return;
    closeCurrent();
    const next = new EventSource(url);
    stream = next;
    next.onopen = markSeen;
    next.addEventListener('ping', markSeen);
    next.onmessage = (event) => {
      markSeen();
      try {
        onEvent(JSON.parse(event.data));
      } catch {
        /* ignore malformed stream payloads */
      }
    };
    next.onerror = () => {
      onError();
      closeCurrent();
      scheduleReconnect();
    };
  };

  const scheduleReconnect = (immediate = false) => {
    if (closed || reconnectTimer) return;
    const delay = immediate ? 0 : retryMs;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (closed) return;
      connect();
      retryMs = Math.min(retryMs * 2, maxRetryMs);
    }, delay);
  };

  const watchdog = setInterval(() => {
    if (closed || Date.now() - lastSeen <= staleMs) return;
    onError();
    closeCurrent();
    scheduleReconnect(true);
  }, watchdogMs);

  connect();

  return {
    close: () => {
      closed = true;
      clearReconnect();
      clearInterval(watchdog);
      closeCurrent();
    },
  };
}
