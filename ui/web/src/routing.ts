// ── client-side routing (docs/handoffs/web-ui-routing.md) ────────────────────
// This app is one stateful console with a single centralized `sel` selection
// model, not a nested route-loader tree, so per the handoff we hand-roll a small
// History-API router rather than adopt React Router. `selToPath`/`pathToSel` are
// the pure, invertible mapping between a selection and a product-facing URL;
// `App`'s `navigate` wraps `setSel` with `pushState`, and a `popstate` listener
// restores `sel` from `window.location` so Back/Forward and reload/deep-link all
// work. Product-facing, stable paths — names/ids are URI-encoded here and decoded
// defensively in `pathToSel`.
export type AgentTab = 'converse' | 'sessions' | 'telemetry' | 'configure';
export type Sel =
  | { kind: 'welcome' }
  | { kind: 'signals' }
  | { kind: 'setup' }
  | { kind: 'comms' }
  | { kind: 'providers' }
  | { kind: 'code-sessions'; focus?: string }
  | { kind: 'agent'; agent: string; tab: AgentTab };
// agent tab ⇄ URL segment. `converse` is the bare /agents/:agent (no suffix).
const TAB_TO_SEG: Record<AgentTab, string> = { converse: '', configure: 'config', sessions: 'history', telemetry: 'activity' };
const SEG_TO_TAB: Record<string, AgentTab> = { config: 'configure', history: 'sessions', activity: 'telemetry' };

export function selToPath(sel: Sel): string {
  switch (sel.kind) {
    case 'welcome': return '/';
    case 'setup': return '/setup';
    case 'signals': return '/activity';
    case 'comms': return '/comms';
    case 'providers': return '/providers';
    case 'code-sessions': return sel.focus ? `/runs/${encodeURIComponent(sel.focus)}` : '/runs';
    case 'agent': {
      const base = `/agents/${encodeURIComponent(sel.agent)}`;
      const seg = TAB_TO_SEG[sel.tab];
      return seg ? `${base}/${seg}` : base;
    }
  }
}

// Parse a pathname back into a Sel. Unknown or malformed paths fall back to
// welcome (the caller replaceState-normalizes the URL to match). Segments are
// decoded defensively — a bad %-escape yields welcome rather than throwing.
export function pathToSel(pathname: string): Sel {
  let parts: string[];
  try {
    parts = pathname.split('/').filter(Boolean).map((s) => decodeURIComponent(s));
  } catch {
    return { kind: 'welcome' };
  }
  if (parts.length === 0) return { kind: 'welcome' };
  switch (parts[0]) {
    case 'setup': return { kind: 'setup' };
    case 'activity': return { kind: 'signals' };
    case 'comms': return { kind: 'comms' };
    case 'providers': return { kind: 'providers' };
    case 'runs': return parts[1] ? { kind: 'code-sessions', focus: parts[1] } : { kind: 'code-sessions' };
    case 'agents': {
      const agent = parts[1];
      if (!agent) return { kind: 'welcome' };
      if (parts[2] === undefined) return { kind: 'agent', agent, tab: 'converse' };
      const tab = SEG_TO_TAB[parts[2]];
      return tab ? { kind: 'agent', agent, tab } : { kind: 'welcome' };
    }
    default: return { kind: 'welcome' };
  }
}
