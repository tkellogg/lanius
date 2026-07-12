import { uid } from './format';

export const agentOf = (topic: string) => topic.match(/^(?:in|obs)\/agent\/([^/]+)/)?.[1] ?? null;
export const newWebConversationId = (agent: string) => `web-${agent}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
export const conversationStorageKey = (agent: string) => `lanius.currentConversation.${agent}`;
// Coding-tool agent NOUNS are `claude-code` and `codex` (src/codeagent.rs); bare
// `claude` is only a CLI alias, never a bus agent — keep it out so a real agent a
// user names `claude` isn't evicted to Workers. The `code-*` session id (see
// isWorkerSessionId) is the reliable per-run fallback.
export const codingAgentNouns = ['claude-code', 'codex'] as const;
const codingAgentNames = new Set<string>(codingAgentNouns);
export const isWorkerAgentName = (name: string) => codingAgentNames.has(String(name ?? '').toLowerCase());
export const isWorkerSessionId = (session: string) => /^code-[A-Za-z0-9_-]+/.test(String(session ?? ''));

// Presentational source-token → chip label (worker-dm unification M2). The
// projection stamps a machine token — `"code"` for a coding-session DM thread —
// and the UI owns the pretty label. This is a LABEL LOOKUP ONLY: it must never
// grow a behavioral branch. Routing/behavior keys on the raw token directly
// (`source === 'code'`), never on this map. An unknown token falls through to
// itself, so an honest source name always shows.
const sourceLabels: Record<string, string> = { code: 'coding session' };
export const sourceLabel = (token: string) => sourceLabels[String(token ?? '')] ?? String(token ?? '');
export const isWorkerSource = (source: unknown) => source === 'code';
export const sessionFromPayload = (payload: any, env: any) => payload?.session || (env?.correlation_id ? `evt-${env.correlation_id}` : env?.id ? `evt-${env.id}` : '');
// Content-identity for a conversation message, IDENTICAL to convKey in
// server.mjs. The same logical message arrives both as a live bus event (keyed
// by correlation) and from the durable backfill (which may lack a correlation),
// so they must collapse to one key or every reply doubles on re-open. (class,
// text) is the only attribute both sources share; asks/failures carry no text so
// they key on correlation. Trade-off: two identical same-class texts in one
// thread merge — rare, and the right call versus guaranteed duplication.
export const convMessageKey = (m: any) => {
  const cls = m.cls ?? (m.who === 'you' ? 'you' : 'agent');
  if (m.type === 'ask') return `ask:${m.corr ?? m.event_id ?? ''}`;
  if (cls === 'failed') return `failed:${m.corr ?? m.event_id ?? ''}`;
  return `${cls}:${String(m.text ?? '')}`;
};
export const mergeConvMessages = (current: any[] = [], incoming: any[] = []) => {
  const byKey = new Map();
  for (const m of [...current, ...incoming]) {
    const key = convMessageKey(m);
    if (!byKey.has(key)) byKey.set(key, { id: m.id ?? uid(), ...m, key });
  }
  return [...byKey.values()].sort((a, b) => String(a.ts ?? '').localeCompare(String(b.ts ?? '')));
};

export function topicFilterMatches(filterText: string, value: string) {
  const f = String(filterText ?? '');
  if (f === '#') return true;
  const fp = f.split('/');
  const vp = String(value ?? '').split('/');
  for (let i = 0, j = 0; i < fp.length; i++, j++) {
    if (fp[i] === '#') return true;
    if (j >= vp.length) return false;
    if (fp[i] !== '+' && fp[i] !== vp[j]) return false;
  }
  return fp.length === vp.length;
}
