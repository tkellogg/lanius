import { useState } from 'react';
import { summarize, timeOf } from '../lib/format';

// A stable per-event identity for the disclosure open-state (chrome-polish M3).
// The ledger event id is the true identity; a live-only frame carries the relay's
// monotonic `seq` (stable across the ring re-broadcast on reconnect); otherwise a
// composite of topic+timestamp. Keyed by identity — NOT the buffer index — so an
// expanded row keeps its content as the feed appends and the ring slides.
function eventKey(m: any): string {
  const id = m?.env?.id;
  if (id != null && id !== '') return `id:${id}`;
  if (typeof m?.seq === 'number') return `seq:${m.seq}`;
  return `t:${m?.topic ?? ''}:${m?.env?.ts ?? ''}:${m?.env?.correlation_id ?? ''}`;
}

function RailView({ hidden, filter, setFilter, paused, setPaused, rows }: any) {
  // Which rows are expanded, keyed by event identity so expansion survives the
  // live feed appending and the buffer sliding (a re-render never reshuffles it).
  const [open, setOpen] = useState<Set<string>>(new Set());
  const toggle = (k: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(k)) next.delete(k);
      else next.add(k);
      return next;
    });
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
        {!paused && rows.map((m: any) => {
          const k = eventKey(m);
          const isOpen = open.has(k);
          // Collapsed rendering stays cheap (600 rows): exactly today's one-liner,
          // with summarize() only. The full JSON is stringified ONLY when expanded.
          return (
            <div key={k} className="rail-item">
              <div
                className={`row ${verbClass(m.topic)}${isOpen ? ' row-open' : ''}`}
                role="button"
                tabIndex={0}
                aria-expanded={isOpen}
                title={isOpen ? 'collapse' : 'show full payload'}
                onClick={() => toggle(k)}
                onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(k); } }}
              >
                <span className="t">{timeOf(m.env)}</span>
                <span><span className="topic">{m.topic} </span><span className="pay">{summarize(m.env?.payload)}</span></span>
              </div>
              {isOpen && (
                <pre className="row-json">{JSON.stringify(m.env?.payload ?? m.env ?? {}, null, 2)}</pre>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default RailView;
