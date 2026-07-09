import { summarize, timeOf } from '../lib/format';

function RailView({ hidden, filter, setFilter, paused, setPaused, rows }: any) {
  const verbClass = (topic: string) => topic.startsWith('signal/') ? 'v-signal' : topic.startsWith('in/') ? 'v-in' : /^obs\/[^/]+\/[^/]+\/[^/]+\/tool\//.test(topic) ? 'v-tool' : 'v-obs';
  const empty = !paused && rows.length === 0;
  const filtered = filter !== 'all';
  return (
    <div id="view-rail" className="view" hidden={hidden}>
      <div className="rail-bar"><div className="tele-filters" aria-label="activity filters">{['all', 'work', 'tools', 'signals'].map((f) => <button key={f} data-f={f} aria-pressed={filter === f} className={filter === f ? 'on' : ''} onClick={() => setFilter(f)}>{f}</button>)}<button id="tele-pause" title="pause the feed" aria-pressed={paused} onClick={() => setPaused(!paused)}>{paused ? '▶' : '⏸'}</button></div></div>
      <div id="tele-feed" className="tele-feed" aria-live="off" aria-label="live activity stream">
        {empty && (
          <div className="rail-empty">
            <p className="rail-empty-mark" aria-hidden="true">≡</p>
            <p>{paused ? 'feed paused — press ▶ to resume.' : filtered ? `nothing has arrived on ${filter} yet. this view updates as the agent works.` : 'nothing has arrived yet. this view updates as the agent runs — tool calls, replies, and signals land here live.'}</p>
          </div>
        )}
        {!paused && rows.map((m: any, i: number) => <div key={`${i}-${m.topic}-${m.env?.id ?? ''}`} className={`row ${verbClass(m.topic)}`}><span className="t">{timeOf(m.env)}</span><span><span className="topic">{m.topic} </span><span className="pay">{summarize(m.env?.payload)}</span></span></div>)}
      </div>
    </div>
  );
}

export default RailView;
