import { useState } from 'react';
import { conversationLabel, shortTs } from '../lib/format';

function SessionsView({ hidden, state, agent, openTranscript, loadSessions, repair, approvePackage }: any) {
  const [note, setNote] = useState('');
  // package-truth.md M2 (wonky bit 3): the sessions tab and the history package
  // row derive the SAME honest degraded state from the shared health projection.
  // A revoked history pane is TERMINAL — no approve button (approve is a no-op);
  // a dispatcher-down / parked-approved pane shows the truth and the `lanius
  // daemon` command, never a fake button. Only a `requested` grant is repairable.
  const r = repair ?? { kind: 'ok', message: '', canApprove: false };
  const doApprove = async () => {
    setNote('allowing…');
    const res = await approvePackage?.('history');
    setNote(res?.ok ? 'allowed — reopening…' : (res?.error ?? 'could not allow it'));
    if (res?.ok) void loadSessions(agent);
  };
  return (
    <div id="view-sessions" className="view" hidden={hidden}>
      <div id="sessions-pane" className="sessions-pane">
        {state.status === 'loading' && <div className="dim-note">asking the history view…</div>}
        {state.status === 'error' && <div className="dim-note sessions-degraded" data-repair={r.kind}><div>History rebuilds transcripts from the history package. It is not Activity, and it is unavailable right now.</div><div className="dim-sub">{r.message || state.error}</div>{r.canApprove && <div className="setup-row"><button className="sessions-approve" onClick={doApprove}>allow and start</button>{note && <span className="dim-sub">{note}</span>}</div>}</div>}
        {state.status === 'list' && (!state.sessions.length ? <div className="dim-note">no conversations recorded yet.</div> : <div className="sess-list"><div className="sess-row sess-head">{['conversation', 'first', 'last', 'msgs', 'events'].map((h) => <span key={h}>{h}</span>)}</div>{state.sessions.map((s: any) => <button key={s.session} className="sess-row" title={s.session} onClick={() => openTranscript(agent, s.session, undefined, false, conversationLabel(s))}><span className="sess-id">{conversationLabel(s)}</span><span>{shortTs(s.first_ts)}</span><span>{shortTs(s.last_ts)}</span><span>{String(s.message_count)}</span><span>{String(s.event_count)}</span></button>)}</div>)}
        {(state.status === 'transcript-loading' || state.status === 'transcript') && <Transcript agent={agent} state={state} openTranscript={openTranscript} loadSessions={loadSessions} />}
      </div>
    </div>
  );
}

function Transcript({ agent, state, openTranscript, loadSessions }: any) {
  const tr = state.transcript;
  const label = conversationLabel(tr);
  if (state.status === 'transcript-loading') return <div className="dim-note" title={tr?.session}>reading conversation…</div>;
  return (
    <>
      <div className="tr-bar"><button className="tr-back" onClick={() => loadSessions(agent)}>← history</button><span className="tr-title" title={tr.session}>{label}</span></div>
      <div className="tr-feed">{tr.has_more && <button className="tr-earlier" onClick={() => openTranscript(agent, tr.session, tr.messages?.[0]?.id, true)}>… load earlier</button>}{!tr.messages.length && <div className="dim-note">empty transcript.</div>}{tr.messages.map((m: any) => <TranscriptMsg key={m.id ?? `${m.role}-${m.created_at}`} m={m} />)}</div>
    </>
  );
}

function DetailsBlock({ label, cls, content }: any) {
  return <details className={`tr-tool ${cls ?? ''}`}><summary>{label}</summary><pre className="tr-pre">{typeof content === 'string' ? content : JSON.stringify(content, null, 2)}</pre></details>;
}

function TranscriptMsg({ m }: any) {
  const c = m.content;
  if (c && typeof c === 'object' && c.truncated === true && c.preview != null) return <div className={`tr-msg tr-${m.role}`}><div className="msg-meta"><span className="msg-who">{m.role}</span><span>{shortTs(m.created_at)}</span>{m.event_id != null && <span className="msg-corr">ev {m.event_id}</span>}</div><div className="msg-body">{c.preview}<div className="dim-sub">(truncated — {c.chars} chars)</div></div></div>;
  if (m.role === 'tool') return <div className={`tr-msg tr-${m.role}`}><div className="msg-meta"><span className="msg-who">{m.role}</span><span>{shortTs(m.created_at)}</span>{m.event_id != null && <span className="msg-corr">ev {m.event_id}</span>}</div><DetailsBlock label={`⚙ ${c?.name ?? 'tool'} → result`} cls="tr-tool-result" content={c?.content ?? c} /></div>;
  const text = typeof c === 'string' ? c : c?.text;
  return <div className={`tr-msg tr-${m.role}`}><div className="msg-meta"><span className="msg-who">{m.role}</span><span>{shortTs(m.created_at)}</span>{m.event_id != null && <span className="msg-corr">ev {m.event_id}</span>}</div><div className="msg-body">{text && <div className="tr-text">{text}</div>}{Array.isArray(c?.tool_calls) && c.tool_calls.map((tc: any, i: number) => <DetailsBlock key={i} label={`⚙ ${tc.fn_name ?? 'call'}`} cls="tr-tool-call" content={tc.fn_arguments ?? tc} />)}{!text && !Array.isArray(c?.tool_calls) && <DetailsBlock label="raw message" content={c} />}</div></div>;
}

export default SessionsView;
