// agent-comms-ui M2/M3/M6: the human's seat for the cross-agent comms plane.
//
// The third leg of the nav split (AGENTS/conversations · WORKERS/runs · COMMS):
// what the agents are saying to EACH OTHER. It renders agent-to-agent mail as a
// threaded, typed list (FROM → TO, priority chip, state badge, failure, the
// mid-task tell) and the coordination rooms (roster + claims + channel). Both are
// projections over the existing ledger — `/api/comms/mail` and `/api/comms/rooms`
// shell the CLI exactly like `/api/code/sessions`, and the live overlay folds the
// same `/api/stream` tail CodeSessions does (backfill + foldLive, deduped by the
// server `seq`). No new bus capture.
//
// CORRECTNESS (handoff "concerns spotted in the shipped code"):
//  - Dedup agent-to-agent mail by EVENT id: a high-priority delivery handed BOTH
//    mid-cycle and next-turn is ONE event, so it is ONE row with a "delivered
//    mid-task" tell, never two rows (the row key is the event id).
//  - The signal lamp (M6) is keyed on the EVENT crossing the stream, in App.tsx,
//    not on a hook firing — a flaky tool re-firing PostToolUseFailure cannot
//    strobe the lamp because the event id is what matters.
//  - Mid-cycle mail is deliberately NOT marked seen; the row says "urgent copy
//    delivered early; still unread" rather than presenting it as a bug.
import { useEffect, useRef, useState } from 'react';
import { openLiveStream } from './live';

type Mail = {
  id: number;
  from: string | null;
  to: string | null;
  to_noun: string | null;
  correlation: string | null;
  priority: number;
  state: string;
  failed: boolean;
  mid_cycle: boolean;
  preview: string;
  ts: string;
};

type RoomMember = { session: string; agent_noun: string; live: boolean };
type RoomClaim = { session: string; path: string; created_at: string };
type RoomMessage = { from: string | null; message: string; created_at: string };
type Room = { room: string; label?: string; workdir?: string | null; members: RoomMember[]; claims: RoomClaim[]; channel: RoomMessage[] };

function relTime(ts: string | null): string {
  if (!ts) return '';
  const t = Date.parse(ts);
  if (!Number.isFinite(t)) return '';
  const s = Math.round((Date.now() - t) / 1000);
  if (s < 0) return 'now';
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

// The priority chip: normal (quiet), high (the algedonic tell), or signal (the
// loudest). Keyed on the numeric priority; the HIGH threshold mirrors the
// backend default (agent-comms.high_priority_threshold = 5).
const HIGH_PRIORITY = 5;
function PriorityChip({ priority }: { priority: number }) {
  if (priority >= HIGH_PRIORITY) return <span className="cm-chip cm-high" title={`priority ${priority}`}>high</span>;
  if (priority > 0) return <span className="cm-chip cm-elevated" title={`priority ${priority}`}>p{priority}</span>;
  return <span className="cm-chip cm-normal">normal</span>;
}

// The state badge: reuses the CodeSessions StatusBadge look (cm-* mirrors cs-*).
function StateBadge({ state, failed }: { state: string; failed: boolean }) {
  if (failed) return <span className="cm-badge cm-failed" title="This run failed.">failed</span>;
  const cls = state === 'done' ? 'cm-badge cm-done' : state === 'pending' || state === 'running' ? 'cm-badge cm-live' : 'cm-badge';
  return <span className={cls}>{state}</span>;
}

// One mail row, expandable to its correlation thread.
function MailRow({ m, onSelectSession }: { m: Mail; onSelectSession?: (id: string) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`comms-row cm-row${m.failed ? ' cm-row-failed' : ''}`} data-event-id={m.id}>
      <div className="cm-line" onClick={() => setOpen((v) => !v)} role="button" aria-expanded={open}>
        <span className="cm-from cm-id" title="from" onClick={(e) => { e.stopPropagation(); if (m.from && onSelectSession) onSelectSession(m.from); }}>{m.from ?? '?'}</span>
        <span className="cm-arrow">→</span>
        <span className="cm-to cm-id" title="to" onClick={(e) => { e.stopPropagation(); if (m.to && onSelectSession) onSelectSession(m.to); }}>{m.to ?? '?'}</span>
        <PriorityChip priority={m.priority} />
        <StateBadge state={m.state} failed={m.failed} />
        {m.mid_cycle && <span className="cm-tell" title="Delivered while the agent was working — unread until it checks its inbox.">delivered mid-task</span>}
        <span className="cm-preview">{m.preview}</span>
        <span className="cm-dim cm-time">{relTime(m.ts)}</span>
      </div>
      {open && (
        <div className="cm-thread">
          <div className="cm-dim">Thread: <code>{m.correlation ?? '(none)'}</code></div>
          {m.failed && <div className="cm-thread-fail">↳ This run failed.</div>}
          {m.mid_cycle && <div className="cm-dim">urgent copy delivered early (mid-task); the same message still counts as unread in the next-turn inbox until the agent pulls it — by design, not a duplicate.</div>}
        </div>
      )}
    </div>
  );
}

export default function CommsView({ onSelectSession }: { onSelectSession?: (id: string) => void } = {}) {
  const [mail, setMail] = useState<Mail[]>([]);
  const [rooms, setRooms] = useState<Room[]>([]);
  const [error, setError] = useState('');
  // Live mail folded off the stream since the last backfill, keyed by event id so
  // a double-channel delivery (mid-cycle + next-turn) is one entry, never two.
  const [liveMail, setLiveMail] = useState<Map<number, Mail>>(new Map());
  const seenSeqs = useRef<Set<number>>(new Set());

  // Backfill from the projection (the history authority). Each backfill clears
  // the live overlay + the dedup seq-set — the fresh rows already reflect every
  // event the projection has folded (same reconcile rule as CodeSessions).
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const [mr, rr] = await Promise.all([fetch('/api/comms/mail'), fetch('/api/comms/rooms')]);
        const md = mr.ok ? await mr.json() : [];
        const rd = rr.ok ? await rr.json() : [];
        if (!alive) return;
        setMail(Array.isArray(md) ? md : []);
        setRooms(Array.isArray(rd) ? rd : []);
        setLiveMail(new Map());
        seenSeqs.current = new Set();
        setError('');
      } catch (e) {
        if (alive) setError(String((e as Error).message ?? e));
      }
    };
    load();
    const t = setInterval(load, 15000);
    return () => { alive = false; clearInterval(t); };
  }, []);

  // Live overlay: fold `in/agent/<noun>/<session>` deliveries off the same stream
  // App.tsx uses. We synthesize a Mail row from the event so a delivery made while
  // the view is open appears without a reload; the next backfill reconciles it
  // (with the authoritative failed/mid_cycle joins). Deduped by server seq AND by
  // event id (the row identity).
  useEffect(() => {
    const es = openLiveStream(
      (msg: any) => {
        if (msg.kind !== 'message' || typeof msg.topic !== 'string') return;
        const parsed = parseAgentMailbox(msg.topic);
        if (!parsed) return;
        if (typeof msg.seq === 'number') {
          if (seenSeqs.current.has(msg.seq)) return;
          seenSeqs.current.add(msg.seq);
        }
        const env = msg.env ?? {};
        const payload = env.payload && typeof env.payload === 'object' ? env.payload : {};
        // Only deliveries (a prompt/text) — not answers/acks — are mail rows.
        const preview = typeof payload.prompt === 'string' ? payload.prompt : typeof payload.text === 'string' ? payload.text : '';
        if (!preview) return;
        const id = typeof env.id === 'number' ? env.id : typeof env.event_id === 'number' ? env.event_id : -(seenSeqs.current.size);
        const row: Mail = {
          id,
          from: env.sender ?? null,
          to: parsed.session,
          to_noun: parsed.noun,
          correlation: env.correlation_id ?? null,
          priority: typeof env.priority === 'number' ? env.priority : (typeof payload.priority === 'number' ? payload.priority : 0),
          state: 'pending',
          failed: payload.failed === true,
          mid_cycle: false,
          preview: preview.slice(0, 200),
          ts: env.ts ?? new Date().toISOString(),
        };
        setLiveMail((prev) => {
          if (prev.has(id)) return prev; // dedup by event id — never a duplicate row
          const next = new Map(prev);
          next.set(id, row);
          return next;
        });
      },
      () => { /* transient drop — the backfill tick reconciles */ },
    );
    return () => es.close();
  }, []);

  // Merge: backfill is the base (authoritative), live rows only ADD events not yet
  // in the backfill (keyed by event id), newest first.
  const byId = new Map<number, Mail>();
  for (const m of mail) byId.set(m.id, m);
  for (const [id, m] of liveMail) if (!byId.has(id)) byId.set(id, m);
  const merged = [...byId.values()].sort((a, b) => (b.id ?? 0) - (a.id ?? 0) || (b.ts ?? '').localeCompare(a.ts ?? ''));

  return (
    <div id="view-comms" className="view cm-wrap">
      <style>{CM_STYLE}</style>
      <div className="cm-main">
        <h3 className="cm-h">Agent-to-agent mail</h3>
        {error && <div className="cm-err">comms projection unavailable: {error}</div>}
        {!error && merged.length === 0 && (
          <div className="cm-dim cm-empty">No messages yet. When one agent hands work to another, it shows up here.</div>
        )}
        <div className="cm-list">
          {merged.map((m) => (
            <MailRow key={m.id} m={m} onSelectSession={onSelectSession} />
          ))}
        </div>
      </div>

      <div className="cm-rooms">
        <h3 className="cm-h">Rooms &amp; shared channels</h3>
        {rooms.length === 0 && <div className="cm-dim">No shared rooms yet. Agents working in the same folder share a room here.</div>}
        {rooms.map((r) => (
          <div key={r.room} className="comms-room cm-room">
            <div className="cm-room-head">
              <span className="cm-room-name" title={r.workdir ?? undefined}>{r.label || r.room}</span>
              {r.label && r.label !== r.room && <span className="cm-room-id">{r.room}</span>}
              <span className="cm-room-count">{r.members.length} member{r.members.length === 1 ? '' : 's'}</span>
            </div>
            <div className="cm-room-members">
              {r.members.map((m) => (
                <span key={m.session} className={`cm-member${m.live ? ' cm-member-live' : ' cm-member-stale'}`} title={m.live ? 'live' : 'stale (owning process gone)'}>
                  <span className="cm-id" onClick={() => onSelectSession && onSelectSession(m.session)}>{m.session}</span>
                  <span className="cm-dim">{m.agent_noun}</span>
                </span>
              ))}
            </div>
            {r.claims.length > 0 && (
              <div className="cm-claims">
                {r.claims.map((c, i) => (
                  <div key={`${c.session}:${c.path}:${i}`} className="cm-claim">
                    <span className="cm-claim-path">{c.path}</span>
                    <span className="cm-dim">claimed by</span>
                    <span className="cm-id">{c.session}</span>
                  </div>
                ))}
              </div>
            )}
            {r.channel.length > 0 && (
              <div className="cm-channel">
                <div className="cm-dim">recent channel traffic:</div>
                {r.channel.map((msg, i) => (
                  <div key={i} className="cm-chan-msg">
                    <span className="cm-id">{msg.from ?? '?'}</span>
                    <span className="cm-chan-text">{msg.message}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// Parse `in/agent/<noun>/<session>` (the session-addressed mailbox). Returns null
// for a bare agent mailbox (`in/agent/<noun>`) or any other shape.
function parseAgentMailbox(topic: string): { noun: string; session: string } | null {
  const segs = topic.split('/');
  if (segs.length !== 4 || segs[0] !== 'in' || segs[1] !== 'agent') return null;
  const session = decodeURIComponent(segs[3]);
  if (!session.startsWith('code-')) return null;
  return { noun: decodeURIComponent(segs[2]), session };
}

/* Messages (agent-to-agent) — on the shrike tokens (themes with the app). Data
   reads mono; the one reserved red (--pain) is spent only on state that demands
   action: a high-priority message and a failed delivery. Elevated priority is a
   step up (--agent), not an alarm; live/normal stay quiet grey and --work. */
const CM_STYLE = `
.cm-wrap { display: flex; gap: 16px; align-items: flex-start; font-variant-numeric: tabular-nums; }
.cm-main { flex: 1 1 60%; min-width: 0; }
.cm-rooms { flex: 1 1 40%; min-width: 0; border-left: 1px solid var(--panel-edge); padding-left: 16px; }
.cm-h { margin: 0 0 8px; font-size: 14px; color: var(--ink); }
.cm-dim { color: var(--dim); }
.cm-id { font-family: var(--mono); color: var(--work); cursor: pointer; }
.cm-id:hover { text-decoration: underline; }
.cm-err { color: var(--pain); font-family: var(--mono); font-size: 12px; }
.cm-empty { font-size: 12px; max-width: 60ch; color: var(--dim); }
.cm-list { display: flex; flex-direction: column; gap: 2px; }
.cm-row { border-radius: var(--r-sharp); font-size: 12px; }
.cm-line { display: flex; gap: 8px; align-items: center; padding: 5px 6px; border-radius: var(--r-sharp); cursor: pointer; flex-wrap: wrap; border-left: 2px solid transparent; }
.cm-line:hover { background: var(--hover); }
.cm-row-failed .cm-line { background: color-mix(in srgb, var(--pain) 7%, transparent); border-left-color: var(--pain); }
.cm-arrow { color: var(--meta); }
.cm-preview { color: var(--dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 28ch; }
.cm-time { margin-left: auto; font-family: var(--mono); color: var(--meta); }
.cm-chip { font-family: var(--mono); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; padding: 0 6px; border-radius: 2px; border: 1px solid currentColor; }
.cm-normal { color: var(--meta); }
.cm-elevated { color: var(--agent); }
.cm-high { color: var(--pain); font-weight: 600; }
.cm-badge { font-family: var(--mono); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; padding: 0 6px; border-radius: 2px; border: 1px solid var(--panel-edge); color: var(--dim); }
.cm-live { border-color: color-mix(in srgb, var(--work) 55%, transparent); color: var(--work); background: color-mix(in srgb, var(--work) 8%, transparent); }
.cm-done { border-color: var(--panel-edge); color: var(--meta); }
.cm-failed { border-color: color-mix(in srgb, var(--pain) 55%, transparent); color: var(--pain); background: color-mix(in srgb, var(--pain) 8%, transparent); font-weight: 600; }
.cm-tell { font-family: var(--mono); font-size: 10px; padding: 0 6px; border-radius: 2px; border: 1px solid var(--ask-border); color: var(--ask); cursor: help; }
.cm-thread { margin: 0 0 4px 18px; padding: 4px 8px; border-left: 1px solid var(--subtle-border-strong); font-size: 11px; display: flex; flex-direction: column; gap: 3px; }
.cm-thread code { font-family: var(--mono); }
.cm-thread-fail { color: var(--pain); }
.cm-room { border: 1px solid var(--panel-edge); border-radius: var(--r-card); padding: 8px 10px; margin-bottom: 8px; font-size: 12px; background: var(--card-bg-soft); }
.cm-room-head { display: flex; gap: 8px; align-items: baseline; margin-bottom: 4px; }
.cm-room-name { font-family: var(--mono); font-weight: 600; color: var(--ink); }
.cm-room-id { font-family: var(--mono); color: var(--meta); font-size: 11px; }
.cm-room-count { color: var(--dim); font-size: 11px; }
.cm-room-members { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 4px; }
.cm-member { display: inline-flex; gap: 5px; align-items: baseline; padding: 1px 6px; border-radius: 2px; font-family: var(--mono); font-size: 11px; }
.cm-member-live { background: color-mix(in srgb, var(--work) 14%, transparent); color: var(--work); }
.cm-member-stale { background: var(--hover-soft); color: var(--meta); opacity: 0.8; }
.cm-claims { display: flex; flex-direction: column; gap: 1px; margin: 4px 0; }
.cm-claim { display: flex; gap: 6px; align-items: baseline; }
.cm-claim-path { font-family: var(--mono); color: var(--dim); }
.cm-channel { margin-top: 4px; }
.cm-chan-msg { display: flex; gap: 6px; padding: 1px 0; }
.cm-chan-text { color: var(--dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
`;
