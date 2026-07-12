export type ApiResult = { ok?: boolean; error?: string; [key: string]: any };

async function json<T = ApiResult>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  const body = await res.json().catch(() => ({}));
  return body as T;
}

export function adminGet<T = ApiResult>(path: string): Promise<T> {
  return json<T>(`/api/admin/${path}`).catch(() => ({ ok: false, error: 'server unreachable' }) as T);
}

export function adminPost<T = ApiResult>(path: string, body: unknown): Promise<T> {
  return json<T>(`/api/admin/${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  }).catch(() => ({ ok: false, error: 'server unreachable' }) as T);
}

export function adminPut<T = ApiResult>(path: string, body: unknown): Promise<T> {
  return json<T>(`/api/admin/${path}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  }).catch(() => ({ ok: false, error: 'server unreachable' }) as T);
}

export function status<T = ApiResult>(): Promise<T> {
  return json<T>('/api/status').catch(() => ({ ok: false, error: 'server unreachable' }) as T);
}

// UI-truthfulness M1: latest retained liveness per capability (running/stopped/
// failed), keyed by package name. Capabilities with no status are absent (the UI
// renders those as "not started").
export function liveness<T = ApiResult>(): Promise<T> {
  return json<T>('/api/liveness').catch(() => ({ ok: false, actors: {} }) as T);
}

export function publish(topic: string, payload: unknown, correlation?: string): Promise<boolean> {
  return json<{ ok?: boolean }>('/api/publish', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ topic, payload, correlation }),
  }).then((r) => r.ok === true).catch(() => false);
}

// worker-dm unification M2: relay a chat reply in a coding-session DM thread
// into the worker's inbox via the deliver path (owner-as-requester; routes NO
// completion reply — the worker answers via its own in/human send). Mirrors the
// WorkerNoteCompose POST, so a worker-thread compose and a runs-panel note hit
// the same server seam. Returns the relay's honest verdict ({ delivered, error }).
export function codeDeliver(session: string, message: string): Promise<ApiResult> {
  return json<ApiResult>('/api/code/deliver', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ session, message }),
  }).catch(() => ({ ok: false, delivered: false, error: 'server unreachable' }));
}

export async function history(params: Record<string, string | number | undefined>): Promise<ApiResult | null> {
  try {
    const qs = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (value != null) qs.set(key, String(value));
    }
    const res = await fetch(`/api/history?${qs}`);
    const body = await res.json().catch(() => null);
    if (res.status === 503 || res.status === 504) return { ok: false, unavailable: true, ...(body ?? {}) };
    if (!res.ok) return body ?? { ok: false };
    return body;
  } catch {
    return null;
  }
}
