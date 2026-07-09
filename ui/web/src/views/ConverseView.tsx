import { useState, useRef, useEffect } from 'react';
import Markdown from '../Markdown';
import AgentChip from '../components/AgentChip';
import { IconButton } from '../components/primitives';
import { relativeTime, summarize } from '../lib/format';

function ConverseView({ hidden, agent, messages, conversations, current, submitCompose, answerAsk, selectAgent, openConversation, newConversation, startBranch, branchOrigin, selectCodeSessions, isTraceAgent, sendLabel, allowHtml }: any) {
  const [conversationSearch, setConversationSearch] = useState('');
  // chat-follow M1: "pinned" is derived from scroll position, never a suppress
  // flag (docs/handoffs/chat-follow.md wonky bit 2) — a programmatic scroll-to-
  // bottom always lands within tolerance, so the scroll listener re-deriving
  // pinned from position can't misread our own scroll as "the user scrolled up".
  const feedRef = useRef<HTMLDivElement>(null);
  const [pinned, setPinned] = useState(true);
  const AT_BOTTOM_TOLERANCE = 40;
  const isAtBottom = (el: HTMLDivElement) => el.scrollHeight - el.scrollTop - el.clientHeight < AT_BOTTOM_TOLERANCE;
  const scrollToBottom = () => { const el = feedRef.current; if (el) el.scrollTop = el.scrollHeight; };
  useEffect(() => {
    const el = feedRef.current;
    if (!el) return;
    const onScroll = () => setPinned(isAtBottom(el));
    el.addEventListener('scroll', onScroll);
    return () => el.removeEventListener('scroll', onScroll);
  }, []);
  // Switching conversations (a different agent, a different session, or a
  // fresh branch) always resets to pinned-at-bottom — no per-conversation
  // scroll memory (wonky bit 4, a stated non-goal).
  useEffect(() => { scrollToBottom(); setPinned(true); }, [agent, current]);
  // Keyed on the last message's id (wonky bit 3), not on every render: an SSE
  // reconnect replay that merges to the same last message must not yank the
  // scroll of someone who scrolled up to read history.
  const lastMessageId = messages.length ? messages[messages.length - 1].id : null;
  useEffect(() => { if (pinned) scrollToBottom(); }, [lastMessageId, pinned]);
  const allConversations = conversations?.list ?? [];
  const query = conversationSearch.trim().toLowerCase();
  const resultConversations = query
    ? allConversations.filter((c: any) => [c.title, c.preview, c.session, c.source].some((v) => String(v ?? '').toLowerCase().includes(query)))
    : allConversations.slice(0, 6);
  // M2 (chat-rendering): decide comms-plane-vs-trace purely from bus/ledger reads.
  // Comms-plane traffic between the owner and this agent = the conversation list the
  // ledger projection returns (/api/conversations reads in/agent + correlated
  // in/human events) is non-empty, OR live comms-plane messages are already in the
  // thread. The observation plane = the agent's obs trace, which surfaces as worker
  // sessions (obs/agent/<noun>/code-*/…) in the agents map. Both signals are bus
  // reads — no per-agent UI flag — so a third-party UI reproduces the same decision.
  //   - comms-plane traffic exists → render the conversation (below);
  //   - NO comms-plane traffic but an obs trace exists → the agent is a worker whose
  //     value is its trace, not a reply → fall back to the runs surface;
  //   - neither (a fresh chat agent the owner just created) → default to the chat
  //     surface so the first message can be composed.
  // `status === 'idle'/'loading'` before the first fetch is "still resolving": keep
  // the chat surface so we never flash the trace fallback while comms history loads.
  const resolved = conversations?.status === 'list' || conversations?.status === 'error';
  const hasComms = allConversations.length > 0 || messages.length > 0;
  const traceOnly = resolved && !hasComms && !!isTraceAgent;
  if (traceOnly) {
    return (
      <div id="view-converse" className="view" data-mode="trace" hidden={hidden}>
        <div id="conv-configure-hint" className="conv-configure-hint">
          <AgentChip name={agent} size="md" />
          <span>Tune {agent} anytime in configure.</span>
          <IconButton label={`configure ${agent}`} className="ghost cfg-icon-btn" onClick={() => selectAgent(agent, 'configure')}>⚙</IconButton>
        </div>
        <div id="conv-trace-fallback" className="conv-trace-fallback">
          <p className="conv-empty-mark"><AgentChip name={agent} size="lg" /></p>
          <p>{agent} hasn’t sent any messages on the comms plane — its work shows up as a trace.</p>
          <p className="dim-note">There’s no chat conversation here. Watch what it’s doing in the runs surface.</p>
          <button id="conv-open-runs" className="ghost" type="button" onClick={() => selectCodeSessions && selectCodeSessions()}>open runs ⟶</button>
        </div>
      </div>
    );
  }
  return (
    <div id="view-converse" className="view" data-mode="comms" hidden={hidden}>
      <div id="conv-configure-hint" className="conv-configure-hint">
        <AgentChip name={agent} size="md" />
        <span>Tune {agent} anytime in configure.</span>
        <IconButton id="conv-new" label={`new conversation with ${agent}`} className="ghost cfg-icon-btn" onClick={() => newConversation(agent)}>＋</IconButton>
        <IconButton label={`configure ${agent}`} className="ghost cfg-icon-btn" onClick={() => selectAgent(agent, 'configure')}>⚙</IconButton>
      </div>
      <div id="conv-recent" className="conv-recent">
        <label className="conv-search">
          <span aria-hidden="true">⌕</span>
          <input type="search" value={conversationSearch} onChange={(e) => setConversationSearch(e.target.value)} placeholder="search conversations" aria-label={`search conversations with ${agent}`} />
        </label>
        <div className="conv-recent-list" aria-label={query ? `conversation search results for ${agent}` : `recent conversations with ${agent}`}>
          {conversations?.status === 'loading' && !allConversations.length ? <span className="dim-inline">loading conversations…</span>
            : conversations?.status === 'error' && !allConversations.length ? <span className="dim-inline">recent conversations unavailable</span>
              : !allConversations.length ? <span className="dim-inline">recent conversations will appear here.</span>
                : !resultConversations.length ? <span className="dim-inline">no matching conversations.</span>
                : resultConversations.map((c: any) => (
                  <button key={c.session} className={`conv-recent-row${c.session === current ? ' on' : ''}`} title={c.session} type="button" onClick={() => openConversation(agent, c.session)}>
                    <span>{c.title || c.preview || 'conversation'}</span>
                    {c.branched_from && <span className="conv-branched-sub" data-sel="conv-branched">branched from: {c.branched_from.preview || 'an earlier message'}</span>}
                    <em><span className="source-badge">{c.source || 'you'}</span>{relativeTime(c.last_ts)}</em>
                  </button>
                ))}
        </div>
      </div>
      <div id="conv-holder" className="conv-feed-holder">
        {branchOrigin && (
          // M3 (docs/handoffs/reply-branching.md): the origin chip quotes the
          // message this thread branched from and links back to the parent.
          // Rendered from `branchOrigin` (a pending fork or a loaded conversation's
          // `branched_from`) — the quoted text + a human affordance, never a raw id.
          <div id="conv-origin" className="conv-origin-chip" data-sel="conv-origin">
            <span className="origin-label">branched from</span>
            <blockquote className="origin-quote">{branchOrigin.quote || 'an earlier message'}</blockquote>
            {branchOrigin.session && <button type="button" className="origin-link ghost" data-sel="conv-origin-link" onClick={() => openConversation(agent, branchOrigin.session)}>view the original conversation ⟶</button>}
          </div>
        )}
        <div ref={feedRef} className="conv-feed" role="log" aria-live="polite" aria-label={`conversation with ${agent}`}>
          {!messages.length && !branchOrigin && <div className="conv-empty"><p className="conv-empty-mark"><AgentChip name={agent} size="lg" /></p><p>Start a conversation with {agent}. Replies and asks stay in this thread.</p></div>}
          {messages.map((m: any) => m.type === 'ask' ? <AskMessage key={m.id} agent={agent} message={m} answerAsk={answerAsk} allowHtml={allowHtml} /> : <div key={m.id} className={`msg ${m.cls}`} title={m.corr ? `conversation ${m.corr}` : ''}><div className="msg-meta"><span className="msg-who">{m.who}</span>{!m.failed && startBranch && <button type="button" className="msg-reply" data-sel="msg-reply" title="reply — branches a new conversation" onClick={() => startBranch(agent, m)}>↳ reply</button>}</div><div className="msg-body">{m.failed ? <><div className="fail-reason">{m.text}</div><div className="fail-hint">check the agent: a model set, the background service running, and the add-on turned on.</div></> : <Markdown text={String(m.text ?? '')} allowHtml={allowHtml} format={m.format} />}</div></div>)}
        </div>
        {!pinned && <button type="button" className="conv-jump" data-sel="conv-jump" onClick={() => { scrollToBottom(); setPinned(true); }}>new messages ↓</button>}
      </div>
      <form id="compose" className="compose" autoComplete="off" onSubmit={submitCompose} aria-label={`message ${agent}`}><span className="compose-sigil">»</span><input id="compose-input" type="text" aria-label={`message ${agent}`} placeholder={`message ${agent}...`} spellCheck={false} /><IconButton type="submit" id="compose-send" label={sendLabel} className="compose-send">➤</IconButton></form>
    </div>
  );
}

function AskMessage({ agent, message, answerAsk, allowHtml }: any) {
  const [text, setText] = useState('');
  const p = message.payload ?? {};
  const send = (answer: string) => answer && answerAsk(agent, message.id, message.corr, answer);
  // An ask draws in its own affordance, not the plain feed row, so the trust
  // gate must be applied here too (docs/handoffs/html-messages.md wonky bit 4):
  // render the question body through Markdown so format="html" at full trust
  // becomes live DOM and reduced trust shows it escaped.
  return (
    <div className="msg agent ask"><div className="msg-meta"><span className="msg-who">agent asks</span>{message.corr && <span className="msg-corr">{message.corr.slice(0, 18)}</span>}</div><div className="msg-body"><div className="ask-q">{p.question != null ? <Markdown text={String(p.question)} allowHtml={allowHtml} format={p.format} /> : summarize(p)}</div>{message.answered ? <div className="ask-done">{message.answered.includes(':') ? <>{message.answered.split(':')[0]}: <b>{message.answered.split(':').slice(1).join(':').trim()}</b></> : message.answered}</div> : <>{Array.isArray(p.options) && !!p.options.length && <div className="ask-options">{p.options.map((o: any) => <button key={String(o)} onClick={() => send(String(o))}>{String(o)}</button>)}</div>}<div className="ask-row"><input placeholder="answer…" value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter' && text.trim()) send(text.trim()); }} /><button onClick={(e) => { e.preventDefault(); send(text.trim()); }}>answer</button></div></>}</div></div>
  );
}

export default ConverseView;
