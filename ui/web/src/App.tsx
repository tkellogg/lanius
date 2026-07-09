import { useEffect, useMemo, useRef, useState } from 'react';
import CodeSessions from './CodeSessions';
import CommsView from './CommsView';
import ProvidersView from './ProvidersView';
import AgentAssistant, { ClientTool } from './components/AgentAssistant';
import { adminGet, adminPost, adminPut, history, publish, status as fetchStatus, liveness as fetchLiveness } from './api';
import { openLiveStream } from './live';
import { IconButton } from './components/primitives';
import { type Sel, type AgentTab, selToPath, pathToSel } from './routing';
import { arr, csv, uid } from './lib/format';
import { agentOf, newWebConversationId, conversationStorageKey, mergeConvMessages, sessionFromPayload, isWorkerAgentName, isWorkerSessionId, topicFilterMatches } from './lib/conversation';
import { declaredConfigParams, configRowMap, prunedSet } from './lib/packages';
import Nav from './views/Nav';
import WelcomeView from './views/WelcomeView';
import SetupView from './views/SetupView';
import ConfigureView, { KitModal } from './views/ConfigureView';
import ConverseView from './views/ConverseView';
import RailView from './views/RailView';
import SessionsView from './views/SessionsView';

// Product setup language is guided by docs/journeys/README.md and
// docs/layering.md. Durable browser-flow expectations live in
// docs/ui-flows/README.md and docs/ui-flows/configuration.md.
const BUFFER_CAP = 2000;
const PARENT_PATH = '$parent';
// M6 (agent-comms-ui): the priority at/above which an agent-to-agent delivery is
// "urgent" and lights the global signal lamp. Mirrors the backend default
// (agent-comms.high_priority_threshold = 5).
const HIGH_PRIORITY_THRESHOLD = 5;

type ThemeChoice = 'system' | 'light' | 'dark';
const THEME_CHOICES: ThemeChoice[] = ['system', 'light', 'dark'];

const emptyForm = {
  agent: '',
  owner: 'owner',
  parent: '',
  autonomy: 'off',
  packagePath: '',
  pathInherit: true,
  effectivePath: '',
  model: '',
  turns: '',
  provider: '',
  baseUrl: '',
  apiKeyEnv: '',
  contextProgram: 'default',
  contextMaxMs: '30000',
  workdir: '',
  fsWrite: '',
  captureExclude: '',
  network: 'open',
  fsReadDeny: '',
  fsReadAllow: '',
  include: '#',
  exclude: '',
  varsRows: [{ id: uid(), key: '', value: '' }],
  throttleRows: [{ id: uid(), name: '', max: '', rate: '', tokens: '', coalesce: false }],
};

export function App() {
  const [defaultAgent, setDefaultAgent] = useState('main');
  const [count, setCount] = useState(0);
  const [conn, setConn] = useState({ connected: false, text: 'connecting' });
  const [signal, setSignal] = useState({ lit: false, label: 'signal' });
  const [historyOk, setHistoryOk] = useState<boolean | null>(null);
  const [sel, setSel] = useState<any>({ kind: 'welcome' });
  const [navOpen, setNavOpen] = useState(false);
  // M2 (agentic-configuration): the AI panel — a non-modal, always-available
  // right-side chat mounting the helper profile. Open/closed persists across
  // reloads like the theme control; the panel itself is
  // rendered once, outside the `sel`-gated view tree, so it is reachable from
  // every view without per-view wiring.
  const [aiPanelOpen, setAiPanelOpen] = useState<boolean>(() => {
    try { return localStorage.getItem('lanius.aiPanel') === '1'; } catch { return false; }
  });
  useEffect(() => { try { localStorage.setItem('lanius.aiPanel', aiPanelOpen ? '1' : '0'); } catch {} }, [aiPanelOpen]);
  const [themeChoice, setThemeChoice] = useState<ThemeChoice>(() => {
    try {
      const stored = localStorage.getItem('lanius.theme');
      return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system';
    } catch {
      return 'system';
    }
  });
  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: light)');
    const apply = () => {
      document.documentElement.dataset.theme = themeChoice === 'system' ? (media.matches ? 'light' : 'dark') : themeChoice;
    };
    try { localStorage.setItem('lanius.theme', themeChoice); } catch {}
    apply();
    if (themeChoice !== 'system') return;
    media.addEventListener('change', apply);
    return () => media.removeEventListener('change', apply);
  }, [themeChoice]);
  const [agents, setAgents] = useState(new Map());
  const [diskProfiles, setDiskProfiles] = useState<any[]>([]);
  const [buffer, setBuffer] = useState<any[]>([]);
  const [filter, setFilter] = useState('signals');
  const [paused, setPaused] = useState(false);
  const [conv, setConv] = useState(new Map());
  const [conversations, setConversations] = useState(new Map());
  // docs/handoffs/reply-branching.md M3 — the branch origin to render for a
  // session's thread (the origin chip): a Map<session, { session (parent),
  // event_id, quote }>. Populated when a reply forks a fresh thread (pending,
  // before the first send) AND from a loaded conversation's `branched_from`
  // (persisted, from the ledger projection). One map so the chip renders the
  // same whether the branch is still pending or already sent.
  const [branchOrigins, setBranchOrigins] = useState(new Map());
  const [sessionsState, setSessionsState] = useState<any>({ status: 'idle', sessions: [], transcript: null, error: '' });
  const [modelOptions, setModelOptions] = useState<any[]>([]);
  const [modelsHint, setModelsHint] = useState('');

  const [systemStatus, setSystemStatus] = useState<any>(null);
  const [liveness, setLiveness] = useState<any>({ actors: {} });
  const [setup, setSetup] = useState<any>({ status: '', statusKind: '', kits: null, packages: null, proposals: null, loading: false });
  const [newAgent, setNewAgent] = useState({ name: '', purpose: '', workdir: '', model: '', turns: '24', autonomy: 'off', capability: '' });
  const [newAgentNote, setNewAgentNote] = useState('');

  const [cfgProfile, setCfgProfile] = useState<string | null>(null);
  const [cfgParsed, setCfgParsed] = useState<any>({});
  const [cfgLoading, setCfgLoading] = useState(false);
  const [cfgNote, setCfgNote] = useState('');
  const [cfgTomlNote, setCfgTomlNote] = useState('');
  const [cfgToml, setCfgToml] = useState('');
  const [cfgForm, setCfgForm] = useState<any>(emptyForm);
  const [cfgPackages, setCfgPackages] = useState<any[]>([]);
  const [cfgKits, setCfgKits] = useState<any[]>([]);
  const [cfgConfigPackages, setCfgConfigPackages] = useState(new Set());
  const [cfgSharedConfigRows, setCfgSharedConfigRows] = useState(new Map());
  const [cfgKitDetails, setCfgKitDetails] = useState(new Map());
  const [cfgContextChain, setCfgContextChain] = useState<any[]>([]);
  const [cfgContextRemoved, setCfgContextRemoved] = useState(new Set());
  const [cfgContextVarEdits, setCfgContextVarEdits] = useState(new Map());
  const [kitModalOpen, setKitModalOpen] = useState(false);

  const refs = useRef<any>({});
  refs.current = { sel, agents, diskProfiles, defaultAgent, historyOk, filter, paused, cfgForm, cfgPackages, cfgContextChain, conversations };
  const corrAgent = useRef(new Map());
  const corrSession = useRef(new Map());
  // The pending branch target for a forked session, keyed by the new session id:
  // { event_id, corr, quote, session (parent) }. submitCompose reads it to attach
  // the structured `branched_from` to the FIRST publish, then consumes it (a
  // branch seeds once; later replies in the same thread are ordinary sends).
  const branchTargets = useRef(new Map());
  const sentCorrs = useRef(new Set());
  const seenAsks = useRef(new Set());
  const seenFailures = useRef(new Set());
  // M6 (agent-comms-ui): event ids that have already lit the lamp, so the same
  // delivery (re-broadcast on an SSE reconnect) cannot strobe it.
  const seenLampEvents = useRef(new Set());
  const agentSessions = useRef(new Map());
  const kitModalRef = useRef<HTMLDialogElement | null>(null);

  const touchAgent = (name: string, opts: any = {}) => {
    if (!name) return;
    setAgents((prev) => {
      const next = new Map(prev);
      const a = next.get(name) ?? { sessions: new Set(), live: false, profile: null };
      if (opts.live) a.live = true;
      if (opts.profile) a.profile = opts.profile;
      for (const s of opts.sessions ?? []) a.sessions.add(s);
      next.set(name, { ...a, sessions: new Set(a.sessions) });
      return next;
    });
  };

  const profileOf = (agentName: string) =>
    refs.current.diskProfiles.find((p: any) => p.agent === agentName)
      ?? refs.current.diskProfiles.find((p: any) => p.profile === agentName)
      ?? null;

  const loadDiskAgents = async () => {
    const j = await adminGet('agents');
    if (!j.ok) return;
    setDiskProfiles(j.profiles ?? []);
    for (const p of j.profiles ?? []) touchAgent(p.agent, { profile: p.profile });
  };

  const loadSystemStatus = async () => {
    const j = await fetchStatus();
    setSystemStatus(j);
  };

  const loadLiveness = async () => {
    const j = await fetchLiveness();
    if (j?.ok) setLiveness(j);
  };

  const setHistoryState = (v: boolean) => setHistoryOk((prev) => prev === v ? prev : v);

  const refreshAgents = async () => {
    const j = await history({ kind: 'agents' });
    if (j?.unavailable) setHistoryState(false);
    if (!j?.ok) return;
    setHistoryState(true);
    for (const a of j.agents ?? []) touchAgent(a.agent, { sessions: a.sessions });
  };

  useEffect(() => {
    void loadSystemStatus();
    void loadLiveness();
    void loadDiskAgents();
    void refreshAgents();
    const iv = setInterval(refreshAgents, 15000);
    const statusIv = setInterval(() => { void loadSystemStatus(); void loadLiveness(); }, 10000);
    return () => { clearInterval(iv); clearInterval(statusIv); };
  }, []);

  useEffect(() => {
    const es = openLiveStream((m: any) => {
      if (m.kind === 'status') {
        if (m.agent) {
          setDefaultAgent(m.agent);
          touchAgent(m.agent);
        }
        setSystemStatus((prev: any) => prev ? { ...prev, broker_connected: !!m.connected, broker: m.broker ?? prev.broker, agent: m.agent ?? prev.agent } : prev);
        setConn({ connected: !!m.connected, text: m.connected ? 'connected' : 'disconnected' });
        return;
      }
      if (m.kind === 'message') onLiveMessage(m);
    }, () => setConn({ connected: false, text: 'server lost — retrying' }));
    return () => es.close();
  }, []);

  useEffect(() => {
    (async () => {
      const j = await adminGet('models');
      if (!(j.models ?? []).length) {
        if (j.note) {
          setModelsHint(`model list unavailable: ${j.note}`);
          console.warn('models:', j.note);
        }
        return;
      }
      setModelOptions(j.models);
    })();
  }, []);

  const primaryAgent = () => {
    const def = diskProfiles.find((p) => p.profile === 'default');
    if (def) return def.agent;
    if (diskProfiles[0]) return diskProfiles[0].agent;
    return [...agents.keys()][0] ?? null;
  };

  // ── routing (docs/handoffs/web-ui-routing.md M2) ───────────────────────────
  // `navigate` is the one path that mutates `sel`: it pushes (or replaces) a
  // browser-history entry keyed by `selToPath` and then applies the selection.
  // `applyRoute` runs the imperative side-effects the declarative
  // useEffect([sel.kind, sel.agent, sel.tab]) below does NOT cover (the signals
  // filter), so a Back/Forward-restored view matches a freshly-clicked one; the
  // effect still owns setup/configure/sessions/converse loading off `sel`.
  const applyRoute = (next: Sel) => {
    if (next.kind === 'signals') setFilter('signals');
    setSel(next);
  };
  const navigate = (next: Sel, mode: 'push' | 'replace' = 'push') => {
    const path = selToPath(next);
    if (mode === 'replace' || path === window.location.pathname) {
      window.history.replaceState(null, '', path);
    } else {
      window.history.pushState(null, '', path);
    }
    applyRoute(next);
  };

  const selectWelcome = () => navigate({ kind: 'welcome' });
  const selectSignals = () => navigate({ kind: 'signals' });
  // Observability M4: the coding-session tree (a "workers" surface). Minimal mount
  // — final placement belongs in the Workers nav the chat track is building.
  const selectCodeSessions = () => navigate({ kind: 'code-sessions' });
  // agent-comms-ui M2: the cross-agent comms plane (agent-to-agent mail + rooms).
  const selectComms = () => navigate({ kind: 'comms' });
  // model-providers M4: the Providers page (the named, encrypted credential vault).
  const selectProviders = () => navigate({ kind: 'providers' });
  // agent-comms-ui M2: cross-link a comms participant to its run in the runs view.
  const selectCodeSession = (session: string) => navigate({ kind: 'code-sessions', focus: session });
  const selectSetup = (status?: any) => {
    navigate({ kind: 'setup' });
    void loadSetup(status);
  };
  const selectAgent = (agent: string, tab?: string) => {
    // Read the prior tab from refs (kept in sync each render) rather than a
    // functional setSel updater, so `navigate`'s single pushState isn't at risk
    // of a double-invoked reducer: reselecting the same agent keeps its tab.
    const prev = refs.current.sel;
    const nextTab = (tab ?? (prev?.kind === 'agent' && prev.agent === agent ? prev.tab : 'converse')) as AgentTab;
    navigate({ kind: 'agent', agent, tab: nextTab });
  };

  // On mount, normalize the URL → sel: a reload/deep-link lands directly on that
  // view, and an unknown/malformed path replaces to '/'. popstate then restores
  // sel as the user walks Back/Forward. Runs once — navigate() owns every push
  // after this. loadSetup/loadConfigure/loadConversations fire off the sel-change
  // effect below, so a deep-linked view hydrates its data without extra wiring.
  useEffect(() => {
    const initial = pathToSel(window.location.pathname);
    window.history.replaceState(null, '', selToPath(initial));
    applyRoute(initial);
    const onPop = () => applyRoute(pathToSel(window.location.pathname));
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // M2 (agentic-configuration): the helper's client tools — "access to
  // everything the UI has access to", starting with reads + navigation.
  // Follows the `contextAuthorTools` pattern (App.tsx configure view): every
  // handler reuses an existing admin/API route (adminGet or the same
  // /api/conversations fetch `loadConversation` already uses) — no new server
  // route, and no write authority added. `navigate` drives the same `sel`
  // state a nav click would, so the helper can take the person to the thing
  // it is describing.
  const helperTools: ClientTool[] = useMemo(() => [
    {
      name: 'get_status',
      description: 'Read overall platform status: root/credential/broker health, history availability, read-camera and cage (sandbox) posture, and LLM-path detection (native provider / harness CLIs on PATH / which world: a, b, or c).',
      parameters: { type: 'object', properties: {} },
      handler: async () => {
        const j = await fetchStatus<any>();
        if (!j?.ok) throw new Error(j?.error ?? 'status unavailable');
        return j;
      },
    },
    {
      name: 'list_agents',
      description: 'List the agents configured on this installation: name, backing agent noun, which profile it mirrors, and whether it is ready to run.',
      parameters: { type: 'object', properties: {} },
      handler: async () => {
        const j = await adminGet('agents');
        if (!j.ok) throw new Error(j.error ?? 'agent list unavailable');
        return { agents: j.profiles ?? [] };
      },
    },
    {
      name: 'list_packages',
      description: "List an agent's packages (add-ons): name, source (kit/copied/linked), and trust/approval state.",
      parameters: {
        type: 'object',
        properties: { agent: { type: 'string', description: 'agent name; defaults to "default"' } },
      },
      handler: async (args: any) => {
        const agent = String(args?.agent ?? '').trim() || 'default';
        const j = await adminGet(`packages?profile=${encodeURIComponent(agent)}`);
        if (!j.ok) throw new Error(j.error ?? 'package list unavailable');
        return { agent, packages: j.packages ?? [] };
      },
    },
    {
      name: 'list_providers',
      description: 'List named model-provider credentials (metadata only — kind, wire, base URL, tool pin; secrets are never returned).',
      parameters: { type: 'object', properties: {} },
      handler: async () => {
        const j = await adminGet('providers');
        if (!j.ok) throw new Error(j.error ?? 'provider list unavailable');
        return { providers: j.providers ?? [] };
      },
    },
    {
      name: 'read_conversation',
      description: 'Read a chat conversation with an agent. Omit session to list that agent\'s recent conversations; pass session to read its messages.',
      parameters: {
        type: 'object',
        properties: {
          agent: { type: 'string', description: 'the agent whose conversation(s) to read' },
          session: { type: 'string', description: 'a specific conversation/session id (optional — omit to list)' },
        },
        required: ['agent'],
      },
      handler: async (args: any) => {
        const agent = String(args?.agent ?? '').trim();
        if (!agent) throw new Error('read_conversation needs {agent}');
        const session = args?.session ? String(args.session).trim() : '';
        if (!session) {
          const r = await fetch(`/api/conversations?agent=${encodeURIComponent(agent)}`);
          const j = await r.json().catch(() => ({}));
          if (!j.ok) throw new Error(j.error ?? 'conversation list unavailable');
          return { agent, conversations: j.conversations ?? [] };
        }
        const r = await fetch(`/api/conversations/${encodeURIComponent(session)}`);
        const j = await r.json().catch(() => ({}));
        if (!j.ok) throw new Error(j.error ?? 'conversation unavailable');
        return { agent, session, messages: j.conversation?.messages ?? [], branched_from: j.conversation?.branched_from ?? null };
      },
    },
    {
      name: 'navigate',
      description: 'Switch what the interface is showing — the same selection a nav click would make. kind is one of welcome, agent, setup, signals, code-sessions, comms, providers; pass agent (and optionally tab: converse/sessions/telemetry/configure) when kind is "agent".',
      parameters: {
        type: 'object',
        properties: {
          kind: { type: 'string', enum: ['welcome', 'agent', 'setup', 'signals', 'code-sessions', 'comms', 'providers'] },
          agent: { type: 'string' },
          tab: { type: 'string', enum: ['converse', 'sessions', 'telemetry', 'configure'] },
        },
        required: ['kind'],
      },
      handler: async (args: any) => {
        const kind = String(args?.kind ?? '');
        switch (kind) {
          case 'welcome': selectWelcome(); break;
          case 'setup': selectSetup(); break;
          case 'signals': selectSignals(); break;
          case 'code-sessions': selectCodeSessions(); break;
          case 'comms': selectComms(); break;
          case 'providers': selectProviders(); break;
          case 'agent': {
            const agent = String(args?.agent ?? '').trim();
            if (!agent) throw new Error('navigate to an agent needs {agent}');
            selectAgent(agent, args?.tab);
            break;
          }
          default: throw new Error(`unknown navigate kind ${kind}`);
        }
        return { navigated: kind };
      },
    },
  ], []);

  // Conversation persist/fork/resume (M2). M5 (the agent-driven conversation
  // selector) is DEFERRED: it is not built here, but the controls below are
  // routed through this state + the M3/M4 API (rememberConversation /
  // loadConversation / openConversation / newConversation read the same
  // /api/conversations endpoints) — not UI-only — so an agent can later drive
  // the same persist/fork/resume calls without a UI rewrite.
  const conversationStateFor = (agent: string) => conversations.get(agent) ?? { status: 'idle', list: [], error: '' };
  const currentConversation = (agent: string) => agentSessions.current.get(agent) ?? localStorage.getItem(conversationStorageKey(agent)) ?? '';
  const rememberConversation = (agent: string, session: string) => {
    if (!agent || !session) return;
    agentSessions.current.set(agent, session);
    localStorage.setItem(conversationStorageKey(agent), session);
  };
  const loadConversations = async (agent: string) => {
    if (!agent) return;
    // M2 (chat-rendering): even a worker-named agent gets a real (empty) resolved
    // state so the converse view's comms-plane-vs-trace decision is driven by the
    // ledger read, not by the name heuristic. The /api/conversations projection
    // already drops worker sessions (is_worker_session), so this returns [] for a
    // pure worker — exactly the "no comms-plane traffic" signal we want, and the
    // same answer any third-party UI gets from the same read.
    setConversations((prev) => new Map(prev).set(agent, { ...(prev.get(agent) ?? {}), status: 'loading', error: '' }));
    try {
      const r = await fetch(`/api/conversations?agent=${encodeURIComponent(agent)}`);
      const j = await r.json().catch(() => ({}));
      if (!j.ok) throw new Error(j.error ?? 'conversation list unavailable');
      setConversations((prev) => new Map(prev).set(agent, { status: 'list', list: j.conversations ?? [], error: '' }));
    } catch (err: any) {
      setConversations((prev) => new Map(prev).set(agent, { status: 'error', list: prev.get(agent)?.list ?? [], error: String(err.message ?? err) }));
    }
  };
  const loadConversation = async (agent: string, session: string) => {
    if (!agent || !session) return;
    rememberConversation(agent, session);
    try {
      const r = await fetch(`/api/conversations/${encodeURIComponent(session)}`);
      const j = await r.json().catch(() => ({}));
      if (!j.ok) return;
      setConv((prev) => {
        const next = new Map(prev);
        next.set(agent, mergeConvMessages(next.get(agent) ?? [], j.conversation?.messages ?? []));
        return next;
      });
      // M3 (reply-branching): a loaded conversation may carry its branch origin
      // (docs/handoffs/reply-branching.md M2). Record it so the origin chip
      // renders for a branch opened from the list — not just a freshly forked
      // one. Only overwrite when the ledger actually has an edge (never clobber a
      // pending fork's origin with null).
      const bf = j.conversation?.branched_from;
      if (bf && (bf.event_id != null || bf.session)) {
        setBranchOrigins((prev) => new Map(prev).set(session, { session: bf.session, event_id: bf.event_id, quote: bf.quote ?? bf.preview ?? '' }));
      }
    } catch {
      /* live tail still works */
    }
  };
  const openConversation = (agent: string, session: string) => {
    rememberConversation(agent, session);
    selectAgent(agent, 'converse');
    void loadConversation(agent, session);
  };
  const newConversation = (agent: string) => {
    const session = newWebConversationId(agent);
    rememberConversation(agent, session);
    setConv((prev) => new Map(prev).set(agent, []));
    selectAgent(agent, 'converse');
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>('#compose-input')?.focus());
  };

  // docs/handoffs/reply-branching.md M3 — clicking reply on any message forks a
  // brand-new conversation seeded with that message as context (Slack-style). We
  // mint a fresh web-* session, stash the target as this session's pending branch
  // (submitCompose attaches the structured `branched_from` to the first publish),
  // and show the origin chip immediately from the target so the fork is legible
  // before the first send. The kernel composes the agent-visible quote from the
  // structured field — the UI never inlines it into the prompt.
  const startBranch = (agent: string, m: any) => {
    const parentSession = currentConversation(agent);
    const session = newWebConversationId(agent);
    const quote = String(m?.text ?? '');
    branchTargets.current.set(session, { event_id: m?.event_id ?? null, corr: m?.corr ?? null, quote, session: parentSession });
    setBranchOrigins((prev) => new Map(prev).set(session, { session: parentSession, event_id: m?.event_id ?? null, quote }));
    rememberConversation(agent, session);
    setConv((prev) => new Map(prev).set(agent, []));
    selectAgent(agent, 'converse');
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>('#compose-input')?.focus());
  };

  const agentNamesKey = [...agents.keys()].sort().join('|');
  useEffect(() => {
    for (const name of agentNamesKey.split('|').filter(Boolean)) {
      if (!isWorkerAgentName(name)) void loadConversations(name);
    }
  }, [agentNamesKey]);

  useEffect(() => {
    if (sel.kind === 'setup' && !setup.loading && !setup.kits) void loadSetup();
    if (sel.kind === 'agent' && sel.tab === 'configure') void loadConfigure(sel.agent);
    if (sel.kind === 'agent' && sel.tab === 'sessions') void loadSessions(sel.agent);
    if (sel.kind === 'agent' && sel.tab === 'telemetry') setFilter('all');
    if (sel.kind === 'agent' && sel.tab === 'converse') {
      void loadConversations(sel.agent);
      const stored = currentConversation(sel.agent);
      if (stored) void loadConversation(sel.agent, stored);
    }
  }, [sel.kind, sel.agent, sel.tab]);

  // Narrow viewport: collapse the nav drawer as soon as a person picks something.
  useEffect(() => { setNavOpen(false); }, [sel]);

  const stageTitle = sel.kind === 'welcome' ? 'welcome'
    : sel.kind === 'signals' ? 'activity'
      : sel.kind === 'setup' ? 'setup'
        : sel.kind === 'code-sessions' ? 'runs'
          : sel.kind === 'comms' ? 'messages'
            : sel.kind === 'providers' ? 'providers'
            : sel.agent;
  const stageNote = sel.kind === 'welcome' ? 'orient, then dive in'
    : sel.kind === 'signals' ? 'What’s happening now. Red means something needs you.'
      : sel.kind === 'code-sessions' ? 'Coding runs and the workers they started.'
      : sel.kind === 'comms' ? 'Messages agents send each other, and the rooms they share.'
      : sel.kind === 'providers' ? 'The model keys your agents use. Add one, test it, pick one per agent.'
      : sel.kind === 'setup' ? 'first-run health, agent setup, capabilities, and trust'
        : sel.tab === 'converse' ? `messages with ${sel.agent}`
          : sel.tab === 'sessions' ? 'your agent’s past conversations'
            : sel.tab === 'configure' ? 'who this agent is — model, cost, and add-ons'
              : `${sel.agent}’s activity`;

  const loadSetup = async (opts: any = {}) => {
    setSetup((s: any) => ({ ...s, loading: true, status: opts.status ?? s.status, statusKind: opts.statusKind ?? s.statusKind }));
    const [status, kits, packages, proposals] = await Promise.all([fetchStatus(), adminGet('kits'), adminGet('packages'), adminGet('proposals')]);
    setSystemStatus(status);
    await loadDiskAgents();
    setSetup({ loading: false, status: opts.status ?? '', statusKind: opts.statusKind ?? '', kits, packages, proposals });
  };

  const createAgent = async () => {
    const name = newAgent.name.trim();
    const model = newAgent.model.trim();
    const purpose = newAgent.purpose.trim();
    const workdir = newAgent.workdir.trim();
    const turns = Number(newAgent.turns || 0);
    if (!name) { setNewAgentNote('name it first'); return; }
    setNewAgentNote('creating…');
    const r = await adminPost('agents', { name, ...(model ? { model } : {}) });
    if (!r.ok) { setNewAgentNote(r.error ?? 'failed'); return; }
    const set: Record<string, any> = { autonomy: newAgent.autonomy };
    if (model) set['model.model'] = model;
    if (turns > 0) set['model.max_turns'] = turns;
    if (workdir) set['sandbox.workdir'] = workdir;
    if (purpose) set['vars.purpose'] = purpose;
    const save = await adminPost('agents/set', { name, set: prunedSet(set) });
    if (!save.ok) {
      setNewAgentNote(`created ${name}, but setup details did not save: ${save.error ?? 'unknown error'}`);
      await loadDiskAgents();
      selectAgent(name, 'configure');
      return;
    }
    if (newAgent.capability) {
      const kit = newAgent.capability;
      const add = await adminPost('kits/add', { kit });
      if (!add.ok) {
        setNewAgentNote(`created ${name}; capability ${kit} did not add: ${add.error ?? 'unknown error'}`);
      }
    }
    setNewAgent({ name: '', purpose: '', workdir: '', model: '', turns: '24', autonomy: 'off', capability: '' });
    setNewAgentNote('');
    await loadDiskAgents();
    await loadSetup({ status: `created ${name}. Next: send a message.`, statusKind: 'ok' });
    selectAgent(name, 'converse');
  };

  const provenance = useMemo(() => {
    const set = new Set();
    for (const p of setup.packages?.packages ?? []) {
      for (const g of p.grants ?? []) if (g.decided_by?.startsWith('kit:')) set.add(g.decided_by.slice(4));
    }
    return set;
  }, [setup.packages]);

  const skillIncluded = (pkg: any, form = cfgForm) => {
    const include = arr(form.include);
    const inc = include.length ? include : ['#'];
    return inc.some((p) => topicFilterMatches(p, pkg.name));
  };
  const skillExcluded = (pkg: any, form = cfgForm) => arr(form.exclude).some((p) => topicFilterMatches(p, pkg.name));
  const skillVisible = (pkg: any, form = cfgForm) => skillIncluded(pkg, form) && !skillExcluded(pkg, form);

  const contextStageKey = (s: any) => `${s.package}/${s.name}`;
  const contextStageDefs = (packages = cfgPackages, form = cfgForm) => {
    const defs = [];
    for (const p of packages.filter((pkg) => skillVisible(pkg, form))) {
      for (const stage of p.manifest?.stages ?? []) {
        defs.push({
          package: p.name,
          name: stage.name,
          description: stage.description ?? stage.summary ?? stage.injects ?? '',
          mode: stage.mode ?? 'exec',
          order: Number(stage.order ?? 50),
          timeout_ms: stage.mode === 'resident' ? 15000 : 10000,
          config: stage.config ?? [],
        });
      }
    }
    return defs.sort((a, b) => (a.order - b.order) || a.package.localeCompare(b.package) || a.name.localeCompare(b.name));
  };

  const resetContextChain = (context: any, packages: any[], form: any) => {
    const defs = contextStageDefs(packages, form);
    const overrides = new Map<string, any>((context.stages ?? []).map((s: any) => [contextStageKey(s), s]));
    const removed = new Set((context.stages ?? []).filter((s: any) => s.enabled === false).map(contextStageKey));
    const chain = [];
    for (const def of defs) {
      const ov = overrides.get(contextStageKey(def));
      if (ov?.enabled === false) continue;
      chain.push({ ...def, enabled: true, order: Number(ov?.order ?? def.order), timeout_ms: Number(ov?.timeout_ms ?? def.timeout_ms) });
    }
    chain.sort((a, b) => (a.order - b.order) || a.package.localeCompare(b.package) || a.name.localeCompare(b.name));
    setCfgContextRemoved(removed);
    setCfgContextVarEdits(new Map());
    setCfgContextChain(chain);
  };

  const reconcileContextChain = (form: any) => {
    const defs = contextStageDefs(cfgPackages, form);
    const defsByKey = new Map(defs.map((s) => [contextStageKey(s), s]));
    const disabledFromProfile = new Set((cfgParsed.context?.stages ?? []).filter((s: any) => s.enabled === false).map(contextStageKey));
    setCfgContextChain((prev) => {
      const seen = new Set();
      const next = prev
        .filter((stage) => defsByKey.has(contextStageKey(stage)))
        .map((stage) => {
          const def = defsByKey.get(contextStageKey(stage));
          seen.add(contextStageKey(stage));
          return { ...def, ...stage, enabled: true };
        });
      for (const def of defs) {
        const key = contextStageKey(def);
        if (!seen.has(key) && !disabledFromProfile.has(key) && !cfgContextRemoved.has(key)) next.push({ ...def, enabled: true });
      }
      return next.sort((a, b) => (Number(a.order ?? 50) - Number(b.order ?? 50)) || a.package.localeCompare(b.package) || a.name.localeCompare(b.name));
    });
  };

  const loadConfigure = async (agentName: string) => {
    const p = profileOf(agentName);
    const profile = p?.profile ?? agentName;
    setCfgProfile(profile);
    setCfgParsed({});
    setCfgNote('loading…');
    setCfgLoading(true);
    const [r, pkgs, kits, configs] = await Promise.all([
      adminGet(`profile?name=${encodeURIComponent(profile)}`),
      adminGet(`packages?profile=${encodeURIComponent(profile)}`),
      adminGet('kits'),
      adminGet('configs'),
    ]);
    const packages = pkgs.ok === false ? [] : (pkgs.packages ?? []);
    const d = r.profile ?? profileOf(agentName) ?? {};
    const localPath = d.local_elanus_path ?? null;
    const localEntries = Array.isArray(localPath) ? localPath.filter((x: string) => x !== PARENT_PATH) : [];
    const nextForm = {
      agent: d.agent ?? agentName,
      owner: d.owner ?? 'owner',
      parent: d.parent ?? '',
      autonomy: d.autonomy ?? 'off',
      packagePath: csv(localEntries),
      pathInherit: !Array.isArray(localPath) || localPath.includes(PARENT_PATH),
      effectivePath: csv(d.elanus_path ?? d.package_path ?? ['packages']),
      model: d.model ?? '',
      turns: d.max_turns ?? '',
      provider: d.provider ?? '',
      baseUrl: d.base_url ?? '',
      apiKeyEnv: d.api_key_env ?? '',
      contextProgram: d.context?.program ?? 'default',
      contextMaxMs: d.context?.max_total_ms ?? 30000,
      workdir: d.workdir ?? '',
      fsWrite: csv(d.fs_write ?? []),
      captureExclude: csv(d.capture_exclude ?? []),
      network: d.network ?? 'open',
      fsReadDeny: csv(d.fs_read_deny ?? []),
      fsReadAllow: csv(d.fs_read_allow ?? []),
      include: csv(d.skills?.include ?? ['#']),
      exclude: csv(d.skills?.exclude ?? []),
      varsRows: Object.entries(d.vars ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => ({ id: uid(), key, value })) || [],
      throttleRows: Object.entries(d.throttle ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([name, t]: any) => ({
        id: uid(), name, max: t?.max_concurrent ?? '', rate: t?.rate_per_min ?? '', tokens: t?.llm_tokens_per_hour ?? '', coalesce: t?.coalesce === true,
      })),
    };
    if (!nextForm.varsRows.length) nextForm.varsRows = [{ id: uid(), key: '', value: '' }];
    if (!nextForm.throttleRows.length) nextForm.throttleRows = [{ id: uid(), name: '', max: '', rate: '', tokens: '', coalesce: false }];
    const configPackageNames = (configs.ok === false ? [] : (configs.configs ?? [])).map((c: any) => c.package).filter(Boolean);
    const sharedConfigNames = new Set([
      ...configPackageNames,
      ...packages.filter((pkg: any) => declaredConfigParams(pkg).length > 0).map((pkg: any) => pkg.name),
    ]);
    const sharedEntries = await Promise.all([...sharedConfigNames].map(async (name) => {
      const raw = await adminGet(`configs?package=${encodeURIComponent(name)}`);
      return [name, raw.ok ? configRowMap(raw.config?.toml || '') : new Map<string, string>()] as [string, Map<string, string>];
    }));
    setCfgPackages(packages);
    setCfgKits(kits.ok === false ? [] : (kits.kits ?? []));
    setCfgConfigPackages(new Set(configPackageNames));
    setCfgSharedConfigRows(new Map<string, Map<string, string>>(sharedEntries));
    setCfgToml(r.ok ? r.toml : '');
    await loadDiskAgents();
    setCfgParsed(d);
    setCfgForm(nextForm);
    resetContextChain(d.context ?? {}, packages, nextForm);
    setCfgLoading(false);
    setCfgNote(r.ok ? '' : `no settings file for ${profile} — this agent only exists as traffic; create an agent here to configure it`);
  };

  const saveConfigure = async (overrides?: any) => {
    if (!cfgProfile) return;
    setCfgNote('saving…');
    const contextChainForSave = Array.isArray(overrides?.contextChain) ? overrides.contextChain : cfgContextChain;
    const set: Record<string, any> = {};
    const newAgentName = cfgForm.agent.trim();
    if (newAgentName) set.agent = newAgentName;
    if (cfgForm.owner.trim()) set.owner = cfgForm.owner.trim();
    set.parent = cfgForm.parent.trim();
    set.autonomy = cfgForm.autonomy;
    const localPath = arr(cfgForm.packagePath);
    if (cfgForm.pathInherit) localPath.push(PARENT_PATH);
    set.elanus_path = localPath;
    if (cfgForm.model.trim()) set['model.model'] = cfgForm.model.trim();
    if (cfgForm.turns) set['model.max_turns'] = Number(cfgForm.turns);
    // model-providers M3/M4: a named provider WINS wholesale over the deprecated
    // inline base_url/api_key_env (which stay working for back-compat). Only set
    // it when chosen; clearing a provider is a raw-TOML edit (M4 scope).
    if (cfgForm.provider.trim()) set['model.provider'] = cfgForm.provider.trim();
    if (cfgForm.baseUrl.trim()) set['model.base_url'] = cfgForm.baseUrl.trim();
    if (cfgForm.apiKeyEnv.trim()) set['model.api_key_env'] = cfgForm.apiKeyEnv.trim();
    if (cfgForm.contextProgram.trim()) set['context.program'] = cfgForm.contextProgram.trim();
    if (cfgForm.contextMaxMs) set['context.max_total_ms'] = Number(cfgForm.contextMaxMs);
    const chain = new Map(contextChainForSave.map((s: any) => [contextStageKey(s), s]));
    const stageRows: any[] = contextChainForSave.map((s: any) => ({
      package: s.package,
      name: s.name,
      enabled: true,
      order: Number(s.order || 50),
      timeout_ms: Number(s.timeout_ms || (s.mode === 'resident' ? 15000 : 10000)),
    }));
    for (const def of contextStageDefs()) {
      if (!chain.has(contextStageKey(def))) stageRows.push({ package: def.package, name: def.name, enabled: false });
    }
    set['context.stage'] = stageRows;
    set['sandbox.workdir'] = cfgForm.workdir.trim();
    set['sandbox.fs_write'] = arr(cfgForm.fsWrite);
    set['sandbox.capture_exclude'] = arr(cfgForm.captureExclude);
    // Read/network cage (sandbox-config-ui M2) — product words map to the stored
    // enums per the handoff: open/loopback/none. All three flow through the same
    // agents/set → `lanius profile set` writer; no new write path.
    set['sandbox.network'] = cfgForm.network;
    set['sandbox.fs_read_deny'] = arr(cfgForm.fsReadDeny);
    set['sandbox.fs_read_allow'] = arr(cfgForm.fsReadAllow);
    set['skills.include'] = arr(cfgForm.include).length ? arr(cfgForm.include) : ['#'];
    set['skills.exclude'] = arr(cfgForm.exclude);
    for (const row of cfgForm.varsRows) if (row.key.trim()) set[`vars.${row.key.trim()}`] = row.value ?? '';
    for (const [k, v] of cfgContextVarEdits) set[`vars.${k}`] = v;
    for (const row of cfgForm.throttleRows) {
      const name = row.name.trim();
      if (!name) continue;
      if (row.max) set[`throttle.${name}.max_concurrent`] = Number(row.max);
      if (row.rate) set[`throttle.${name}.rate_per_min`] = Number(row.rate);
      if (row.tokens) set[`throttle.${name}.llm_tokens_per_hour`] = Number(row.tokens);
      if (row.coalesce || cfgParsed.throttle?.[name]?.coalesce != null) set[`throttle.${name}.coalesce`] = row.coalesce;
    }
    const r = await adminPost('agents/set', { name: cfgProfile, set: prunedSet(set) });
    if (!r.ok) { setCfgNote(r.error ?? 'save failed'); return; }
    setCfgNote('saved — applies on the next run');
    await loadDiskAgents();
    if (newAgentName && sel.kind === 'agent' && newAgentName !== sel.agent) selectAgent(newAgentName, 'configure');
    else {
      // Re-read just the parsed profile so the server-computed posture cards (M3)
      // reflect the save — WITHOUT a full loadConfigure, which would reset the
      // form and clobber the "saved" note. The rename branch re-selects instead.
      const fresh = await adminGet(`profile?name=${encodeURIComponent(cfgProfile)}`);
      if (fresh.ok && fresh.profile) setCfgParsed(fresh.profile);
    }
  };

  const saveRawToml = async () => {
    if (!cfgProfile) return;
    // Raw TOML bypasses the field-level save's validation and rewrite. The
    // off-switch for a linked kit confirms; a raw write that can rewrite
    // grants or sandbox paths should too. Plain prompt — durable confirm is
    // the resulting cfg-toml-note that survives the re-load.
    if (!window.confirm(`Save raw TOML for ${cfgProfile}? This rewrites the whole file and bypasses field-level checks.`)) return;
    setCfgTomlNote('saving…');
    const r = await adminPut(`profile?name=${encodeURIComponent(cfgProfile)}`, { toml: cfgToml });
    setCfgTomlNote(r.ok ? 'saved' : 'save failed');
    if (r.ok) void loadConfigure(sel.agent);
  };

  const contextDefs = useMemo(() => contextStageDefs(), [cfgPackages, cfgForm.include, cfgForm.exclude]);
  const availableContextStages = contextDefs.filter((s) => !cfgContextChain.some((c) => contextStageKey(c) === contextStageKey(s)));

  const moveContextStage = (index: number, dir: number) => {
    setCfgContextChain((prev) => {
      const next = [...prev];
      const j = index + dir;
      if (j < 0 || j >= next.length) return prev;
      [next[index], next[j]] = [next[j], next[index]];
      return next.map((s, i) => ({ ...s, order: (i + 1) * 10 }));
    });
  };
  const removeContextStage = (index: number) => {
    setCfgContextChain((prev) => {
      const next = [...prev];
      const [removed] = next.splice(index, 1);
      if (removed) setCfgContextRemoved((old) => new Set([...old, contextStageKey(removed)]));
      return next.map((s, i) => ({ ...s, order: (i + 1) * 10 }));
    });
  };
  const addContextStage = (key: string) => {
    const def = contextDefs.find((s) => contextStageKey(s) === key);
    if (!def) return;
    setCfgContextRemoved((old) => { const next = new Set(old); next.delete(key); return next; });
    setCfgContextChain((prev) => [...prev, { ...def, enabled: true, order: (prev.length + 1) * 10 }]);
  };
  const saveContextStageFromAssistant = async (stage: any) => {
    const key = typeof stage === 'string'
      ? stage
      : stage?.key || (stage?.package && stage?.name ? `${stage.package}/${stage.name}` : '');
    const def = contextDefs.find((s) => contextStageKey(s) === key);
    if (!def) return { ok: false, error: `unknown context block ${key || '(missing)'}` };
    if (cfgContextChain.some((s) => contextStageKey(s) === key)) {
      return { ok: true, already_present: true, stage: key };
    }
    const nextChain = [...cfgContextChain, { ...def, enabled: true, order: (cfgContextChain.length + 1) * 10 }]
      .map((s, i) => ({ ...s, order: (i + 1) * 10 }));
    setCfgContextRemoved((old) => { const next = new Set(old); next.delete(key); return next; });
    setCfgContextChain(nextChain);
    await saveConfigure({ contextChain: nextChain });
    return { ok: true, stage: key };
  };

  const setSkillExcluded = (pkgName: string, excluded: boolean) => {
    const next = arr(cfgForm.exclude).filter((p) => p !== pkgName);
    if (excluded) next.push(pkgName);
    const form = { ...cfgForm, exclude: next.join(', ') };
    setCfgForm(form);
    reconcileContextChain(form);
  };
  const setKitPackagesExcluded = (pkgs: any[], excluded: boolean) => {
    const names = new Set(pkgs.map((p) => p.name));
    const next = arr(cfgForm.exclude).filter((p) => !names.has(p));
    if (excluded) next.push(...[...names].sort());
    const form = { ...cfgForm, exclude: next.join(', ') };
    setCfgForm(form);
    reconcileContextChain(form);
  };

  const openKitModal = () => {
    setKitModalOpen(true);
    requestAnimationFrame(() => kitModalRef.current?.showModal?.());
  };
  const closeKitModal = () => {
    setKitModalOpen(false);
    kitModalRef.current?.close?.();
  };

  const loadKitDetail = async (k: any) => {
    if (cfgKitDetails.has(k.name)) return cfgKitDetails.get(k.name);
    const r = await adminGet(`kits/packages?kit=${encodeURIComponent(k.name)}`);
    const detail = r.ok ? r.kit : { ...k, packages: [], error: r.error ?? 'could not load kit' };
    setCfgKitDetails((prev) => new Map(prev).set(k.name, detail));
    return detail;
  };

  const installKitForAgent = async (k: any, mode: string, setNote: (s: string) => void) => {
    if (!cfgProfile) return;
    setNote(mode === 'copy' ? 'copying...' : 'linking...');
    if (mode === 'copy') {
      const r = await adminPost('kits/add', { kit: k.name, copy: true });
      setNote(r.ok ? `copied ${k.name}` : (r.error ?? 'copy failed'));
      if (r.ok) await loadConfigure(sel.agent ?? cfgParsed.agent ?? cfgProfile);
      return;
    }
    const entries = arr(cfgForm.packagePath).filter((p) => p !== k.dir);
    if (cfgForm.pathInherit) entries.push(PARENT_PATH);
    const insertAt = entries.indexOf(PARENT_PATH);
    if (insertAt >= 0) entries.splice(insertAt, 0, k.dir);
    else entries.unshift(k.dir);
    const r = await adminPost('agents/set', { name: cfgProfile, set: prunedSet({ parent: cfgForm.parent.trim(), elanus_path: entries }) });
    setNote(r.ok ? `linked ${k.name}` : (r.error ?? 'link failed'));
    if (r.ok) await loadConfigure(sel.agent ?? cfgParsed.agent ?? cfgProfile);
  };

  const onLiveMessage = (msg: any) => {
    const { topic, env } = msg;
    setCount((c) => c + 1);
    setBuffer((prev) => [...prev.slice(Math.max(0, prev.length - BUFFER_CAP + 1)), msg]);
    const noun = agentOf(topic);
    if (noun) {
      const session = topic.match(/^obs\/agent\/[^/]+\/([^/]+)\//)?.[1];
      touchAgent(noun, { live: true, sessions: session ? [session] : [] });
    }
    const p = env?.payload && typeof env.payload === 'object' ? env.payload : {};
    if (topic.startsWith('signal/')) {
      setSignal({ lit: true, label: topic });
      return;
    }
    if (topic.startsWith('in/human/')) {
      const corr = env.correlation_id;
      const agent = corrAgent.current.get(corr) ?? (refs.current.sel.kind === 'agent' ? refs.current.sel.agent : null) ?? [...refs.current.agents.keys()][0] ?? refs.current.defaultAgent;
      const session = corrSession.current.get(corr);
      const cur = agent ? currentConversation(agent) : '';
      // Only render a reply into the open thread when we can positively attribute
      // it (session known AND === the open conversation). If the
      // correlation→session mapping is unknown — the originating in/agent wasn't
      // seen live, e.g. an event-triggered or resumed-from-history thread —
      // blind-appending would leak a reply into whatever thread happens to be
      // open. Instead refresh the list, and best-effort backfill the open thread
      // from the durable transcript (idempotent via mergeConvMessages) in case
      // the reply actually belongs to it.
      if (agent && cur && session === cur) {
        if (p.failed) addFailure(agent, env);
        else if (p.question != null) addAsk(agent, env);
        else if (typeof p.text === 'string') addConv(agent, { who: 'agent', cls: 'agent', text: p.text, corr, ts: env.ts ?? new Date().toISOString() });
      } else if (agent && cur && !session) {
        void loadConversation(agent, cur);
      }
      if (agent) void loadConversations(agent);
      return;
    }
    if (topic.startsWith('in/agent/')) {
      // M6 (agent-comms-ui): the algedonic tell. Light the global signal lamp
      // when a HIGH-priority agent-to-agent delivery (or a failure-mail) crosses
      // the stream, so a human watching ANY view gets the cue. Keyed on the EVENT
      // (deduped by event_id), NOT a hook firing — a flaky tool re-emitting cannot
      // strobe the lamp. Priority rides the announced line / payload when present;
      // absent, only failures and the >=threshold deliveries light it.
      const evId = env.event_id ?? env.id;
      const prio = typeof env.priority === 'number' ? env.priority : (typeof p.priority === 'number' ? p.priority : 0);
      const isUrgent = prio >= HIGH_PRIORITY_THRESHOLD || p.failed === true;
      // Require an event id to light the lamp. Real deliveries (record_delivery /
      // failure-mail) always carry one, so this only suppresses the idless edge —
      // an idless stream of re-emissions can't strobe the lamp (no dedup key).
      if (isUrgent && evId != null && !seenLampEvents.current.has(evId)) {
        seenLampEvents.current.add(evId);
        setSignal({ lit: true, label: p.failed === true ? `failure: ${topic}` : `urgent: ${topic}` });
      }
    }
    if (noun && topic.startsWith('in/agent/')) {
      const session = sessionFromPayload(p, env);
      if (env.correlation_id) {
        corrAgent.current.set(env.correlation_id, noun);
        if (session) corrSession.current.set(env.correlation_id, session);
      }
      if (typeof p.prompt === 'string') {
        if (session && currentConversation(noun) === session && !sentCorrs.current.has(env.correlation_id)) addConv(noun, { key: `live:${env.correlation_id}:you:${p.prompt}`, who: 'you', cls: 'you', text: p.prompt, corr: env.correlation_id, ts: env.ts ?? new Date().toISOString() });
      } else if (p.answer != null) {
        closeAskFromOutside(env.correlation_id, p.answer);
      }
      if (session) void loadConversations(noun);
    }
  };

  const addConv = (agent: string, message: any) => {
    setConv((prev) => {
      const next = new Map(prev);
      next.set(agent, mergeConvMessages(next.get(agent) ?? [], [{ id: uid(), type: 'msg', ts: new Date().toISOString(), ...message }]));
      return next;
    });
  };
  const addFailure = (agent: string, env: any) => {
    const corr = env.correlation_id;
    if (corr && seenFailures.current.has(corr)) return;
    if (corr) seenFailures.current.add(corr);
    addConv(agent, { key: `live:${corr}:failed`, who: 'agent failed', cls: 'failed', text: env.payload?.error || 'the agent failed with no detail.', corr, failed: true, ts: env.ts ?? new Date().toISOString() });
  };
  const addAsk = (agent: string, env: any) => {
    const corr = env.correlation_id;
    if (corr && seenAsks.current.has(corr)) return;
    if (corr) seenAsks.current.add(corr);
    setConv((prev) => {
      const next = new Map(prev);
      next.set(agent, mergeConvMessages(next.get(agent) ?? [], [{ id: uid(), key: `live:${corr}:ask`, type: 'ask', corr, payload: env.payload ?? {}, answered: null, ts: env.ts ?? new Date().toISOString() }]));
      return next;
    });
  };
  const answerAsk = async (agent: string, msgId: string, corr: string, text: string) => {
    const ok = await publish(`in/agent/${agent}`, { answer: text }, corr);
    setConv((prev) => {
      const next = new Map(prev);
      next.set(agent, (next.get(agent) ?? []).map((m: any) => m.id === msgId ? { ...m, answered: `answered: ${text}${ok ? '' : '  (send failed)'}` } : m));
      return next;
    });
  };
  const closeAskFromOutside = (corr: string, ans: unknown) => {
    if (!corr) return;
    setConv((prev) => {
      const next = new Map(prev);
      for (const [agent, messages] of next) {
        next.set(agent, messages.map((m: any) => m.type === 'ask' && m.corr === corr && !m.answered
          ? { ...m, answered: `answered elsewhere: ${String(ans ?? '✓')}` }
          : m));
      }
      return next;
    });
  };

  const submitCompose = async (e: any) => {
    e.preventDefault();
    if (sel.kind !== 'agent') return;
    const input = e.currentTarget.querySelector('#compose-input') as HTMLInputElement;
    const text = input.value.trim();
    if (!text) return;
    const agent = sel.agent;
    const corr = `web-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    const session = currentConversation(agent) || newWebConversationId(agent);
    rememberConversation(agent, session);
    sentCorrs.current.add(corr);
    corrAgent.current.set(corr, agent);
    corrSession.current.set(corr, session);
    addConv(agent, { key: `live:${corr}:you:${text}`, who: 'you', cls: 'you', text, corr, ts: new Date().toISOString() });
    input.value = '';
    const btn = e.currentTarget.querySelector('#compose-send') as HTMLButtonElement;
    // M3 (reply-branching): if this session is a pending fork, attach the
    // structured `branched_from` to the seed publish (the kernel composes the
    // agent-visible quote from it). Consumed after the first send — a branch
    // seeds once.
    const target = branchTargets.current.get(session);
    const payload: any = { prompt: text, session };
    if (target) {
      payload.branched_from = { event_id: target.event_id, corr: target.corr, quote: target.quote, session: target.session };
      branchTargets.current.delete(session);
    }
    const ok = await publish(`in/agent/${agent}`, payload, corr);
    void loadConversations(agent);
    btn.textContent = ok ? 'accepted ✓' : 'failed ✕';
    btn.classList.toggle('sent', ok);
    setTimeout(() => { btn.textContent = '➤'; btn.classList.remove('sent'); }, 1400);
  };

  const loadSessions = async (agent: string) => {
    setSessionsState({ status: 'loading', sessions: [], transcript: null, error: '' });
    const j = await history({ kind: 'sessions', agent });
    if (j?.unavailable) setHistoryState(false);
    if (refs.current.sel.kind !== 'agent' || refs.current.sel.agent !== agent || refs.current.sel.tab !== 'sessions') return;
    if (!j?.ok) { setSessionsState({ status: 'error', sessions: [], transcript: null, error: j?.error ?? 'turn on the history view under add-ons to browse transcripts.' }); return; }
    setHistoryState(true);
    for (const s of j.sessions ?? []) touchAgent(agent, { sessions: [s.session] });
    setSessionsState({ status: 'list', sessions: j.sessions ?? [], transcript: null, error: '' });
  };

  const openTranscript = async (agent: string, session: string, beforeId?: number, prepend = false, label = 'conversation') => {
    if (!prepend) setSessionsState({ status: 'transcript-loading', sessions: [], transcript: { session, title: label, messages: [], has_more: false }, error: '' });
    const params: any = { kind: 'transcript', session };
    if (beforeId != null) params.before_id = beforeId;
    const j = await history(params);
    if (j?.unavailable) setHistoryState(false);
    if (refs.current.sel.kind !== 'agent' || refs.current.sel.agent !== agent || refs.current.sel.tab !== 'sessions') return;
    if (!j?.ok) { setSessionsState({ status: 'error', sessions: [], transcript: null, error: j?.error }); return; }
    setHistoryState(true);
    setSessionsState((prev: any) => ({
      status: 'transcript',
      sessions: [],
      transcript: {
        agent,
        session,
        title: prev.transcript?.title || label,
        messages: prepend ? [...(j.messages ?? []), ...(prev.transcript?.messages ?? [])] : (j.messages ?? []),
        has_more: j.has_more,
      },
      error: '',
    }));
  };

  const filteredRail = buffer.filter((m) => {
    const t = m.topic;
    const inScope = sel.kind === 'agent' ? t.startsWith(`obs/agent/${sel.agent}/`) || t.startsWith(`in/agent/${sel.agent}`) : true;
    const match = filter === 'all' ? true
      : filter === 'work' ? t.startsWith('in/')
        : filter === 'tools' ? /^obs\/[^/]+\/[^/]+\/[^/]+\/tool\//.test(t)
          : t.startsWith('signal/');
    return inScope && match;
  }).slice(-600);

  return (
    <div className="app-shell">
      <div className="vignette" aria-hidden="true" />
      <header className="mast">
        <button className={`mast-left${sel.kind === 'welcome' ? ' on' : ''}`} id="mast-home" title="home" onClick={selectWelcome}>
          <h1 className="mast-wordmark" aria-label="lanius">
            <svg className="mast-wordmark-svg" viewBox="0 0 231 76" role="img" aria-hidden="true">
              <g fill="none" stroke="currentColor" strokeWidth="11" strokeLinecap="round" strokeLinejoin="round">
                <path d="M16 11 L16 62.5" />
                <circle cx="50" cy="48" r="14.5" />
                <path d="M64.5 33.5 V62.5" />
                <path d="M84 62.5 V33.5 M84 46 C84 39 89.5 33.5 98 33.5 C106.5 33.5 112 39 112 46 V62.5" />
                <path d="M127.5 33.5 V62.5" />
                <path d="M143 33.5 V50 C143 57.5 149 62.5 157 62.5 C165 62.5 171 57.5 171 50 V33.5" />
                <path strokeWidth="10.5" d="M213.5 38.5 C211 35 206.5 33.5 201.5 33.5 C194.5 33.5 190 36.5 190 41 C190 45.5 194.5 47 201.5 48 C208.5 49 213 50.5 213 55 C213 59.5 208 62.5 201.5 62.5 C196.5 62.5 192 61 189.5 57.5" />
              </g>
              {/* the tittle of the i is the 01 thorn, in the reserved brand red */}
              <path style={{ fill: 'var(--accent, #E5484D)' }} transform="translate(117.3 5) scale(0.375)" d="M16 56 C16 34 30 16 54 8 C40 24 34 38 34 56 Z" />
            </svg>
          </h1>
          <span className="mast-sub">your agents</span>
        </button>
        <div className="mast-right">
          <label className="theme-control" htmlFor="theme-mode" title="theme">
            <span aria-hidden="true">{themeChoice === 'dark' ? '☾' : themeChoice === 'light' ? '☀' : '◐'}</span>
            <select id="theme-mode" aria-label="theme" value={themeChoice} onChange={(e) => setThemeChoice(e.target.value as ThemeChoice)}>
              {THEME_CHOICES.map((choice) => <option key={choice} value={choice}>{choice}</option>)}
            </select>
          </label>
          {/* M2 (agentic-configuration): the AI panel toggle — available from
              every view (the panel it opens is mounted once, outside the
              sel-gated tree, below). */}
          <button id="ai-panel-toggle" type="button" className={`lamp${aiPanelOpen ? ' lit' : ''}`} aria-pressed={aiPanelOpen} title={aiPanelOpen ? 'close the helper' : 'ask the helper — status, navigation, and setup help'} onClick={() => setAiPanelOpen((v) => !v)}>
            <span aria-hidden="true">✦</span><span className="lamp-label">helper</span>
          </button>
          <button id="signal-lamp" className={`lamp${signal.lit ? ' lit' : ''}`} title="urgent alerts — click to acknowledge" onClick={() => setSignal({ lit: false, label: 'signal' })}>
            <span className="lamp-dot" /><span id="signal-label">{signal.label}</span>
          </button>
          <span id="conn" className={`conn ${conn.connected ? 'conn-up' : 'conn-down'}`}><span className="conn-dot" /><span id="conn-text">{conn.text}</span></span>
        </div>
      </header>

      <div className="body-row">
      <main className="deck">
        <Nav agents={agents} conversations={conversations} sel={sel} historyOk={historyOk} selectAgent={selectAgent} openConversation={openConversation} selectSignals={selectSignals} selectSetup={selectSetup} selectCodeSessions={selectCodeSessions} selectComms={selectComms} selectProviders={selectProviders} navOpen={navOpen} setNavOpen={setNavOpen} exploreLabel="explore" />

        <section className="stage panel" aria-label="view">
          <div className="panel-head">
            <h2 id="stage-title">{stageTitle}</h2>
            <div id="agent-tabs" className="tabs" aria-label={`${sel.agent} views`} hidden={sel.kind !== 'agent'}>
              {(['converse', 'sessions', 'telemetry', 'configure'] as const).map((tab) => {
                const on = sel.kind === 'agent' && sel.tab === tab;
                const display = tab === 'sessions' ? 'History' : tab === 'telemetry' ? 'Activity' : tab;
                return tab === 'configure'
                  ? <IconButton key={tab} data-tab={tab} label={`configure ${sel.agent}`} className={on ? 'on tab-icon-btn' : 'tab-icon-btn'} aria-pressed={on} onClick={() => sel.kind === 'agent' && selectAgent(sel.agent, tab)}>⚙</IconButton>
                  : <button key={tab} data-tab={tab} className={on ? 'on' : ''} aria-pressed={on} onClick={() => sel.kind === 'agent' && selectAgent(sel.agent, tab)}>{display}</button>;
              })}
            </div>
            <span id="stage-note" className="panel-note">{stageNote}</span>
          </div>

          <WelcomeView hidden={sel.kind !== 'welcome'} primary={primaryAgent()} historyOk={historyOk} systemStatus={systemStatus} selectAgent={selectAgent} selectSetup={selectSetup} selectSignals={selectSignals} />
          <ConverseView
            hidden={!(sel.kind === 'agent' && sel.tab === 'converse')}
            agent={sel.agent}
            messages={conv.get(sel.agent) ?? []}
            conversations={sel.kind === 'agent' ? conversationStateFor(sel.agent) : { list: [] }}
            current={sel.kind === 'agent' ? currentConversation(sel.agent) : ''}
            submitCompose={submitCompose}
            answerAsk={answerAsk}
            selectAgent={selectAgent}
            openConversation={openConversation}
            newConversation={newConversation}
            startBranch={startBranch}
            branchOrigin={sel.kind === 'agent' ? branchOrigins.get(currentConversation(sel.agent)) : undefined}
            selectCodeSessions={selectCodeSessions}
            isTraceAgent={sel.kind === 'agent' && (isWorkerAgentName(sel.agent) || [...(agents.get(sel.agent)?.sessions ?? [])].some((s) => isWorkerSessionId(s)))}
            sendLabel="Send"
            allowHtml={systemStatus?.trust === 'full'}
          />
          <SessionsView hidden={!(sel.kind === 'agent' && sel.tab === 'sessions')} state={sessionsState} agent={sel.agent} openTranscript={openTranscript} loadSessions={loadSessions} />
          <ConfigureView
            hidden={!(sel.kind === 'agent' && sel.tab === 'configure')}
            modelOptions={modelOptions}
            form={cfgForm}
            setForm={(patch: any) => setCfgForm((f: any) => ({ ...f, ...patch }))}
            cfgProfile={cfgProfile}
            cfgParsed={cfgParsed}
            cfgLoading={cfgLoading}
            cfgNote={cfgNote}
            setCfgNote={setCfgNote}
            cfgToml={cfgToml}
            setCfgToml={setCfgToml}
            cfgTomlNote={cfgTomlNote}
            saveConfigure={saveConfigure}
            saveRawToml={saveRawToml}
            cfgPackages={cfgPackages}
            cfgKits={cfgKits}
            cfgConfigPackages={cfgConfigPackages}
            cfgSharedConfigRows={cfgSharedConfigRows}
            setCfgSharedConfigRows={setCfgSharedConfigRows}
            cfgContextChain={cfgContextChain}
            setCfgContextChain={setCfgContextChain}
            cfgContextVarEdits={cfgContextVarEdits}
            setCfgContextVarEdits={setCfgContextVarEdits}
            contextDefs={contextDefs}
            availableContextStages={availableContextStages}
            moveContextStage={moveContextStage}
            removeContextStage={removeContextStage}
            addContextStage={addContextStage}
            saveContextStageFromAssistant={saveContextStageFromAssistant}
            skillIncluded={skillIncluded}
            skillExcluded={skillExcluded}
            setSkillExcluded={setSkillExcluded}
            setKitPackagesExcluded={setKitPackagesExcluded}
            openKitModal={openKitModal}
            selectProviders={selectProviders}
          />
          {sel.kind === 'code-sessions' && <CodeSessions focus={sel.focus} />}
          {sel.kind === 'comms' && <CommsView onSelectSession={selectCodeSession} />}
          {sel.kind === 'providers' && <ProvidersView />}
          <SetupView
            hidden={sel.kind !== 'setup'}
            setup={setup}
            systemStatus={systemStatus}
            liveness={liveness}
            provenance={provenance}
            profiles={diskProfiles}
            newAgent={newAgent}
            setNewAgent={setNewAgent}
            newAgentNote={newAgentNote}
            createAgent={createAgent}
            modelsHint={modelsHint}
            modelOptions={modelOptions}
            loadSetup={loadSetup}
            selectAgent={selectAgent}
            selectProviders={selectProviders}
            openHelperChat={() => setAiPanelOpen(true)}
          />
          <RailView hidden={!(sel.kind === 'signals' || (sel.kind === 'agent' && sel.tab === 'telemetry'))} filter={filter} setFilter={setFilter} paused={paused} setPaused={setPaused} rows={filteredRail} />

          <KitModal
            modalRef={kitModalRef}
            open={kitModalOpen}
            close={closeKitModal}
            kits={cfgKits}
            cfgForm={cfgForm}
            cfgPackages={cfgPackages}
            cfgKitDetails={cfgKitDetails}
            loadKitDetail={loadKitDetail}
            installKitForAgent={installKitForAgent}
          />
        </section>
      </main>

      {/* M2 (agentic-configuration): the AI panel — a non-modal, right-side
          surface mounting the helper profile. Rendered here (a sibling of
          `.deck` inside `.body-row`, not inside any `sel`-gated view) so it
          toggles from every view. A real flex column (not a fixed overlay),
          so opening it shrinks `.deck` instead of covering the masthead or
          other page content. Mounted only while open (mirrors the
          context-assistant modal's own comment): an always-mounted
          AgentAssistant would fire its opening publish on every page load. */}
      {aiPanelOpen && (
        <aside id="ai-panel" className="ai-panel" aria-label="helper assistant">
          <div className="ai-panel-head">
            <strong>helper</strong>
            <IconButton label="close the helper panel" className="cfg-icon-btn" onClick={() => setAiPanelOpen(false)}>×</IconButton>
          </div>
          {/* M3 (agentic-configuration): no dead ends. World c (no provider, no
              logged-in coding CLI) means a helper turn would only fail — say so
              plainly and point at setup instead of sending a doomed prompt. */}
          {systemStatus?.llm?.world === 'c' ? (
            <div id="ai-panel-no-llm" className="ai-panel-empty">
              <p className="dim-note">No LLM path is set up yet, so the helper can't run turns. Add a model provider (or sign in to a coding CLI) first — this is one more step, not a dead end.</p>
              <button onClick={() => { selectSetup(); setAiPanelOpen(false); }}>go to setup →</button>
            </div>
          ) : (
            <AgentAssistant
              profile="helper"
              title="helper"
              intro="Ask me to help you get set up, or about anything here — your agents, models, and settings. I can look things up and take you where you need to go."
              tools={helperTools}
              onDone={() => setAiPanelOpen(false)}
            />
          )}
        </aside>
      )}
      </div>

      <footer className="strip">
        <span id="stat-count">{count} event{count === 1 ? '' : 's'}</span>
        <span id="stat-broker" />
        <span className="strip-note">same authority as your terminal — because it is your terminal</span>
      </footer>
    </div>
  );
}
