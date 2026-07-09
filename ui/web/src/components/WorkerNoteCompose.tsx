import { useEffect, useState } from 'react';

// The shared "send a note to this worker" compose (worker-notes-panel handoff).
// ONE component, used by BOTH the runs detail panel (CodeSessions) and the trace
// fallback (ConverseView) — no copy-paste divergence.
//
// It relays a human note through POST /api/code/deliver into the worker's INBOX;
// it is NEVER a chat message and never touches the conversation projection. The
// honest framing (a running job's inbox, not a chat) and the CLI's own
// accepted/failed verdict are load-bearing: feedback is the relay's real exit,
// never a fabricated delivery promise.
export default function WorkerNoteCompose({
  session,
  onDelivered,
  id,
}: {
  session: string;
  onDelivered?: () => void;
  id?: string;
}) {
  const [note, setNote] = useState('');
  const [sending, setSending] = useState(false);
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  // Clear the compose when it re-targets a different worker/run.
  useEffect(() => {
    setNote('');
    setFeedback(null);
  }, [session]);

  const send = async () => {
    const message = note.trim();
    if (!message || !session) return;
    setSending(true);
    setFeedback(null);
    try {
      const r = await fetch('/api/code/deliver', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ session, message }),
      });
      const d = await r.json().catch(() => ({}));
      if (r.ok && d.delivered) {
        setFeedback({ ok: true, text: 'delivered — the worker reads it on its next turn' });
        setNote('');
        onDelivered?.();
      } else {
        setFeedback({ ok: false, text: d.error ? `not delivered: ${d.error}` : `not delivered (HTTP ${r.status})` });
      }
    } catch (e) {
      setFeedback({ ok: false, text: `not delivered: ${String((e as Error).message ?? e)}` });
    } finally {
      setSending(false);
    }
  };

  return (
    <div className="worker-note" id={id} data-sel="worker-note">
      <div className="worker-note-head">send a note to this worker</div>
      <div className="worker-note-sub">this is a running job, not a chat — your note goes to its inbox and it reads it on its next turn.</div>
      <div className="worker-note-row">
        <input
          className="worker-note-input"
          type="text"
          value={note}
          placeholder="a note for this worker…"
          aria-label="a note for this worker"
          disabled={sending}
          onChange={(e) => setNote(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') send(); }}
        />
        <button className="worker-note-send" disabled={sending || !note.trim()} onClick={send}>{sending ? 'sending…' : 'send'}</button>
      </div>
      {feedback && (
        <div className={feedback.ok ? 'worker-note-ok' : 'worker-note-err'} data-deliver-feedback={feedback.ok ? 'ok' : 'err'}>{feedback.text}</div>
      )}
    </div>
  );
}
