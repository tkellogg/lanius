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

export function publish(topic: string, payload: unknown, correlation?: string): Promise<boolean> {
  return json<{ ok?: boolean }>('/api/publish', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ topic, payload, correlation }),
  }).then((r) => r.ok === true).catch(() => false);
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
