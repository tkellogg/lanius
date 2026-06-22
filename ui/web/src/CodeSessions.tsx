// Observability M4: the coding-session tree.
//
// A self-contained view over the M2 read API (/api/code/sessions and
// /api/code/sessions/<id>, served by the relay proxying the elanus CLI over the
// M1 sqlite projection). It renders the nested spawner->worker tree with
// per-session stats, and a detail panel with a paste-able resume command and the
// event timeline. Kept in its own file (and styled with a scoped <style> block)
// so it can be dropped into whatever nav/"Workers" surface owns it without
// colliding with the rest of App.tsx.
import { useEffect, useState } from 'react';

type Stat = {
  elanus_session: string;
  tool: string | null;
  agent_noun: string | null;
  native_session: string | null;
  workdir: string | null;
  model: string | null;
  effort: string | null;
  parent: string | null;
  started_at: string | null;
  ended_at: string | null;
  exit_code: number | null;
  last_status: string | null;
  resume_count: number;
  input_tokens: number;
  output_tokens: number;
  updated_at: string | null;
  duration_ms: number | null;
};

type Ev = { id: number; ts: string | null; kind: string | null; summary: string | null };
type Detail = { session: Stat; events: Ev[]; resume_command: string; children: Stat[] };

function humanDuration(ms: number | null): string {
  if (ms == null || ms < 0) return '—';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

function humanTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function statusRank(s: string | null): number {
  if (s === 'running' || s === 'idle') return 0;
  if (s === 'done') return 1;
  return 2;
}

function StatusBadge({ status }: { status: string | null }) {
  const s = status ?? 'unknown';
  const cls = s === 'running' || s === 'idle' ? 'cs-badge cs-live' : s === 'done' ? 'cs-badge cs-done' : 'cs-badge';
  return <span className={cls}>{s}</span>;
}

// One node in the tree: a session row plus its (recursively rendered) children.
function SessionNode({
  stat,
  childrenOf,
  selected,
  onSelect,
  depth,
}: {
  stat: Stat;
  childrenOf: Map<string, Stat[]>;
  selected: string | null;
  onSelect: (id: string) => void;
  depth: number;
}) {
  const kids = (childrenOf.get(stat.elanus_session) ?? [])
    .slice()
    .sort((a, b) => statusRank(a.last_status) - statusRank(b.last_status) || (b.started_at ?? '').localeCompare(a.started_at ?? ''));
  return (
    <div className="cs-node" style={{ marginLeft: depth ? 16 : 0 }}>
      <div
        className={`cs-row${selected === stat.elanus_session ? ' cs-sel' : ''}`}
        onClick={() => onSelect(stat.elanus_session)}
      >
        <span className="cs-id">{stat.elanus_session}</span>
        <span className="cs-tool">{stat.tool ?? '?'}</span>
        <span className="cs-dim">{(stat.model ?? '?') + ' / ' + (stat.effort ?? '?')}</span>
        <StatusBadge status={stat.last_status} />
        <span className="cs-dim">{humanDuration(stat.duration_ms)}</span>
        {stat.resume_count > 0 && <span className="cs-dim">↻{stat.resume_count}</span>}
        <span className="cs-dim">
          {humanTokens(stat.input_tokens)}↓ {humanTokens(stat.output_tokens)}↑
        </span>
      </div>
      {kids.map((k) => (
        <SessionNode
          key={k.elanus_session}
          stat={k}
          childrenOf={childrenOf}
          selected={selected}
          onSelect={onSelect}
          depth={depth + 1}
        />
      ))}
    </div>
  );
}

export default function CodeSessions() {
  const [sessions, setSessions] = useState<Stat[]>([]);
  const [error, setError] = useState('');
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [copied, setCopied] = useState(false);

  // Poll the list every 5s — a simple stand-in for a live feed until an SSE
  // relay of obs/agent/+/+/# is wired (M3).
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const r = await fetch('/api/code/sessions');
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const data = (await r.json()) as Stat[];
        if (alive) {
          setSessions(Array.isArray(data) ? data : []);
          setError('');
        }
      } catch (e) {
        if (alive) setError(String((e as Error).message ?? e));
      }
    };
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  // Load detail when a session is selected (and refresh it with the list tick).
  useEffect(() => {
    if (!selected) {
      setDetail(null);
      return;
    }
    let alive = true;
    fetch(`/api/code/sessions/${encodeURIComponent(selected)}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => alive && setDetail(d as Detail | null))
      .catch(() => alive && setDetail(null));
    return () => {
      alive = false;
    };
  }, [selected, sessions]);

  // Roots: sessions with no parent, or whose parent is not in the set.
  const ids = new Set(sessions.map((s) => s.elanus_session));
  const childrenOf = new Map<string, Stat[]>();
  for (const s of sessions) {
    if (s.parent && ids.has(s.parent)) {
      const arr = childrenOf.get(s.parent) ?? [];
      arr.push(s);
      childrenOf.set(s.parent, arr);
    }
  }
  const roots = sessions
    .filter((s) => !s.parent || !ids.has(s.parent))
    .sort((a, b) => statusRank(a.last_status) - statusRank(b.last_status) || (b.started_at ?? '').localeCompare(a.started_at ?? ''));

  const copyResume = (cmd: string) => {
    navigator.clipboard?.writeText(cmd).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  };

  return (
    <div className="cs-wrap">
      <style>{CS_STYLE}</style>
      <div className="cs-tree">
        <h3 className="cs-h">Coding runs</h3>
        {error && <div className="cs-err">projection unavailable: {error}</div>}
        {!error && sessions.length === 0 && (
          <div className="cs-dim">No coding sessions yet. (Run `elanus code project` to refresh, or start a worker.)</div>
        )}
        {roots.map((s) => (
          <SessionNode
            key={s.elanus_session}
            stat={s}
            childrenOf={childrenOf}
            selected={selected}
            onSelect={setSelected}
            depth={0}
          />
        ))}
      </div>

      {detail && (
        <div className="cs-detail">
          <h3 className="cs-h">{detail.session.elanus_session}</h3>
          <div className="cs-kv">
            <span>tool</span><b>{detail.session.tool ?? '?'}</b>
            <span>model / effort</span><b>{(detail.session.model ?? '?') + ' / ' + (detail.session.effort ?? '?')}</b>
            <span>status</span><b><StatusBadge status={detail.session.last_status} /></b>
            <span>duration</span><b>{humanDuration(detail.session.duration_ms)}</b>
            <span>tokens</span><b>{humanTokens(detail.session.input_tokens)} in / {humanTokens(detail.session.output_tokens)} out</b>
            <span>resumes</span><b>{detail.session.resume_count}</b>
            {detail.session.parent && (<><span>parent</span><b className="cs-id">{detail.session.parent}</b></>)}
            {detail.session.workdir && (<><span>workdir</span><b className="cs-id">{detail.session.workdir}</b></>)}
          </div>

          <div className="cs-resume">
            <code>{detail.resume_command}</code>
            <button className="cs-btn" onClick={() => copyResume(detail.resume_command)}>{copied ? 'copied' : 'copy'}</button>
          </div>

          {detail.children.length > 0 && (
            <div className="cs-sub">
              <div className="cs-dim">spawned workers:</div>
              {detail.children.map((c) => (
                <div key={c.elanus_session} className="cs-row" onClick={() => setSelected(c.elanus_session)}>
                  <span className="cs-id">{c.elanus_session}</span>
                  <span className="cs-tool">{c.tool ?? '?'}</span>
                  <StatusBadge status={c.last_status} />
                </div>
              ))}
            </div>
          )}

          <div className="cs-sub">
            <div className="cs-dim">timeline ({detail.events.length}):</div>
            <div className="cs-timeline">
              {detail.events.map((e) => (
                <div key={e.id} className="cs-ev">
                  <span className="cs-dim">{e.ts ?? ''}</span>
                  <span className="cs-evkind">{e.kind ?? '?'}</span>
                  {e.summary && <span className="cs-evsum">{e.summary}</span>}
                </div>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Scoped styles so this view needs no edit to the shared styles.css.
const CS_STYLE = `
.cs-wrap { display: flex; gap: 16px; align-items: flex-start; }
.cs-tree { flex: 1 1 60%; min-width: 0; }
.cs-detail { flex: 1 1 40%; min-width: 0; border-left: 1px solid #2a2a2a; padding-left: 16px; }
.cs-h { margin: 0 0 8px; font-size: 14px; }
.cs-row { display: flex; gap: 10px; align-items: center; padding: 4px 6px; border-radius: 4px; cursor: pointer; font-size: 12px; flex-wrap: wrap; }
.cs-row:hover { background: rgba(255,255,255,0.05); }
.cs-sel { background: rgba(120,160,255,0.15); }
.cs-id { font-family: ui-monospace, monospace; }
.cs-tool { font-weight: 600; }
.cs-dim { color: #8a8a8a; }
.cs-badge { font-size: 10px; padding: 1px 6px; border-radius: 8px; background: #333; color: #ddd; }
.cs-live { background: #1f6f3f; color: #d8ffe8; }
.cs-done { background: #3a3a3a; color: #bbb; }
.cs-err { color: #ff8a8a; font-size: 12px; }
.cs-kv { display: grid; grid-template-columns: auto 1fr; gap: 2px 12px; font-size: 12px; margin-bottom: 10px; }
.cs-kv span { color: #8a8a8a; }
.cs-resume { display: flex; gap: 8px; align-items: center; margin-bottom: 10px; }
.cs-resume code { font-family: ui-monospace, monospace; font-size: 11px; background: #1a1a1a; padding: 4px 6px; border-radius: 4px; flex: 1; }
.cs-btn { font-size: 11px; padding: 3px 8px; cursor: pointer; }
.cs-sub { margin-top: 10px; }
.cs-timeline { max-height: 320px; overflow-y: auto; font-size: 11px; }
.cs-ev { display: flex; gap: 8px; padding: 1px 0; }
.cs-evkind { font-family: ui-monospace, monospace; }
.cs-evsum { color: #8a8a8a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
`;
