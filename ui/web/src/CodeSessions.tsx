// Observability M4: the coding-session tree.
//
// A self-contained view over the M2 read API (/api/code/sessions and
// /api/code/sessions/<id>, served by the relay proxying the elanus CLI over the
// M1 sqlite projection). It renders the nested spawner->worker tree with
// per-session stats, and a detail panel with a paste-able resume command and the
// event timeline. Kept in its own file (and styled with a scoped <style> block)
// so it can be dropped into whatever nav/"Workers" surface owns it without
// colliding with the rest of App.tsx.
import { useEffect, useRef, useState } from 'react';
import { openLiveStream } from './live';

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
  // TG3: thread-grouping fields. Optional so older payloads still render.
  // incarnations = constituent elanus_session ids (newest first);
  // relaunches = manual re-launches (incarnations - 1);
  // driven_resumes = daemon-driven resume_count sum.
  incarnations?: string[];
  relaunches?: number;
  driven_resumes?: number;
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
  const [expanded, setExpanded] = useState(false);
  const kids = (childrenOf.get(stat.elanus_session) ?? [])
    .slice()
    .sort((a, b) => statusRank(a.last_status) - statusRank(b.last_status) || (b.started_at ?? '').localeCompare(a.started_at ?? ''));
  const incarnations = stat.incarnations ?? [];
  const isThread = incarnations.length > 1;
  return (
    <div className="cs-node" style={{ marginLeft: depth ? 16 : 0 }}>
      <div
        className={`cs-row${selected === stat.elanus_session ? ' cs-sel' : ''}`}
        onClick={() => onSelect(stat.elanus_session)}
      >
        {isThread && (
          <span
            className="cs-toggle"
            role="button"
            aria-expanded={expanded}
            title={`${incarnations.length} incarnations (click to ${expanded ? 'collapse' : 'expand'})`}
            onClick={(e) => {
              e.stopPropagation();
              setExpanded((v) => !v);
            }}
          >
            {expanded ? '▾' : '▸'}
          </span>
        )}
        <span className="cs-id">{stat.elanus_session}</span>
        <span className="cs-tool">{stat.tool ?? '?'}</span>
        <span className="cs-dim">{(stat.model ?? '?') + ' / ' + (stat.effort ?? '?')}</span>
        <StatusBadge status={stat.last_status} />
        <span className="cs-dim">{humanDuration(stat.duration_ms)}</span>
        {stat.resume_count > 0 && <span className="cs-dim">↻{stat.resume_count}</span>}
        {isThread && <span className="cs-dim cs-thread" title="incarnations in this thread">×{incarnations.length}</span>}
        <span className="cs-dim">
          {humanTokens(stat.input_tokens)}↓ {humanTokens(stat.output_tokens)}↑
        </span>
      </div>
      {isThread && expanded && (
        <div className="cs-incs">
          {incarnations.map((id, i) => (
            <div key={id} className="cs-inc">
              <span className="cs-inc-i">{i === 0 ? 'newest' : i === incarnations.length - 1 ? 'oldest' : `-${i}`}</span>
              <span className="cs-id">{id}</span>
            </div>
          ))}
        </div>
      )}
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

// ---------------------------------------------------------------------------
// M3 live fold. The materializer (src/code_projection.rs) folds coding obs into
// the sqlite projection; here we mirror that fold over the live bus tail so a
// session that started before the page loaded (sqlite backfill) and one that
// starts while watching (live) render identically.
//
// A LivePatch is the accumulated effect of the live events seen for one
// elanus_session since the last sqlite backfill that already reflected them.
// Idempotency: the obs envelope on the bus carries NO per-event id, but the web
// relay stamps every formed SSE message with a server-side monotonic `seq`
// (server.mjs) that is stable across the ring-buffer replay it sends to each
// (re)connecting EventSource. We apply each seq at most once (seenSeqs ref-set,
// mirroring App.tsx seenAsks/seenFailures), so an at-least-once / replayed
// delivery — e.g. the whole ring re-broadcast on a transient SSE reconnect —
// never double-counts tokens, resumes, or status. On every backfill we DROP the
// live patches AND clear seenSeqs — the fresh sqlite row is the authority for
// everything the materializer has already folded, and only events newer than
// that backfill survive (re-folded from the still-open stream going forward).
// Live is strictly additive on top of sqlite.
type LivePatch = {
  elanus_session: string;
  agent_noun?: string | null;
  tool?: string | null;
  workdir?: string | null;
  model?: string | null;
  effort?: string | null;
  parent?: string | null;
  native_session?: string | null;
  started_at?: string | null;
  ended_at?: string | null;
  exit_code?: number | null;
  last_status?: string | null;
  input_tokens: number; // delta accumulated from live session/idle events
  output_tokens: number; // delta
  resume_count: number; // delta from live session/resume events
  updated_at?: string | null;
  saw_event: boolean; // a non-lifecycle leaf (tool/assistant/...) bumps activity
};

// Parse a coding-session obs topic: obs/agent/<noun>/<elanus_session>/<leaf>.
// Mirrors parsed_topic() in the materializer (codex | claude-code; code-* id).
function parseCodingTopic(topic: string): { noun: string; session: string; leaf: string } | null {
  const rest = topic.startsWith('obs/agent/') ? topic.slice('obs/agent/'.length) : null;
  if (!rest) return null;
  const slash1 = rest.indexOf('/');
  if (slash1 < 0) return null;
  const noun = rest.slice(0, slash1);
  if (noun !== 'codex' && noun !== 'claude-code') return null;
  const slash2 = rest.indexOf('/', slash1 + 1);
  if (slash2 < 0) return null;
  const session = rest.slice(slash1 + 1, slash2);
  if (!session.startsWith('code-')) return null;
  const leaf = rest.slice(slash2 + 1);
  if (!leaf) return null;
  return { noun, session, leaf };
}

function num(v: any): number {
  const n = typeof v === 'number' ? v : Number(v);
  return Number.isFinite(n) ? n : 0;
}

// Fold one live obs event into the patch map (mutating a fresh copy). Returns the
// next map. Token/resume effects accumulate as deltas; lifecycle leaves set the
// terminal status; everything else just marks activity (keeps a card "running").
function foldLive(prev: Map<string, LivePatch>, topic: string, env: any): Map<string, LivePatch> {
  const parsed = parseCodingTopic(topic);
  if (!parsed) return prev;
  const { noun, session, leaf } = parsed;
  const payload = env?.payload && typeof env.payload === 'object' ? env.payload : {};
  const ts: string | null = (typeof payload.ts === 'string' ? payload.ts : null) ?? env?.ts ?? null;
  const next = new Map(prev);
  const p: LivePatch = {
    ...(next.get(session) ?? {
      elanus_session: session,
      input_tokens: 0,
      output_tokens: 0,
      resume_count: 0,
      saw_event: false,
    }),
  };
  p.agent_noun = p.agent_noun ?? noun;
  if (ts) p.updated_at = ts;
  switch (leaf) {
    case 'session/start':
      p.tool = p.tool ?? (typeof payload.tool === 'string' ? payload.tool : null);
      p.workdir = p.workdir ?? (typeof payload.workdir === 'string' ? payload.workdir : null);
      p.model = p.model ?? (typeof payload.model === 'string' ? payload.model : null);
      p.effort = p.effort ?? (typeof payload.effort === 'string' ? payload.effort : null);
      p.parent = p.parent ?? (typeof payload.parent === 'string' ? payload.parent : null);
      p.started_at = p.started_at ?? ts;
      p.last_status = 'running';
      break;
    case 'session/thread':
      if (noun === 'codex' && typeof payload.codex_thread === 'string') p.native_session = payload.codex_thread;
      break;
    case 'session/started':
      if (noun === 'claude-code' && typeof payload.cc_session === 'string') p.native_session = payload.cc_session;
      break;
    case 'session/resume':
      p.resume_count += 1;
      p.last_status = 'running';
      break;
    case 'session/idle':
      p.input_tokens += num(payload?.usage?.input_tokens);
      p.output_tokens += num(payload?.usage?.output_tokens);
      p.last_status = 'idle';
      break;
    case 'session/stop':
      p.ended_at = ts;
      if (payload.exit_code != null) p.exit_code = num(payload.exit_code);
      p.last_status = 'done';
      break;
    default:
      // tool/<...>, assistant/message, tokens, etc. — activity only.
      p.saw_event = true;
      if (p.last_status == null || p.last_status === 'done') p.last_status = 'running';
      break;
  }
  next.set(session, p);
  return next;
}

// Merge the sqlite backfill with the live patches. Idempotent by construction:
// the backfill row is the base; a patch only ADDS token/resume deltas it has
// accumulated since that row and advances status/lifecycle fields. A session
// seen only live (started while watching, not yet in sqlite) is synthesized from
// its patch alone, so it renders the same shape as a backfilled row.
function mergeLive(backfill: Stat[], patches: Map<string, LivePatch>): Stat[] {
  const byId = new Map<string, Stat>();
  for (const s of backfill) byId.set(s.elanus_session, { ...s });
  for (const [id, p] of patches) {
    const base = byId.get(id);
    if (base) {
      const merged: Stat = { ...base };
      merged.input_tokens = base.input_tokens + p.input_tokens;
      merged.output_tokens = base.output_tokens + p.output_tokens;
      merged.resume_count = base.resume_count + p.resume_count;
      if (p.native_session && !merged.native_session) merged.native_session = p.native_session;
      if (p.parent && !merged.parent) merged.parent = p.parent;
      // Lifecycle: 'done' is terminal and wins; otherwise the live status (the
      // newer signal) takes precedence over the backfill's older status.
      if (p.last_status) {
        if (p.last_status === 'done') {
          merged.last_status = 'done';
          merged.ended_at = p.ended_at ?? merged.ended_at;
          if (p.exit_code != null) merged.exit_code = p.exit_code;
        } else if (merged.last_status !== 'done') {
          merged.last_status = p.last_status;
        }
      }
      if (p.updated_at && (!merged.updated_at || p.updated_at > merged.updated_at)) merged.updated_at = p.updated_at;
      merged.duration_ms = computeDuration(merged);
      byId.set(id, merged);
    } else {
      // Live-only session not yet in sqlite — synthesize a full row.
      const syn: Stat = {
        elanus_session: id,
        tool: p.tool ?? null,
        agent_noun: p.agent_noun ?? null,
        native_session: p.native_session ?? null,
        workdir: p.workdir ?? null,
        model: p.model ?? null,
        effort: p.effort ?? null,
        parent: p.parent ?? null,
        started_at: p.started_at ?? null,
        ended_at: p.ended_at ?? null,
        exit_code: p.exit_code ?? null,
        last_status: p.last_status ?? (p.saw_event ? 'running' : null),
        resume_count: p.resume_count,
        input_tokens: p.input_tokens,
        output_tokens: p.output_tokens,
        updated_at: p.updated_at ?? null,
        duration_ms: null,
      };
      syn.duration_ms = computeDuration(syn);
      byId.set(id, syn);
    }
  }
  return [...byId.values()];
}

// Derive duration the same way the read API does (start→end, or start→now while
// running) so a live-only card shows a ticking duration consistent with sqlite.
function computeDuration(s: Stat): number | null {
  if (!s.started_at) return s.duration_ms ?? null;
  const start = Date.parse(s.started_at);
  if (!Number.isFinite(start)) return s.duration_ms ?? null;
  const endRef = s.ended_at ? Date.parse(s.ended_at) : Date.now();
  if (!Number.isFinite(endRef)) return s.duration_ms ?? null;
  return Math.max(0, endRef - start);
}

// agent-comms-ui M5: render one estimate-vs-actual dimension. Dollars are
// best-effort: when `unavailable`, the actual + variance render "unknown", NEVER a
// fabricated number (matching the setup view's "no dollars until pricing is
// known" stance). The non-dollar dims (turns/tokens/wall-clock) lead.
function EstimateRow({
  label,
  v,
  unit,
  wall,
  unavailable,
}: {
  label: string;
  v: { estimate?: number | null; actual?: number | null; delta?: number | null };
  unit?: string;
  wall?: boolean;
  unavailable?: boolean;
}) {
  const fmt = (n: number | null | undefined): string => {
    if (n == null) return '—';
    if (wall) return humanDuration(n);
    if (unit === '$') return `$${n.toFixed(2)}`;
    return Number.isInteger(n) ? String(n) : n.toFixed(1);
  };
  // For dollars with no pricing source, never invent a figure (decision 4).
  const actualUnknown = unit === '$' && unavailable;
  const delta = v.delta;
  const overUnder = delta == null ? '' : delta > 0 ? ' over' : delta < 0 ? ' under' : ' on';
  const deltaCls = delta == null ? 'cs-dim' : delta > 0 ? 'cs-over' : delta < 0 ? 'cs-under' : 'cs-dim';
  return (
    <>
      <span className="cs-evt-label">{label}</span>
      <span>{fmt(v.estimate)}</span>
      <span>{actualUnknown ? <span className="cs-dim">unknown</span> : fmt(v.actual)}</span>
      <span className={deltaCls}>
        {actualUnknown || delta == null ? '—' : `${delta > 0 ? '+' : ''}${fmt(delta)}${overUnder}`}
      </span>
    </>
  );
}

// agent-comms-ui M5: one estimate-vs-actual dimension as the route returns it.
type Variance = { estimate?: number | null; actual?: number | null; delta?: number | null };
type EstimateReport = {
  session: string;
  dollars: Variance;
  turns: Variance;
  tool_calls: Variance;
  tokens: Variance;
  wall_clock_ms: Variance;
  dollars_unavailable: boolean;
};

// agent-comms-ui M4: one block as the inspector route returns it.
type BlockRow = {
  name: string;
  scope: string;
  placement: string;
  priority: number;
  owner: string;
  content: string;
  ephemeral: boolean;
};

// The block-inspector inline editor (the documented follow-on to M4's read-only
// inspector). A DURABLE block (identity/learned/note — it carries an owner) gets an
// edit affordance: a textarea + save that POSTs the new content to `/api/blocks`
// (through the origin_ok CSRF guard, stamping `--by ui` server-side) and reflects
// the persisted value. An EPHEMERAL inbox/channel block is owner-less, computed each
// turn, and never stored — it renders read-only with NO editor (decision 2/3).
function BlockCard({
  block,
  session,
  onSaved,
}: {
  block: BlockRow;
  session: string;
  onSaved: (rows: BlockRow[]) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(block.content);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState('');
  // Only durable blocks are editable. Ephemeral blocks have no owner and are
  // session-computed, so there is nothing to persist — show them read-only.
  const editable = !block.ephemeral && !!block.owner;

  const save = async () => {
    setSaving(true);
    setErr('');
    try {
      const r = await fetch('/api/blocks', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          session,
          name: block.name,
          owner: block.owner,
          scope: block.scope,
          placement: block.placement,
          priority: block.priority,
          content: draft,
        }),
      });
      const d = await r.json().catch(() => ({}));
      if (!r.ok || d.ok === false) {
        throw new Error(d.error ?? `HTTP ${r.status}`);
      }
      // The route re-reads the durable blocks so the panel reflects the persisted
      // value; fall back to a local content patch if the route didn't echo them.
      if (Array.isArray(d.blocks)) {
        onSaved(d.blocks as BlockRow[]);
      }
      setEditing(false);
    } catch (e) {
      setErr(String((e as Error).message ?? e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={`cs-block${block.ephemeral ? ' cs-block-eph' : ''}`} data-block={block.name}>
      <div className="cs-block-head">
        <span className="cs-block-name">{block.name}</span>
        <span className="cs-block-scope">{block.scope}/{block.placement}</span>
        <span className="cs-dim">p{block.priority}</span>
        {block.ephemeral
          ? <span className="cs-block-tag" title="computed each turn, never written to context_blocks">live, not stored</span>
          : block.owner && <span className="cs-dim">{block.owner}</span>}
        {editable && !editing && (
          <button
            className="cs-btn cs-block-edit"
            data-block-edit={block.name}
            onClick={() => {
              setDraft(block.content);
              setEditing(true);
              setErr('');
            }}
          >
            edit
          </button>
        )}
      </div>
      {editing ? (
        <div className="cs-block-editor">
          <textarea
            className="cs-block-textarea"
            data-block-textarea={block.name}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            rows={5}
          />
          <div className="cs-block-actions">
            <button
              className="cs-btn cs-block-save"
              data-block-save={block.name}
              disabled={saving}
              onClick={save}
            >
              {saving ? 'saving…' : 'save'}
            </button>
            <button
              className="cs-btn"
              disabled={saving}
              onClick={() => {
                setEditing(false);
                setErr('');
              }}
            >
              cancel
            </button>
            {err && <span className="cs-err" data-block-error={block.name}>{err}</span>}
          </div>
        </div>
      ) : (
        <div className="cs-block-content">{block.content}</div>
      )}
    </div>
  );
}

export default function CodeSessions({ focus }: { focus?: string } = {}) {
  const [backfill, setBackfill] = useState<Stat[]>([]);
  const [livePatches, setLivePatches] = useState<Map<string, LivePatch>>(new Map());
  const [error, setError] = useState('');
  const [selected, setSelected] = useState<string | null>(focus ?? null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [copied, setCopied] = useState(false);
  // M5: the estimate-vs-actual report for the selected session, or null when it
  // recorded no estimate (the group is simply omitted — no crash).
  const [estimate, setEstimate] = useState<EstimateReport | null>(null);
  // M4: the selected session's memory blocks (durable + recomputed ephemeral).
  const [blocks, setBlocks] = useState<BlockRow[]>([]);

  // M2 cross-link: when the runs view is opened focused on a comms participant,
  // select that session (and re-select if the focus changes).
  useEffect(() => {
    if (focus) setSelected(focus);
  }, [focus]);
  // The newest updated_at across the live patches, captured at backfill time so
  // we know the projection has caught up. Used only to gate detail refresh.
  const liveSeq = useRef(0);
  // Per-delivery dedup: the set of server SSE `seq` values already folded since
  // the last backfill. The ring buffer is re-broadcast on every EventSource
  // reconnect, so without this the same session/idle / session/resume frames
  // would be re-folded and double-count tokens/resumes on live cards.
  const seenSeqs = useRef<Set<number>>(new Set());

  // Backfill from sqlite (M2). This is the source of truth for HISTORY — on first
  // mount and on an infrequent repair tick (the live feed carries the fast path
  // now, so this is just reconciliation, not the 5s stand-in poll). Each backfill
  // CLEARS the live patches: the fresh sqlite row already folds every event the
  // materializer has processed, so replaying those patches would double-count.
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const r = await fetch('/api/code/sessions');
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        const data = (await r.json()) as Stat[];
        if (alive) {
          setBackfill(Array.isArray(data) ? data : []);
          // Reconcile: drop accumulated live deltas now reflected in sqlite, and
          // forget the seqs we deduped against — they are folded into this row
          // now, and seqs newer than this backfill will be applied fresh.
          setLivePatches(new Map());
          seenSeqs.current = new Set();
          setError('');
        }
      } catch (e) {
        if (alive) setError(String((e as Error).message ?? e));
      }
    };
    load();
    const t = setInterval(load, 30000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  // M3: subscribe to the SAME live SSE feed App.tsx uses (/api/stream via
  // openLiveStream). Fold coding obs deltas into livePatches; ignore everything
  // else. Shares the relay transport — no new endpoint, no new socket protocol.
  useEffect(() => {
    const es = openLiveStream(
      (m: any) => {
        if (m.kind !== 'message' || typeof m.topic !== 'string') return;
        if (!parseCodingTopic(m.topic)) return;
        // Apply each server `seq` at most once. The ring is re-broadcast on every
        // SSE reconnect; without this guard the additive token/resume deltas in
        // foldLive would be re-applied and inflate live cards. (Frames lacking a
        // seq fall through and are folded — the relay always stamps one today.)
        if (typeof m.seq === 'number') {
          if (seenSeqs.current.has(m.seq)) return;
          seenSeqs.current.add(m.seq);
        }
        setLivePatches((prev) => {
          const next = foldLive(prev, m.topic, m.env);
          if (next !== prev) liveSeq.current += 1;
          return next;
        });
      },
      () => {
        /* transient relay drop — the backfill tick still reconciles state */
      },
    );
    return () => es.close();
  }, []);

  // The rendered set: sqlite history with live deltas merged on top.
  const sessions = mergeLive(backfill, livePatches);

  // Load detail (from sqlite M2) when a session is selected, and refresh it when
  // the backfill changes OR the selected session sees new live activity. We key
  // on the selected patch's updated_at (a primitive) rather than the `sessions`
  // array identity — which is fresh every render and would refetch in a loop.
  const selectedLiveStamp = selected ? livePatches.get(selected)?.updated_at ?? '' : '';
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
  }, [selected, backfill, selectedLiveStamp]);

  // M5 + M4: load the estimate-vs-actual report and the memory blocks for the
  // selected session. Both are read routes that shell the CLI (no new transport);
  // a session with no estimate returns `null` (group omitted), a session with no
  // blocks returns `[]`. Refreshed alongside the detail (on selection + backfill).
  useEffect(() => {
    if (!selected) {
      setEstimate(null);
      setBlocks([]);
      return;
    }
    let alive = true;
    fetch(`/api/estimate/${encodeURIComponent(selected)}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => alive && setEstimate(d && typeof d === 'object' && 'session' in d ? (d as EstimateReport) : null))
      .catch(() => alive && setEstimate(null));
    fetch(`/api/blocks?session=${encodeURIComponent(selected)}`)
      .then((r) => (r.ok ? r.json() : []))
      .then((d) => alive && setBlocks(Array.isArray(d) ? d : []))
      .catch(() => alive && setBlocks([]));
    return () => {
      alive = false;
    };
  }, [selected, backfill, selectedLiveStamp]);

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

      {detail && (() => {
        // Overlay the live-folded row (status/tokens/duration/resumes) onto the
        // sqlite detail so the header badge stays consistent with the live card
        // even between backfills — same merge the list uses, keyed by id.
        const live = sessions.find((s) => s.elanus_session === detail.session.elanus_session);
        const ds: Stat = live ? { ...detail.session, ...live, relaunches: detail.session.relaunches, driven_resumes: detail.session.driven_resumes, incarnations: detail.session.incarnations } : detail.session;
        return (
        <div className="cs-detail">
          <h3 className="cs-h">{ds.elanus_session}</h3>
          <div className="cs-kv">
            <span>tool</span><b>{ds.tool ?? '?'}</b>
            <span>model / effort</span><b>{(ds.model ?? '?') + ' / ' + (ds.effort ?? '?')}</b>
            <span>status</span><b><StatusBadge status={ds.last_status} /></b>
            <span>duration</span><b>{humanDuration(ds.duration_ms)}</b>
            <span>tokens</span><b>{humanTokens(ds.input_tokens)} in / {humanTokens(ds.output_tokens)} out</b>
            {ds.relaunches != null || ds.driven_resumes != null ? (
              <>
                <span>relaunches</span>
                <b>{ds.relaunches ?? 0} <span className="cs-dim">manual</span></b>
                <span>driven resumes</span>
                <b>{ds.driven_resumes ?? 0} <span className="cs-dim">daemon</span></b>
              </>
            ) : (
              <><span>resumes</span><b>{ds.resume_count}</b></>
            )}
            {ds.parent && (<><span>parent</span><b className="cs-id">{ds.parent}</b></>)}
            {ds.workdir && (<><span>workdir</span><b className="cs-id">{ds.workdir}</b></>)}
          </div>

          <div className="cs-resume">
            <code>{detail.resume_command}</code>
            <button className="cs-btn" onClick={() => copyResume(detail.resume_command)}>{copied ? 'copied' : 'copy'}</button>
          </div>

          {/* M5: estimate vs actual — only when the session recorded an estimate. */}
          {estimate && estimate.session === detail.session.elanus_session && (
            <div className="cs-sub cs-estimate" id="cs-estimate">
              <div className="cs-dim">estimate vs actual:</div>
              <div className="cs-evt-grid">
                <span className="cs-evt-h" />
                <span className="cs-evt-h">est</span>
                <span className="cs-evt-h">actual</span>
                <span className="cs-evt-h">variance</span>
                <EstimateRow label="dollars" v={estimate.dollars} unit="$" unavailable={estimate.dollars_unavailable} />
                <EstimateRow label="turns" v={estimate.turns} />
                <EstimateRow label="tool calls" v={estimate.tool_calls} />
                <EstimateRow label="tokens" v={estimate.tokens} />
                <EstimateRow label="wall-clock" v={estimate.wall_clock_ms} wall />
              </div>
            </div>
          )}

          {/* M4: memory-block inspector (read-only). Durable identity/learned
              blocks + the recomputed ephemeral inbox/channel (clearly labeled
              "live, not stored" — they are session-computed and owner-less). */}
          {blocks.length > 0 && (
            <div className="cs-sub cs-blocks" id="cs-blocks">
              <div className="cs-dim">memory blocks ({blocks.length}):</div>
              {blocks.map((b, i) => (
                <BlockCard
                  key={`${b.name}:${i}`}
                  block={b}
                  session={selected!}
                  onSaved={(rows) => setBlocks(rows)}
                />
              ))}
            </div>
          )}

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
        );
      })()}
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
.cs-toggle { cursor: pointer; user-select: none; width: 12px; text-align: center; color: #8a8a8a; }
.cs-toggle:hover { color: #ddd; }
.cs-thread { cursor: help; }
.cs-incs { margin: 2px 0 4px 18px; padding: 2px 0 2px 8px; border-left: 1px solid #333; display: flex; flex-direction: column; gap: 1px; font-size: 11px; }
.cs-inc { display: flex; gap: 8px; align-items: baseline; color: #8a8a8a; }
.cs-inc-i { font-size: 9px; min-width: 48px; text-transform: uppercase; letter-spacing: 0.04em; opacity: 0.8; }
.cs-evt-grid { display: grid; grid-template-columns: auto auto auto auto; gap: 2px 14px; font-size: 11px; margin-top: 3px; align-items: baseline; }
.cs-evt-h { color: #8a8a8a; font-size: 10px; text-transform: uppercase; letter-spacing: 0.04em; }
.cs-evt-label { color: #c8c8c8; }
.cs-over { color: #ffb27a; }
.cs-under { color: #8fe0a0; }
.cs-block { border: 1px solid #2a2a2a; border-radius: 5px; padding: 5px 7px; margin: 3px 0; font-size: 11px; }
.cs-block-eph { border-style: dashed; border-color: #4a3f6a; }
.cs-block-head { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
.cs-block-name { font-family: ui-monospace, monospace; font-weight: 600; }
.cs-block-scope { color: #8a8a8a; font-size: 10px; }
.cs-block-tag { font-size: 9px; padding: 1px 5px; border-radius: 7px; background: #3a2f5a; color: #d8c8ff; }
.cs-block-content { color: #b8b8b8; white-space: pre-wrap; margin-top: 2px; max-height: 120px; overflow-y: auto; }
.cs-block-edit { margin-left: auto; font-size: 10px; padding: 1px 7px; }
.cs-block-editor { margin-top: 4px; }
.cs-block-textarea { width: 100%; box-sizing: border-box; font-family: ui-monospace, monospace; font-size: 11px; background: #141414; color: #ddd; border: 1px solid #3a3a3a; border-radius: 4px; padding: 5px 6px; resize: vertical; }
.cs-block-actions { display: flex; gap: 8px; align-items: center; margin-top: 4px; }
`;
