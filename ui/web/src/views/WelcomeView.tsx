import AgentChip from '../components/AgentChip';

function historyHint(historyState: string | null | undefined): string {
  if (historyState === 'absent') return 'History is not running. Start or allow it to use transcripts.';
  if (historyState === 'unreachable') return 'History is allowed but not answering. Restart it to use transcripts.';
  return 'History is unavailable right now.';
}

function WelcomeView({ hidden, primary, historyOk, historyState, systemStatus, selectAgent, selectSetup, selectSignals }: any) {
  const healthy = systemStatus?.credential === 'present' && (systemStatus?.broker_connected || false);
  return (
    <div id="view-welcome" className="view" hidden={hidden}>
      <div className="welcome-pane">
        <p className="welcome-lead">Set up an agent, check everything’s running, and open one when you need the details.</p>
        <div className={`welcome-health ${healthy ? 'ok' : 'warn'}`}>
          <span>{healthy ? 'local stack looks ready' : 'setup needs attention'}</span>
          <span>{systemStatus?.root ?? 'checking root...'}</span>
        </div>
        <div id="welcome-agent" className="welcome-agent">
          {!primary ? <div className="dim-note">no agents yet — create your first one.</div> : (
            <>
              <div className="welcome-agent-label">your agent</div>
              <div className="welcome-agent-row">
                <AgentChip name={primary} size="md" />
                <span className="welcome-agent-name">{primary}</span>
                <button onClick={() => selectAgent(primary, 'converse')}>chat with {primary}</button>
                <button className="ghost" onClick={() => selectAgent(primary, 'configure')}>configure</button>
              </div>
            </>
          )}
        </div>
        <div className="welcome-actions">
          <button id="welcome-new" className="ghost" onClick={() => selectSetup()}>＋ guided setup</button>
          <button id="welcome-signals" className="ghost" onClick={selectSignals}>◮ activity</button>
        </div>
        {historyOk === false && <p id="welcome-hint" className="dim-note">{historyHint(historyState)}</p>}
      </div>
    </div>
  );
}

export default WelcomeView;
