import { useEffect, useMemo, useState } from 'react';
import { relativeTime } from '../lib/format';
import { isWorkerSessionId } from '../lib/conversation';
import WorkerNoteCompose from './WorkerNoteCompose';

// One delivery to a worker, as `lanius code mail --json` (the /api/comms/mail
// projection) returns it. A PURE LEDGER READ over `in/agent/%` — so a note the
// human sent to a worker survives a page reload with no live stream (the
// worker-notes-panel truth source).
type MailRow = {
  id: number;
  from: string | null;
  to: string | null;
  to_noun: string | null;
  preview: string;
  ts: string;
  state: string;
};

// The trace-fallback panel: the notes already sent to this worker (read back from
// the mail projection, so they persist across reload) plus the shared compose.
//
// A coding worker is listed by its agent NOUN (claude-code/codex), and one noun
// may cover several runs. The candidate run inboxes are derived from BOTH the live
// obs sessions AND the ledger's own mail rows, so after a reload — when there is no
// live obs stream — the run a note was sent to is still recoverable from the
// durable ledger. Raw session/correlation ids are NEVER shown to the human: a run
// is named "run N", a code-* sender is "another worker", the owner is "you".
export default function WorkerNotesPanel({
  agent,
  liveSessions,
  owner,
}: {
  agent: string;
  liveSessions: string[];
  owner?: string;
}) {
  const [mail, setMail] = useState<MailRow[] | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [picked, setPicked] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    fetch('/api/comms/mail')
      .then((r) => (r.ok ? r.json() : []))
      .then((d) => alive && setMail(Array.isArray(d) ? d : []))
      .catch(() => alive && setMail([]));
    return () => {
      alive = false;
    };
  }, [agent, refresh]);

  // Notes addressed to THIS worker only: the noun matches (a worker named by its
  // noun), or the agent name itself IS the code-* run. Never mixes other workers.
  const mine = useMemo(
    () =>
      (mail ?? []).filter(
        (m) => (m.to_noun != null && m.to_noun === agent) || (isWorkerSessionId(agent) && m.to === agent),
      ),
    [mail, agent],
  );

  // The candidate run inboxes: live obs sessions ∪ any run that has received a
  // note ∪ the agent name when it is itself a run id.
  const sessions = useMemo(() => {
    const s = new Set<string>();
    for (const id of liveSessions) if (isWorkerSessionId(id)) s.add(id);
    for (const m of mine) if (m.to && isWorkerSessionId(m.to)) s.add(m.to);
    if (isWorkerSessionId(agent)) s.add(agent);
    return [...s];
  }, [liveSessions, mine, agent]);
  const sessionsKey = sessions.join('|');

  // Keep the pick valid as the candidate set resolves.
  useEffect(() => {
    if (!sessions.length) {
      setPicked(null);
      return;
    }
    setPicked((p) => (p && sessions.includes(p) ? p : sessions[0]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionsKey]);

  if (mail === null) return null; // still loading — don't flash the panel
  if (!sessions.length) return null; // nothing to send to — keep the "open runs" fallback

  const session = picked && sessions.includes(picked) ? picked : sessions[0];
  // Oldest → newest so the list reads like a thread above the compose.
  const notes = mine.filter((m) => m.to === session).slice().reverse();
  const who = (from: string | null) => {
    if (!from) return 'someone';
    if (owner && from === owner) return 'you';
    if (isWorkerSessionId(from)) return 'another worker';
    return from;
  };

  return (
    <div className="worker-notes" data-sel="worker-notes">
      {sessions.length > 1 && (
        <label className="worker-notes-pick">
          <span>run</span>
          <select value={session} onChange={(e) => setPicked(e.target.value)} aria-label="choose which run to message">
            {sessions.map((s, i) => (
              <option key={s} value={s}>
                run {i + 1}
                {liveSessions.includes(s) ? ' · live' : ''}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className="worker-notes-list" data-sel="worker-notes-list" aria-label="notes sent to this worker">
        {notes.length ? (
          notes.map((m) => (
            <div key={m.id} className="worker-note-item">
              <div className="worker-note-item-meta">
                <span className="worker-note-who">{who(m.from)}</span>
                <span className="worker-note-when">{relativeTime(m.ts)}</span>
              </div>
              <div className="worker-note-item-text">{m.preview}</div>
            </div>
          ))
        ) : (
          <div className="worker-notes-empty">No notes sent to this worker yet.</div>
        )}
      </div>
      <WorkerNoteCompose session={session} onDelivered={() => setRefresh((n) => n + 1)} />
    </div>
  );
}
