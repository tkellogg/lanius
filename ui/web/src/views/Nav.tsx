import AgentChip from '../components/AgentChip';
import { IconButton } from '../components/primitives';
import { relativeTime } from '../lib/format';
import { isWorkerAgentName, isWorkerSessionId } from '../lib/conversation';

function historyHint(historyState: string | null | undefined): string {
  if (historyState === 'absent') return 'History is not running. Start or allow it to use transcripts.';
  if (historyState === 'unreachable') return 'History is allowed but not answering. Restart it to use transcripts.';
  return 'History is unavailable right now.';
}

function Nav({ agents, panelAgents, conversations, sel, historyOk, historyState, selectAgent, openConversation, selectSignals, selectSetup, selectCodeSessions, selectComms, selectProviders, navOpen, setNavOpen, exploreLabel }: any) {
  // helper-first-encounter H4 (wonky bit 2): a profile presented in a dedicated
  // surface (`[ui] surface = "panel"`, e.g. the helper) never appears as an
  // agent-list row. Filter by that GENERIC property — passed down as a set of
  // agent nouns — not by matching any literal name.
  const panel: Set<string> = panelAgents ?? new Set();
  const items = [...agents.keys()].sort().filter((name) => !panel.has(name));
  const isWorkerItem = (name: string) => {
    const a = agents.get(name);
    return isWorkerAgentName(name) || [...(a?.sessions ?? [])].some((s) => isWorkerSessionId(s));
  };
  const chatItems = items.filter((name) => !isWorkerItem(name));
  const workerItems = items.filter(isWorkerItem);
  const workerCount = workerItems.reduce((n, name) => n + Math.max(1, agents.get(name)?.sessions?.size ?? 0), 0);
  const onKey = (e: any) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const navItems = [...document.querySelectorAll<HTMLElement>('#nav-list .nav-item')];
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const i = active ? navItems.indexOf(active) : -1;
    navItems[(i + (e.key === 'ArrowDown' ? 1 : -1) + navItems.length) % navItems.length]?.focus();
  };
  const stageLabel = sel.kind === 'welcome' ? 'welcome'
    : sel.kind === 'agent' ? sel.agent
      : sel.kind === 'signals' ? 'activity'
        : sel.kind === 'code-sessions' ? 'runs'
          : sel.kind === 'comms' ? 'messages'
            : sel.kind === 'providers' ? 'providers'
            : 'setup';
  return (
    <nav className={`nav panel${navOpen ? ' nav-open' : ''}`} aria-label="explorer">
      <div className="panel-head">
        <h2>{exploreLabel}</h2>
        <IconButton id="nav-toggle" label={navOpen ? 'collapse navigation' : `expand navigation: ${stageLabel}`} className="ghost cfg-icon-btn nav-toggle-btn" aria-expanded={navOpen} onClick={() => setNavOpen(!navOpen)}>{navOpen ? '✕' : '≡'}</IconButton>
      </div>
      <div id="nav-list" className="nav-list" onKeyDown={onKey}>
        <button className={`nav-item nav-signals${sel.kind === 'signals' ? ' on' : ''}`} data-sel="signals" title="what's happening now across every agent" onClick={selectSignals}><span className="nav-sigil">◮</span> activity</button>
        <button className={`nav-item nav-setup${sel.kind === 'setup' ? ' on' : ''}`} data-sel="setup" title="health check, agent setup, capabilities, and trust" onClick={() => selectSetup()}><span className="nav-sigil">⚒</span> setup</button>
        <button className={`nav-item nav-workers${sel.kind === 'code-sessions' ? ' on' : ''}`} data-sel="code-sessions" title="coding runs and the workers they started" onClick={() => selectCodeSessions && selectCodeSessions()}><span className="nav-sigil">▤</span> runs</button>
        <button className={`nav-item nav-comms${sel.kind === 'comms' ? ' on' : ''}`} data-sel="comms" title="messages agents send each other" onClick={() => selectComms && selectComms()}><span className="nav-sigil">⇄</span> messages</button>
        <button className={`nav-item nav-providers${sel.kind === 'providers' ? ' on' : ''}`} data-sel="providers" title="the model keys your agents use — add, test, pick one per agent" onClick={() => selectProviders && selectProviders()}><span className="nav-sigil">⛁</span> providers</button>
        <div className="nav-label">agents</div>
        <div id="nav-agents">
          {chatItems.map((name) => {
            const a = agents.get(name);
            const convoState = conversations.get(name) ?? {};
            const convos = convoState.list ?? [];
            return (
              <div key={name}>
                <button className={`nav-item nav-agent${sel.kind === 'agent' && sel.agent === name ? ' on' : ''}`} data-sel={`agent:${name}`} title={`open ${name}`} onClick={() => selectAgent(name)}>
                  <AgentChip name={name} /> <span className="nav-agent-name">{name}</span>{a.live && <span className="nav-live">·live</span>}
                </button>
                {convos.slice(0, 8).map((c: any) => (
                  <button key={c.session} className="nav-item nav-conversation" title={c.session} onClick={() => openConversation(name, c.session)}>
                    <span className="nav-convo-title">{c.title || c.preview || 'conversation'}</span>
                    <span className="nav-convo-meta"><span className="source-badge">{c.source || 'you'}</span>{relativeTime(c.last_ts)}</span>
                  </button>
                ))}
                {convoState.status === 'loading' && !convos.length && <div className="nav-hint">loading conversations…</div>}
                {convoState.status === 'error' && !convos.length && <div className="nav-hint">conversation list unavailable</div>}
                {convos.length > 8 && <div className="nav-hint">+{convos.length - 8} more recent</div>}
              </div>
            );
          })}
        </div>
        <div id="nav-empty" className="nav-hint" hidden={chatItems.length > 0}>no agents yet — create one below</div>
        {!!workerItems.length && (
          <details id="nav-workers" className="nav-workers">
            <summary><span>workers</span><span className="nav-worker-count">{workerCount}</span></summary>
            <div className="nav-worker-list">
              {workerItems.map((name) => <button key={name} className="nav-item nav-worker" onClick={() => selectAgent(name)}><span>{name}</span><span className="nav-convo-meta">{agents.get(name)?.sessions?.size ?? 0} run{(agents.get(name)?.sessions?.size ?? 0) === 1 ? '' : 's'}</span></button>)}
            </div>
          </details>
        )}
        <button id="nav-new-agent" className="nav-item nav-new" onClick={() => selectSetup()}><span className="nav-sigil">＋</span> new agent</button>
      </div>
      <div id="history-hint" className="nav-hint nav-foot" hidden={historyOk !== false}>{historyHint(historyState)}</div>
    </nav>
  );
}

export default Nav;
