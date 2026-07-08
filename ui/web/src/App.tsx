import { useEffect, useMemo, useRef, useState } from 'react';
import CodeSessions from './CodeSessions';
import CommsView from './CommsView';
import Markdown from './Markdown';
import ProvidersView from './ProvidersView';
import AgentAssistant, { ClientTool } from './components/AgentAssistant';
import { adminGet, adminPost, adminPut, history, publish, status as fetchStatus, liveness as fetchLiveness } from './api';
import { openLiveStream } from './live';
import { Button, IconButton, ModelField, WorkdirInput } from './components/primitives';

// Product setup language is guided by docs/journeys/README.md and
// docs/layering.md. Durable browser-flow expectations live in
// docs/ui-flows/README.md and docs/ui-flows/configuration.md.
const BUFFER_CAP = 2000;
const PARENT_PATH = '$parent';
// M6 (agent-comms-ui): the priority at/above which an agent-to-agent delivery is
// "urgent" and lights the global signal lamp. Mirrors the backend default
// (agent-comms.high_priority_threshold = 5).
const HIGH_PRIORITY_THRESHOLD = 5;

const arr = (v: unknown) => String(v ?? '').split(',').map((x) => x.trim()).filter(Boolean);
const csv = (values: unknown) => Array.isArray(values) ? values.join(', ') : '';
const shortTs = (t: unknown) => (typeof t === 'string' ? t.replace('T', ' ').slice(0, 19) : '');
const timeOf = (env: any) => {
  const d = new Date(env?.ts ?? Date.now());
  return isNaN(d.getTime()) ? '--:--:--' : d.toTimeString().slice(0, 8);
};
// Deterministic per-agent identity chip: a small monogram in a bordered box.
// The brand is disciplined about color — the thorn (red) must always be the
// loudest thing on screen — so chips are NOT a rainbow. Each agent gets only a
// whisper of per-name hue within a narrow cool grey-blue band; the lightness
// (and thus contrast) comes from the theme via CSS, not a hardcoded hex.
function agentHue(name: string) {
  let h = 0;
  for (const c of String(name).toLowerCase()) h = (h * 31 + c.charCodeAt(0)) | 0;
  return 198 + (Math.abs(h) % 42); // 198–239: cool blue-grey, never warm
}
function AgentChip({ name, size = 'sm' as 'sm' | 'md' | 'lg', className = '' }: { name: string; size?: 'sm' | 'md' | 'lg'; className?: string }) {
  const mono = String(name).trim().slice(0, 2).toUpperCase() || '??';
  return <span className={`agent-chip agent-chip-${size}${className ? ` ${className}` : ''}`} style={{ ['--chip-h' as any]: agentHue(name) }} aria-hidden="true">{mono}</span>;
}
const relativeTime = (t: unknown) => {
  const d = new Date(String(t ?? ''));
  if (isNaN(d.getTime())) return '';
  const sec = Math.max(0, Math.floor((Date.now() - d.getTime()) / 1000));
  if (sec < 60) return 'now';
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hrs = Math.floor(min / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return days < 14 ? `${days}d ago` : shortTs(t).slice(0, 10);
};
const summarize = (p: unknown, max = 110) => {
  if (p == null) return '';
  const s = typeof p === 'string' ? p : JSON.stringify(p);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
};
const conversationLabel = (s: any) => s?.title || s?.preview || 'conversation';
const agentOf = (topic: string) => topic.match(/^(?:in|obs)\/agent\/([^/]+)/)?.[1] ?? null;
const uid = () => Math.random().toString(36).slice(2);
const newWebConversationId = (agent: string) => `web-${agent}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
const conversationStorageKey = (agent: string) => `lanius.currentConversation.${agent}`;
type ThemeChoice = 'system' | 'light' | 'dark';
const THEME_CHOICES: ThemeChoice[] = ['system', 'light', 'dark'];
// Coding-tool agent NOUNS are `claude-code` and `codex` (src/codeagent.rs); bare
// `claude` is only a CLI alias, never a bus agent — keep it out so a real agent a
// user names `claude` isn't evicted to Workers. The `code-*` session id (see
// isWorkerSessionId) is the reliable per-run fallback.
const codingAgentNames = new Set(['claude-code', 'codex']);
const isWorkerAgentName = (name: string) => codingAgentNames.has(String(name ?? '').toLowerCase());
const isWorkerSessionId = (session: string) => /^code-[A-Za-z0-9_-]+/.test(String(session ?? ''));
const sessionFromPayload = (payload: any, env: any) => payload?.session || (env?.correlation_id ? `evt-${env.correlation_id}` : env?.id ? `evt-${env.id}` : '');
// Content-identity for a conversation message, IDENTICAL to convKey in
// server.mjs. The same logical message arrives both as a live bus event (keyed
// by correlation) and from the durable backfill (which may lack a correlation),
// so they must collapse to one key or every reply doubles on re-open. (class,
// text) is the only attribute both sources share; asks/failures carry no text so
// they key on correlation. Trade-off: two identical same-class texts in one
// thread merge — rare, and the right call versus guaranteed duplication.
const convMessageKey = (m: any) => {
  const cls = m.cls ?? (m.who === 'you' ? 'you' : 'agent');
  if (m.type === 'ask') return `ask:${m.corr ?? m.event_id ?? ''}`;
  if (cls === 'failed') return `failed:${m.corr ?? m.event_id ?? ''}`;
  return `${cls}:${String(m.text ?? '')}`;
};
const mergeConvMessages = (current: any[] = [], incoming: any[] = []) => {
  const byKey = new Map();
  for (const m of [...current, ...incoming]) {
    const key = convMessageKey(m);
    if (!byKey.has(key)) byKey.set(key, { id: m.id ?? uid(), ...m, key });
  }
  return [...byKey.values()].sort((a, b) => String(a.ts ?? '').localeCompare(String(b.ts ?? '')));
};

function topicFilterMatches(filterText: string, value: string) {
  const f = String(filterText ?? '');
  if (f === '#') return true;
  const fp = f.split('/');
  const vp = String(value ?? '').split('/');
  for (let i = 0, j = 0; i < fp.length; i++, j++) {
    if (fp[i] === '#') return true;
    if (j >= vp.length) return false;
    if (fp[i] !== '+' && fp[i] !== vp[j]) return false;
  }
  return fp.length === vp.length;
}

function packageSource(pkg: any) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return { kind: 'linked', label: parts[kits + 1], icon: '↗' };
  const packages = parts.lastIndexOf('packages');
  if (packages >= 0) return { kind: 'copied', label: 'instance', icon: '⬚' };
  return { kind: 'path', label: 'path entry', icon: '•' };
}

function kitNameFor(pkg: any) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return parts[kits + 1];
  const source = packageSource(pkg);
  return source.kind === 'copied' ? 'instance' : source.label;
}

function firstSentence(text: string) {
  const compact = String(text ?? '').replace(/\s+/g, ' ').trim();
  if (!compact) return '';
  const sentence = compact.match(/^(.{20,220}?[.!?])\s/)?.[1] ?? compact;
  return sentence.length > 180 ? `${sentence.slice(0, 177)}...` : sentence;
}

function shortList(values: string[] = [], max = 2) {
  const list = values.filter(Boolean);
  if (!list.length) return '';
  const shown = list.slice(0, max).join(', ');
  return list.length > max ? `${shown}, +${list.length - max}` : shown;
}

function packageDescription(pkg: any) {
  const manifest = pkg.manifest ?? {};
  if (manifest.description) return firstSentence(manifest.description);
  if (pkg.skill?.description) return pkg.skill.description;
  if (manifest.process?.mode === 'daemon') return 'resident actor on the bus';
  if (manifest.process?.mode === 'exec') return 'per-event script actor';
  if (manifest.hooks) return 'policy hook package';
  if ((manifest.stages ?? []).length) return 'context helper package';
  if ((manifest.mcp ?? []).length) return 'MCP tool server package';
  return 'package';
}

function actorDetail(pkg: any) {
  const manifest = pkg.manifest ?? {};
  const bits = [];
  const process = manifest.process;
  if (process?.mode === 'daemon') bits.push(`runs ${process.run ?? 'its script'} as a resident actor`);
  else if (process?.mode === 'exec') bits.push(`runs ${process.run ?? 'its script'} for each matching event`);
  const request = manifest.request ?? {};
  const subscribes = shortList(request.subscribe);
  if (subscribes) bits.push(`listens on ${subscribes}`);
  const publishes = shortList(request.publish);
  if (publishes) bits.push(`can emit ${publishes}`);
  const blocking = shortList(request.blocking);
  if (blocking) bits.push(`can block ${blocking}`);
  if (process?.http) bits.push('serves a local HTTP endpoint');
  if (manifest.hooks) bits.push(`declares ${manifest.hooks} hook${manifest.hooks === 1 ? '' : 's'}`);
  if ((manifest.stages ?? []).length) bits.push(`adds ${manifest.stages.length} context helper${manifest.stages.length === 1 ? '' : 's'}`);
  if ((manifest.mcp ?? []).length) bits.push(`exposes ${manifest.mcp.length} MCP server${manifest.mcp.length === 1 ? '' : 's'}`);
  if (manifest.cron) bits.push(`schedules ${manifest.cron} recurring event${manifest.cron === 1 ? '' : 's'}`);
  const summary = bits.length ? `${bits.join('; ')}.` : packageDescription(pkg);
  const desc = manifest.description && firstSentence(manifest.description) !== manifest.description ? manifest.description : '';
  return desc ? `${summary} ${desc}` : summary;
}

function packageBadges(pkg: any) {
  const badges = [];
  const manifest = pkg.manifest ?? {};
  if (pkg.skill) badges.push({ cls: 'badge', text: 'skill' });
  if (manifest.process?.mode) badges.push({ cls: 'badge badge-wait', text: manifest.process.mode === 'daemon' ? 'Service' : manifest.process.mode });
  if (manifest.process?.http) badges.push({ cls: 'badge badge-wait', text: 'http' });
  if (manifest.hooks) badges.push({ cls: 'badge badge-wait', text: 'Event handler' });
  if (manifest.cron) badges.push({ cls: 'badge badge-wait', text: 'cron' });
  if (manifest.providers) badges.push({ cls: 'badge badge-wait', text: 'provider' });
  if ((manifest.stages ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'Prompt step' });
  if ((manifest.mcp ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'Tool' });
  return badges;
}

function grantState(pkg: any) {
  const grants = pkg.grants ?? [];
  if (!grants.length) return 'no review record';
  if (grants.some((g: any) => g.state === 'requested')) return 'needs review';
  if (grants.some((g: any) => g.state === 'approved')) return 'allowed';
  return grants[0]?.state ?? 'unknown';
}

// UI-truthfulness M1: turn a capability's latest liveness (from /api/liveness,
// keyed by package name) into the product word the interface shows. A capability
// the dispatcher has never spawned has no status entry → "not started", which is
// visibly distinct from "running". `state` drives a CSS class so failed/stopped
// read differently from running at a glance.
function livenessState(liveness: any, name: string) {
  const status = liveness?.actors?.[name]?.status;
  if (!status) return { label: 'not started', cls: 'idle' };
  if (status === 'running') return { label: 'running', cls: 'ok' };
  if (status === 'failed') return { label: 'failed', cls: 'bad' };
  if (status === 'stopped') return { label: 'stopped', cls: 'idle' };
  if (status === 'restarting') return { label: 'restarting', cls: 'warn' };
  return { label: status, cls: 'idle' };
}

function riskBadges(pkg: any) {
  const manifest = pkg.manifest ?? {};
  const request = manifest.request ?? {};
  const badges: string[] = [];
  if ((request.fs_write ?? []).length) badges.push('writes files');
  if (manifest.process?.mode === 'daemon') badges.push('daemon');
  if (manifest.process?.http) badges.push('local http');
  if (manifest.hooks) badges.push('hook');
  if ((manifest.mcp ?? []).length) badges.push('mcp');
  if ((manifest.stages ?? []).length) badges.push('Adds to prompts');
  if ((manifest.config?.agent_tunable ?? []).length) badges.push('Agent can change this');
  if ((request.publish ?? []).some((v: string) => v === '#' || v.endsWith('/#'))) badges.push('Posts widely');
  const state = grantState(pkg);
  if (state === 'needs review') badges.unshift('needs review');
  else if (state === 'allowed') badges.unshift('allowed');
  return badges.length ? badges : ['Low risk'];
}

function capabilityOutcome(kit: any) {
  const hook = String(kit.hook ?? '').trim();
  if (hook) return hook;
  if (/core/i.test(kit.name)) return 'core agent behaviors and skills';
  if (/dev/i.test(kit.name)) return 'developer safety and coding-workflow helpers';
  if (/funnel/i.test(kit.name)) return 'turn incoming work into structured agent tasks';
  return 'adds reusable behavior to your agents';
}

// Cost honesty (journey 03): the label set is hard cap / soft limit / estimate /
// unknown, and they must not be conflated. A run-step limit truly bounds one
// activation's model/tool loop — a HARD CAP. A throttle (tokens/hour, max
// concurrent) only SLOWS an agent — a SOFT LIMIT, not an activation cap. Keeping
// them in separate lists is what lets the UI separate hard limits from estimates.
function costSummary(profile: any, fallbackModel = '') {
  const model = profile?.model ?? fallbackModel;
  const turns = profile?.max_turns;
  const autonomy = profile?.autonomy ?? 'off';
  const hardCaps = [];
  if (turns) hardCaps.push(`${turns} run steps`);
  const softLimits = [];
  const throttle = profile?.throttle ?? {};
  for (const [name, t] of Object.entries(throttle) as any) {
    if (t?.llm_tokens_per_hour) softLimits.push(`${name}: ${t.llm_tokens_per_hour} tokens/hour`);
    if (t?.max_concurrent) softLimits.push(`${name}: ${t.max_concurrent} concurrent`);
  }
  const parts = [];
  if (hardCaps.length) parts.push('hard cap set');
  if (softLimits.length) parts.push('soft limit set');
  return {
    model: model || 'provider default',
    autonomy,
    hardCaps,
    softLimits,
    label: parts.length ? parts.join(' · ') : 'no limits set yet',
  };
}

function autonomyConsequence(level = 'off') {
  switch (level) {
    case 'manual':
      return 'Agent setting changes can be prepared, but you still confirm before they take effect.';
    case 'assisted':
      return 'Low-risk agent setting changes may be accepted automatically; new add-ons and sandbox changes still ask you.';
    case 'autonomous':
      return 'This agent may accept its own routine setting changes without asking; high-risk changes still need you.';
    case 'off':
    default:
      return 'This agent cannot accept its own setting changes; every change waits for you.';
  }
}

function modelCostHint(model = '') {
  const m = model.toLowerCase();
  if (!m) return 'cost/performance: unknown until a model is chosen';
  if (/haiku|mini|small|cheap|fast/.test(m)) return 'cost/performance: cheap';
  if (/sonnet|balanced|medium/.test(m)) return 'cost/performance: balanced';
  if (/opus|gpt-5|large|pro|max|power/.test(m)) return 'cost/performance: powerful';
  return 'cost/performance: unknown';
}

function declaredConfigParams(pkg: any) {
  const byKey = new Map();
  for (const key of pkg.manifest?.config?.agent_tunable ?? []) {
    if (!key) continue;
    byKey.set(key, { key, type: 'string', label: key, help: 'This add-on allows an agent-specific value.', agent_tunable: true, agentScoped: true, source: 'add-on setting' });
  }
  for (const stage of pkg.manifest?.stages ?? []) {
    for (const param of stage.config ?? []) {
      if (!param.key) continue;
      byKey.set(param.key, {
        key: param.key,
        type: param.type ?? 'string',
        label: param.label || param.key,
        help: param.help || '',
        default: param.default,
        options: param.options ?? [],
        agent_tunable: param.agent_tunable === true,
        agentScoped: true,
        source: `agent context ${stage.name}`,
      });
    }
  }
  return [...byKey.values()].sort((a, b) => a.key.localeCompare(b.key));
}

function packageHasAgentScopedSettings(pkg: any) {
  return declaredConfigParams(pkg).some((param: any) => param.agentScoped);
}

function tomlDisplayValue(value: any, type = 'string') {
  if (value === undefined || value === null) return '';
  if (type === 'array') return JSON.stringify(value);
  if (type === 'boolean') return value ? 'true' : 'false';
  return String(value);
}

function parseConfigRows(raw = '') {
  const rows = [];
  for (const line of String(raw).split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('[')) continue;
    const m = trimmed.match(/^([A-Za-z0-9_.-]+)\s*=\s*(.*)$/);
    if (m) rows.push({ key: m[1], value: m[2] });
  }
  return rows;
}

function configRowMap(raw = ''): Map<string, string> {
  return new Map(parseConfigRows(raw).map((row) => [row.key, row.value] as [string, string]));
}

function displayConfigValue(raw: any, type = 'string') {
  if (raw === undefined || raw === null || raw === '') return '';
  const s = String(raw).trim();
  if (type === 'string') {
    try {
      const parsed = JSON.parse(s);
      if (typeof parsed === 'string') return parsed;
    } catch {}
  }
  return s;
}

function valueSourceLabel(source: string, agentName: string) {
  if (source === 'agent') return `overridden here for ${agentName || 'this agent'}`;
  if (source === 'shared') return 'from the shared default';
  if (source === 'package') return 'from the package default';
  return 'not set yet';
}

function effectiveConfigValue(param: any, sharedRows: Map<string, string>, profileVars: any = {}, agentName = '') {
  const key = param.key;
  const agentValue = profileVars?.[key];
  if (agentValue !== undefined && agentValue !== null) {
    return { value: String(agentValue), source: 'agent', label: valueSourceLabel('agent', agentName) };
  }
  const sharedValue = sharedRows.get(key);
  if (sharedValue !== undefined && String(sharedValue).trim() !== '') {
    return { value: displayConfigValue(sharedValue, param.type), source: 'shared', label: valueSourceLabel('shared', agentName) };
  }
  const defaultValue = tomlDisplayValue(param.default, param.type);
  if (defaultValue !== '') return { value: defaultValue, source: 'package', label: valueSourceLabel('package', agentName) };
  return { value: '', source: 'unset', label: valueSourceLabel('unset', agentName) };
}

function prunedSet(set: Record<string, any>) {
  const out: Record<string, any> = {};
  for (const [k, v] of Object.entries(set)) {
    if (v === '' && k !== 'sandbox.workdir' && k !== 'parent') continue;
    out[k] = v;
  }
  return out;
}

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

  const selectWelcome = () => setSel({ kind: 'welcome' });
  const selectSignals = () => { setFilter('signals'); setSel({ kind: 'signals' }); };
  // Observability M4: the coding-session tree (a "workers" surface). Minimal mount
  // — final placement belongs in the Workers nav the chat track is building.
  const selectCodeSessions = () => setSel({ kind: 'code-sessions' });
  // agent-comms-ui M2: the cross-agent comms plane (agent-to-agent mail + rooms).
  const selectComms = () => setSel({ kind: 'comms' });
  // model-providers M4: the Providers page (the named, encrypted credential vault).
  const selectProviders = () => setSel({ kind: 'providers' });
  // agent-comms-ui M2: cross-link a comms participant to its run in the runs view.
  const selectCodeSession = (session: string) => setSel({ kind: 'code-sessions', focus: session });
  const selectSetup = (status?: any) => {
    setSel({ kind: 'setup' });
    void loadSetup(status);
  };
  const selectAgent = (agent: string, tab?: string) => {
    setSel((prev: any) => ({ kind: 'agent', agent, tab: tab ?? (prev.kind === 'agent' && prev.agent === agent ? prev.tab : 'converse') }));
  };

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

function Nav({ agents, conversations, sel, historyOk, selectAgent, openConversation, selectSignals, selectSetup, selectCodeSessions, selectComms, selectProviders, navOpen, setNavOpen, exploreLabel }: any) {
  const items = [...agents.keys()].sort();
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
        <button className={`nav-item nav-workers${sel.kind === 'code-sessions' ? ' on' : ''}`} data-sel="code-sessions" title="coding runs and the workers they started" onClick={() => selectCodeSessions && selectCodeSessions()}><span className="nav-sigil">⚙</span> runs</button>
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
              {workerItems.map((name) => <button key={name} className="nav-item nav-worker" onClick={() => selectAgent(name, 'telemetry')}><span>{name}</span><span className="nav-convo-meta">{agents.get(name)?.sessions?.size ?? 0} run{(agents.get(name)?.sessions?.size ?? 0) === 1 ? '' : 's'}</span></button>)}
            </div>
          </details>
        )}
        <button id="nav-new-agent" className="nav-item nav-new" onClick={() => selectSetup()}><span className="nav-sigil">＋</span> new agent</button>
      </div>
      <div id="history-hint" className="nav-hint nav-foot" hidden={historyOk !== false}>transcripts unavailable — live view only</div>
    </nav>
  );
}

function WelcomeView({ hidden, primary, historyOk, systemStatus, selectAgent, selectSetup, selectSignals }: any) {
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
                <button onClick={() => selectAgent(primary, 'converse')}>converse with {primary}</button>
                <button className="ghost" onClick={() => selectAgent(primary, 'configure')}>configure</button>
              </div>
            </>
          )}
        </div>
        <div className="welcome-actions">
          <button id="welcome-new" className="ghost" onClick={() => selectSetup()}>＋ guided setup</button>
          <button id="welcome-signals" className="ghost" onClick={selectSignals}>◮ activity</button>
        </div>
        {historyOk === false && <p id="welcome-hint" className="dim-note">transcripts are unavailable until the history view is on.</p>}
      </div>
    </div>
  );
}

function SetupView({ hidden, setup, systemStatus, liveness, provenance, profiles, newAgent, setNewAgent, newAgentNote, createAgent, modelsHint, modelOptions, loadSetup, selectAgent, selectProviders, openHelperChat }: any) {
  // M3 (agentic-configuration): "runnable" means detection found either a
  // native provider (world a) or a logged-in coding CLI on PATH (world b) — the
  // helper can produce a real turn. World c (or not-yet-known) hides the chat
  // offer; the form wizard below is always available regardless.
  const helperRunnable = systemStatus?.llm?.world === 'a' || systemStatus?.llm?.world === 'b';
  const kits = setup.kits;
  const pkgs = setup.packages;
  const proposals = setup.proposals;
  const packages = pkgs?.packages ?? [];
  const primaryProfile = profiles?.find((p: any) => p.profile === 'default') ?? profiles?.[0] ?? null;
  const cost = costSummary(primaryProfile, newAgent.model);
  const capabilityOptions = (kits?.kits ?? []).filter((k: any) => !provenance.has(k.name));
  const health = [
    { label: 'Data folder', value: systemStatus?.root ?? 'checking...', state: systemStatus?.root_exists === false ? 'bad' : 'ok' },
    { label: 'Model key', value: systemStatus?.credential ?? 'checking...', state: systemStatus?.credential === 'present' ? 'ok' : 'bad' },
    { label: 'Message bus', value: systemStatus?.broker_connected ? 'connected' : 'not connected', state: systemStatus?.broker_connected ? 'ok' : 'bad' },
    { label: 'history', value: systemStatus?.history?.available ? 'available' : 'live-only', state: systemStatus?.history?.available ? 'ok' : 'warn' },
    // Read camera (read-provenance M3) — make all three states legible:
    // available & on (ok), available & off (warn — a real "off" state, not an
    // error), and the authoritative tier "unavailable here" (warn/neutral — an
    // ACCEPTED platform gap on non-Linux, reported honestly, NOT alarmed as bad).
    {
      label: 'Activity is readable (advisory)',
      value: systemStatus?.read_camera?.advisory?.enabled ? 'on' : 'off',
      state: systemStatus?.read_camera?.advisory?.enabled ? 'ok' : 'warn',
    },
    {
      label: 'Activity is readable (confirmed)',
      value: systemStatus?.read_camera?.authoritative?.available ? 'available' : 'unavailable here',
      state: systemStatus?.read_camera?.authoritative?.available ? 'ok' : 'warn',
    },
    // Cage posture (single-cage macOS increment) — writes/reads/network, in
    // product words. Off macOS every dimension reads "unavailable here" (warn,
    // an honest platform gap), never a silent "on". "writes open" / "reads
    // open" / "network open" are the default, unrestricted posture (neutral).
    {
      label: 'Sandbox — file writes',
      value: systemStatus?.cage?.write ?? 'checking...',
      state: systemStatus?.cage?.available === false ? 'warn' : 'ok',
    },
    {
      label: 'Sandbox — file reads',
      value: systemStatus?.cage?.read ?? 'checking...',
      state: systemStatus?.cage?.available === false ? 'warn' : 'ok',
    },
    {
      label: 'Sandbox — network',
      value: systemStatus?.cage?.network ?? 'checking...',
      state: systemStatus?.cage?.available === false ? 'warn' : 'ok',
    },
  ];
  return (
    <div id="view-setup" className="view" hidden={hidden}>
      <div className="setup-pane">
        <div id="setup-status" className={`setup-status${setup.statusKind ? ` status-${setup.statusKind}` : ''}`} role="status" aria-live="polite" hidden={!setup.status}>{setup.status}</div>

        <section className="setup-block setup-home">
          <div>
            <h3>setup home</h3>
            <p className="dim-note">First success starts here: local health, one useful agent, capabilities, cost limits, and risk signals.</p>
          </div>
          <div className="setup-health-grid">
            {health.map((item) => <div key={item.label} className={`setup-health-card is-${item.state}`}><span>{item.label}</span><strong>{item.value}</strong></div>)}
          </div>
          <div className="setup-next">
            <strong>recommended next step</strong>
            {!profiles?.length ? <span>Create your first agent, then send it one message.</span>
              : !packages.length ? <span>Add a capability so the agent can do useful work.</span>
                : <span>Open an agent for its history, activity, and settings.</span>}
          </div>
        </section>

        {helperRunnable && (
          <section id="setup-chat-offer" className="setup-block setup-chat-offer">
            <h3>set up by chatting</h3>
            <p className="dim-note">Tell the helper what you're trying to do — reads are transparent, and any change still asks before it lands. The form below still works if you'd rather drive it directly.</p>
            <button id="setup-open-helper-chat" onClick={openHelperChat}>chat with the helper →</button>
          </section>
        )}

        <section className="setup-block setup-wizard">
          <h3>guided new agent</h3>
          <p className="dim-note">Name it, give it a purpose, choose where it works, and put a visible run budget on day one.</p>
          <form className="wizard-form" onSubmit={(e) => { e.preventDefault(); void createAgent(); }}>
            <div className="wizard-grid">
              <label><span>1. name</span><input id="na-name" placeholder="kestrel" spellCheck={false} value={newAgent.name} onChange={(e) => setNewAgent({ ...newAgent, name: e.target.value })} /></label>
              <label><span>2. purpose</span><input id="na-purpose" placeholder="watch launches, draft briefs, triage issues..." spellCheck={false} value={newAgent.purpose} onChange={(e) => setNewAgent({ ...newAgent, purpose: e.target.value })} /></label>
              <label><span>3. home / workdir</span><WorkdirInput id="na-workdir" placeholder="optional path where tools should run" value={newAgent.workdir} onChange={(v) => setNewAgent({ ...newAgent, workdir: v })} /></label>
              <label><span>4. model</span><ModelField id="na-model" value={newAgent.model} onChange={(v) => setNewAgent({ ...newAgent, model: v })} models={modelOptions} onSetupProvider={selectProviders} /></label>
              <label><span>5. run-step cap</span><input id="na-turns" type="number" min="1" max="200" value={newAgent.turns} onChange={(e) => setNewAgent({ ...newAgent, turns: e.target.value })} /></label>
              <label><span>6. autonomy</span><select id="na-autonomy" value={newAgent.autonomy} onChange={(e) => setNewAgent({ ...newAgent, autonomy: e.target.value })}>{['off', 'manual', 'assisted', 'autonomous'].map((v) => <option key={v} value={v}>{v}</option>)}</select></label>
              <label><span>starting capability</span><select id="na-capability" value={newAgent.capability} onChange={(e) => setNewAgent({ ...newAgent, capability: e.target.value })}><option value="">none yet</option>{capabilityOptions.map((k: any) => <option key={k.name} value={k.name}>{k.name}</option>)}</select></label>
            </div>
            <div className="setup-row">
              <button id="na-create" type="submit" disabled={!newAgent.name.trim()}>create agent</button>
              <span id="na-note" className="dim-note">{newAgentNote || (!newAgent.name.trim() ? 'Name it to enable Create.' : 'Creates a normal profile; advanced settings remain inspectable.')}</span>
            </div>
            <p id="models-hint" className="dim-note" hidden={!modelsHint}>{modelsHint}</p>
          </form>
        </section>

        <details className="setup-block setup-cost setup-fold">
          <summary><h3>cost visibility</h3><span className="dim-note">how spend is bounded — model, autonomy, hard caps</span></summary>
          <p className="dim-note">Showing {primaryProfile?.agent || primaryProfile?.profile || 'the default agent'}.</p>
          <div className="cost-grid">
            <div><span>model</span><strong>{cost.model}</strong></div>
            <div><span>autonomy</span><strong>{cost.autonomy}</strong></div>
            <div><span>limits</span><strong>{cost.label}</strong></div>
          </div>
          <div className="cost-limits">
            <div className="cost-group cost-hard">
              <span className="cost-group-label">hard cap</span>
              {cost.hardCaps.length
                ? <div className="risk-badges">{cost.hardCaps.map((cap: string) => <span key={cap} className="badge">{cap}</span>)}</div>
                : <span className="dim-note">none set</span>}
            </div>
            <div className="cost-group cost-soft">
              <span className="cost-group-label">soft limit</span>
              {cost.softLimits.length
                ? <div className="risk-badges">{cost.softLimits.map((cap: string) => <span key={cap} className="badge badge-wait">{cap}</span>)}</div>
                : <span className="dim-note">none set</span>}
            </div>
            <div className="cost-group cost-estimate">
              <span className="cost-group-label">estimate</span>
              <span className="dim-note">unknown until provider pricing is known — per-run dollar estimates show in the runs view when a model is priced.</span>
            </div>
          </div>
          <p className="dim-note">Run-step caps are hard activation limits; throttles are soft limits that slow an agent, not activation caps. Dollar estimates are not shown until provider pricing is known; unknown is better than fake precision.</p>
        </details>

        <section className="setup-block">
          <h3>capability catalog</h3>
          <p className="dim-note">Ready-made outcomes for your agents. Expand a card before adding unfamiliar capabilities.</p>
          <div id="setup-kits">
            {setup.loading || !kits ? 'resolving…' : kits.ok === false ? <div className="dim-note">capabilities could not load: {kits.error ?? 'unknown - is the lanius binary on the server PATH current?'}</div>
              : <><CodingAgentCatalogCard />{!(kits.kits ?? []).length ? <div className="dim-note">no other capabilities found</div>
                : kits.kits.map((k: any) => <SetupKit key={k.name} kit={k} installed={provenance.has(k.name)} loadSetup={loadSetup} />)}</>}
          </div>
        </section>
        <details className="setup-block setup-fold" open>
          <summary><h3>installed capabilities</h3><span className="dim-note">add-ons already on this installation and their trust state</span></summary>
          <p className="dim-note">Installed is not the same as allowed or running. Use the badges to see current trust state.</p>
          <div id="setup-configs">
            {setup.loading || !pkgs ? 'checking…' : pkgs.ok === false ? <div className="dim-note">could not load installed capabilities: {pkgs.error ?? 'unknown error'}</div>
              : !(pkgs.packages ?? []).length ? <div className="dim-note">nothing added yet</div>
                : pkgs.packages.map((p: any) => <SetupPackageConfig key={p.name} pkg={p} loadSetup={loadSetup} liveness={liveness} />)}
          </div>
        </details>
        <details className="setup-block setup-trust setup-fold">
          <summary><h3>trust and footprint</h3><span className="dim-note">where data lives, what is local, what creates risk</span></summary>
          <p className="dim-note">A quick security summary: what runs locally, where your data is, and what could be risky.</p>
          <div className="trust-grid">
            <div><span>Owner</span><strong>{systemStatus?.owner ?? 'owner'}</strong></div>
            <div><span>This web app</span><strong>127.0.0.1:{systemStatus?.web?.port ?? '7180'}</strong></div>
            <div><span>database</span><strong>{systemStatus?.paths?.database?.path ?? 'unknown'}</strong></div>
            <div><span>config repo</span><strong>{systemStatus?.paths?.config?.path ?? 'unknown'}</strong></div>
          </div>
          <details>
            <summary className="dim-note">copyable security summary</summary>
            <pre className="setup-readme">{[
              `root: ${systemStatus?.root ?? 'unknown'}`,
              `principal: ${systemStatus?.owner ?? 'unknown'}`,
              `broker: ${systemStatus?.broker ?? 'unknown'} (${systemStatus?.broker_connected ? 'connected' : 'not connected'})`,
              `credential: ${systemStatus?.credential ?? 'unknown'}`,
              `history: ${systemStatus?.history?.available ? systemStatus.history.endpoint : 'live-only/unavailable'}`,
              `database: ${systemStatus?.paths?.database?.path ?? 'unknown'}`,
              `config: ${systemStatus?.paths?.config?.path ?? 'unknown'}`,
            ].join('\n')}</pre>
          </details>
        </details>
        <details className="setup-block setup-fold" open={(proposals?.proposals ?? []).length > 0}>
          <summary><h3>agent requests</h3><span className="dim-note">{(proposals?.proposals ?? []).length ? `${(proposals.proposals).length} waiting on you` : 'none right now'}</span></summary>
          <p className="dim-note">when an agent suggests a settings change, accept or decline it here.</p>
          <div id="setup-pending">
            {setup.loading || !proposals ? 'checking…' : proposals.ok === false ? <div className="dim-note">could not load agent requests: {proposals.error ?? 'unknown error'}</div>
              : !(proposals.proposals ?? []).length ? <div className="dim-note">no agent requests</div>
                : proposals.proposals.map((p: any) => <ProposalCard key={p.proposal} proposal={p} loadSetup={loadSetup} />)}
          </div>
        </details>
      </div>
    </div>
  );
}

function CodingAgentCatalogCard() {
  return (
    <div id="coding-agent-entry" className="setup-kit setup-kit-coming">
      <div className="setup-kit-head">
        <span className="setup-kit-name">coding agents</span>
        <span className="setup-kit-hook dim-note">Coming soon: run Codex or Claude Code with a repo sandbox, a recorded activity trail, and a visible spend ceiling.</span>
        <span className="badge badge-wait">coming</span>
        <button disabled>not connected yet</button>
      </div>
      <div className="capability-meta">
        <span>for: coding work you already do</span>
        <span>value: sandbox, recording, and cost control</span>
        <span>state: coming soon, not configured here yet</span>
      </div>
    </div>
  );
}

function SetupKit({ kit, installed, loadSetup }: any) {
  const [readme, setReadme] = useState('');
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const toggle = async () => {
    if (!open && !readme) {
      setReadme('fetching…');
      const r = await adminGet(`kits/readme?kit=${encodeURIComponent(kit.name)}`);
      setReadme(r.ok ? r.readme : (r.error ?? 'no readme'));
    }
    setOpen(!open);
  };
  const add = async () => {
    setBusy(true);
    const r = await adminPost('kits/add', { kit: kit.name });
    await loadSetup(r.ok ? { status: `added ${kit.name}.`, statusKind: 'ok' } : { status: `✕ couldn't add ${kit.name}: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
  };
  return (
    <div className="setup-kit">
      <div className="setup-kit-head">
        <span className="setup-kit-name">{kit.name}</span>
        <span className="setup-kit-hook dim-note">{capabilityOutcome(kit)}</span>
        {installed && <span className="badge">installed</span>}
        <button className="ghost" onClick={toggle}>details</button>
        <button onClick={add} disabled={busy}>{busy ? 'adding...' : installed ? 'add again' : 'add'}</button>
      </div>
      <div className="capability-meta">
        <span>for: useful behavior shared across agents</span>
        <span>needs: review details before adding unfamiliar kits</span>
        <span>state: {installed ? 'installed' : 'available'}</span>
      </div>
      <pre className="setup-readme" hidden={!open}>{readme}</pre>
    </div>
  );
}

function SetupPackageConfig({ pkg, loadSetup, liveness }: any) {
  const params = declaredConfigParams(pkg);
  const [rows, setRows] = useState<any[] | null>(null);
  const [note, setNote] = useState('');
  const [raw, setRaw] = useState('not loaded');
  const [confirmOff, setConfirmOff] = useState(false);
  const source = packageSource(pkg);
  const kit = (pkg.grants ?? []).map((g: any) => String(g.decided_by ?? '')).find((v: string) => v.startsWith('kit:'))?.slice(4) ?? '';
  const canUnlink = !!kit && source.kind === 'linked' && source.label === kit;
  const active = (pkg.grants ?? []).some((g: any) => g.state === 'approved');
  const live = livenessState(liveness, pkg.name);
  const loadRaw = async () => {
    setRaw('loading...');
    const r = await adminGet(`configs?package=${encodeURIComponent(pkg.name)}`);
    setRaw(r.ok ? (r.config?.toml || 'no settings yet') : (r.error ?? 'could not load settings'));
    return r.ok ? (r.config?.toml || 'no settings yet') : null;
  };
  const loadRows = async () => {
    const toml = await loadRaw();
    const current = toml ? parseConfigRows(toml) : [];
    const byKey = new Map(current.map((row) => [row.key, row.value] as [string, string]));
    const declared = new Map(params.map((param: any) => [param.key, param]));
    for (const param of params) if (!byKey.has(param.key)) byKey.set(param.key, tomlDisplayValue(param.default, param.type));
    for (const key of byKey.keys()) if (!declared.has(key)) declared.set(key, { key, type: 'string', label: key, help: 'Existing setting not declared in the add-on description.', source: 'current settings' });
    setRows([...declared.values()].sort((a: any, b: any) => a.key.localeCompare(b.key)).map((param: any) => ({
      param: { ...param, scope: 'applies to every agent that uses this add-on' },
      value: byKey.get(param.key) ?? '',
      note: '',
    })));
  };
  const saveRow = async (idx: number) => {
    if (!rows) return;
    const row = rows[idx];
    setRows(rows.map((r, i) => i === idx ? { ...r, note: 'saving...' } : r));
    const r = await adminPost('configs/set', { package: pkg.name, key: row.param.key, value: row.value });
    if (!r.ok) { setNote(r.error ?? 'save failed'); return; }
    await loadRows();
    setNote('saved and reloaded');
  };
  const unlink = async () => {
    if (!canUnlink) return;
    setNote('turning off...');
    const r = await adminPost('kits/unlink', { kit });
    await loadSetup(r.ok
      ? { status: `turned off ${kit} for this installation. The review record stays; add it again from the catalog to restore it.`, statusKind: 'ok' }
      : { status: `✕ couldn't turn off ${kit}: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
  };
  return (
    <div className="setup-pending-pkg">
      <div className="setup-kit-head"><span className="setup-kit-name">{pkg.name}</span><span className={active ? 'badge' : 'badge badge-wait'}>{active ? 'on' : 'off'}</span><span className={`badge live-${live.cls}`} title="whether this capability's background service is currently running">{live.label}</span><span className="dim-note">{grantState(pkg)}{kit ? ` · from ${kit}` : ''}</span>{canUnlink && <button className="ghost" onClick={() => setConfirmOff(!confirmOff)}>{confirmOff ? 'cancel' : 'turn off'}</button>}</div>
      <div className="cfg-package-detail">{packageDescription(pkg)}</div>
      <div className="risk-badges">{riskBadges(pkg).map((b) => <span key={b} className={`badge${/pending|broad|writes|daemon|http|hook|mcp|prompt/.test(b) ? ' badge-wait' : ''}`}>{b}</span>)}</div>
      <p className="dim-note">These settings apply to every agent that uses this add-on. Use an agent's configure tab when only one agent should change.</p>
      {source.kind === 'copied' && <p className="dim-note">Copied into this installation; removal is not supported here yet.</p>}
      {confirmOff && <div className="setup-confirm"><strong>Turn off {kit}?</strong><span>This removes it from this installation's add-on path. The review record stays, and adding it again restores it.</span><button onClick={unlink}>turn off {kit}</button></div>}
      <div className="setup-row">
        <button className="ghost" onClick={loadRows}>settings</button>
        <span className="dim-note">{note}</span>
      </div>
      <div className="cfg-package-config-panel setup-config-panel" hidden={rows === null}>{rows === null ? null : !rows.length ? <div className="dim-note">no configurable settings declared</div> : rows.map((row, idx) => <ConfigInputRow key={row.param.key} param={row.param} value={row.value} setValue={(v: string) => setRows(rows.map((r, i) => i === idx ? { ...r, value: v } : r))} save={<><IconButton label={`save ${pkg.name}.${row.param.key} for every agent — affects every agent using this add-on`} className="cfg-icon-btn cfg-shared-save" onClick={() => saveRow(idx)}>⚑ save</IconButton><span className="dim-note">{row.note}</span></>} />)}</div>
      <details onToggle={(e) => e.currentTarget.open && raw === 'not loaded' && void loadRaw()}>
        <summary className="dim-note">current settings</summary>
        <pre className="setup-readme">{raw}</pre>
      </details>
    </div>
  );
}

function ProposalCard({ proposal, loadSetup }: any) {
  const [diff, setDiff] = useState('');
  const [open, setOpen] = useState(false);
  const show = async () => {
    if (!open && !diff) {
      setDiff('loading...');
      const r = await adminGet(`proposals/show?id=${encodeURIComponent(proposal.proposal)}`);
      setDiff(r.ok ? r.diff : (r.error ?? 'could not load the change'));
    }
    setOpen(!open);
  };
  const decide = async (verb: 'accept' | 'decline') => {
    const r = await adminPost(`proposals/${verb}`, { id: proposal.proposal });
    await loadSetup(r.ok ? { status: verb === 'accept' ? 'accepted the change.' : 'declined the change.', statusKind: 'ok' } : { status: `✕ couldn't ${verb} it: ${r.error ?? 'unknown error'}`, statusKind: 'err' });
  };
  const who = proposal.agent && typeof proposal.agent === 'string' ? proposal.agent : 'an agent';
  return (
    <div className="setup-pending-pkg">
      <div className="setup-kit-name">{who} wants to change settings</div>
      <div className="dim-note">{(proposal.files ?? []).join(', ') || 'settings change'}</div>
      <div className="setup-row">
        <button className="ghost" onClick={show}>show change</button>
        <button onClick={() => decide('accept')}>accept</button>
        <button className="ghost" onClick={() => decide('decline')}>decline</button>
      </div>
      <pre className="setup-readme" hidden={!open}>{diff}</pre>
    </div>
  );
}

function ConfigureView(props: any) {
  const { hidden, modelOptions, form, setForm, cfgProfile, cfgParsed, cfgLoading, cfgNote, cfgToml, setCfgToml, cfgTomlNote, saveConfigure, saveRawToml, cfgPackages, cfgKits, cfgConfigPackages, cfgSharedConfigRows, setCfgSharedConfigRows, cfgContextChain, setCfgContextChain, cfgContextVarEdits, setCfgContextVarEdits, contextDefs, availableContextStages, moveContextStage, removeContextStage, saveContextStageFromAssistant, skillIncluded, skillExcluded, setSkillExcluded, setKitPackagesExcluded, openKitModal, selectProviders } = props;
  // model-providers M4: the named-provider tie-in. Load the vault list so the
  // agent can SELECT a provider (writing model.provider on save); when an api-key
  // provider is selected the model dropdown sources its list from that provider's
  // /models probe, and a native-login provider shows NEITHER a list NOR the
  // "provider list unavailable" warning (the real fix for the spurious warning on
  // a Claude.AI OAuth login).
  const [providers, setProviders] = useState<any[]>([]);
  const [providerModels, setProviderModels] = useState<any[]>([]);
  useEffect(() => {
    if (hidden) return;
    let alive = true;
    (async () => { const j = await adminGet('providers'); if (alive && j.ok) setProviders(j.providers ?? []); })();
    return () => { alive = false; };
  }, [hidden]);
  const selectedProvider = providers.find((p: any) => p.name === form.provider) ?? null;
  const providerIsNative = selectedProvider?.kind === 'native_login';
  const providerIsApiKey = selectedProvider?.kind === 'api_key';
  useEffect(() => {
    let alive = true;
    if (!form.provider || !providerIsApiKey) { setProviderModels([]); return; }
    (async () => {
      const j = await adminGet(`providers/test?name=${encodeURIComponent(form.provider)}`);
      if (alive) setProviderModels(j.ok && Array.isArray(j.models) ? j.models : []);
    })();
    return () => { alive = false; };
  }, [form.provider, providerIsApiKey]);
  // With a named provider chosen, the model list comes from IT (api-key) or is
  // suppressed (native). With no provider, fall back to the ambient model probe.
  const modelFieldModels = form.provider ? providerModels : modelOptions;
  const [contextAssistantOpen, setContextAssistantOpen] = useState(false);
  const contextAssistantRef = useRef<HTMLDialogElement | null>(null);
  const draggingContextIndex = useRef<number | null>(null);
  useEffect(() => {
    const el = contextAssistantRef.current;
    if (!el) return;
    if (contextAssistantOpen && !el.open) el.showModal();
    if (!contextAssistantOpen && el.open) el.close();
  }, [contextAssistantOpen]);
  const disabled = cfgLoading;
  const agentName = form.agent || cfgParsed.agent || cfgProfile || 'this agent';
  const cost = costSummary({ model: form.model, max_turns: Number(form.turns || 0), autonomy: form.autonomy, throttle: cfgParsed.throttle ?? {} }, form.model);
  const updateVar = (id: string, patch: any) => setForm({ varsRows: form.varsRows.map((r: any) => r.id === id ? { ...r, ...patch } : r) });
  const updateThrottle = (id: string, patch: any) => setForm({ throttleRows: form.throttleRows.map((r: any) => r.id === id ? { ...r, ...patch } : r) });
  const reorderContextStage = (from: number, to: number) => {
    if (from === to || from < 0 || to < 0 || from >= cfgContextChain.length || to >= cfgContextChain.length) return;
    setCfgContextChain((prev: any[]) => {
      const next = [...prev];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return next.map((s, i) => ({ ...s, order: (i + 1) * 10 }));
    });
  };
  const contextAuthorTools: ClientTool[] = useMemo(() => [
    {
      name: 'list_context_blocks',
      description: 'List context blocks available to add for this agent.',
      parameters: { type: 'object', properties: {} },
      handler: async () => ({
        blocks: availableContextStages.map((s: any) => ({
          key: `${s.package}/${s.name}`,
          package: s.package,
          name: s.name,
          mode: s.mode,
          timeout_ms: s.timeout_ms,
          settings: (s.config ?? []).map((p: any) => p.key).filter(Boolean),
        })),
      }),
    },
    {
      name: 'save_context_block',
      description: 'Add one context block to this agent and save through the normal configure path.',
      parameters: {
        type: 'object',
        properties: {
          stage: {
            type: 'object',
            properties: {
              key: { type: 'string' },
              package: { type: 'string' },
              name: { type: 'string' },
            },
          },
        },
        required: ['stage'],
      },
      handler: async (args: any) => saveContextStageFromAssistant(args.stage ?? args),
    },
  ], [availableContextStages, saveContextStageFromAssistant]);
  return (
    <div id="view-configure" className="view" hidden={hidden}>
      <div className="setup-pane cfg-pane">
        <aside className="cfg-index" aria-label="configure sections">
          {[
            ['essentials', 'essentials'],
            ['packages', 'add-ons'],
            ['advanced', 'advanced'],
            ['context', 'context'],
            ['sandbox', 'sandbox'],
            ['throttle', 'throttle'],
            ['raw', 'raw'],
          ].map(([id, label]) => <a key={id} href={`#cfg-section-${id}`}>{label}</a>)}
        </aside>
        <div className="cfg-sections">
          <section className="setup-block cfg-essentials" id="cfg-section-essentials">
            <h3><span className="section-glyph" aria-hidden="true">◎</span>essentials</h3>
            <p className="dim-note">These settings apply to {agentName} only. Edits land in <code id="cfg-file">{cfgProfile ? `${cfgProfile} settings` : "this agent's settings file"}</code> and apply on the agent's next run.</p>
            <div className="cfg-cost-summary">
              <div><span>model</span><strong>{cost.model}</strong><em>{modelCostHint(form.model)}</em></div>
              <div><span>autonomy</span><strong>{cost.autonomy}</strong><em id="cfg-autonomy-consequence">{autonomyConsequence(form.autonomy)}</em></div>
            </div>
            <div className="cfg-grid">
              <label id="cfg-section-agent">name <input id="cfg-agent" disabled={disabled} spellCheck={false} value={form.agent} onChange={(e) => setForm({ agent: e.target.value })} /></label>
              <label id="cfg-section-model">model <ModelField id="cfg-model" disabled={disabled} value={form.model} onChange={(v) => setForm({ model: v })} models={modelFieldModels} native={providerIsNative} onSetupProvider={selectProviders} hint={modelCostHint(form.model)} /></label>
              <label>max run steps <input id="cfg-turns" disabled={disabled} type="number" min="1" max="200" value={form.turns} onChange={(e) => setForm({ turns: e.target.value })} /><span className="cfg-field-hint">hard ceiling for one activation's model/tool loop</span></label>
              <label>autonomy <select id="cfg-autonomy" disabled={disabled} value={form.autonomy} onChange={(e) => setForm({ autonomy: e.target.value })}>{['off', 'manual', 'assisted', 'autonomous'].map((v) => <option key={v} value={v}>{v}</option>)}</select></label>
              <label>working directory <WorkdirInput id="cfg-workdir" disabled={disabled} placeholder="(lanius root)" value={form.workdir} onChange={(v) => setForm({ workdir: v })} /></label>
            </div>
            <div className="setup-row"><button id="cfg-save" disabled={disabled} onClick={saveConfigure}>save</button><span id="cfg-note" className="dim-note">{cfgNote}</span></div>
            <p className="dim-note">renaming changes where future messages go; old messages and history stay under the old name.</p>
          </section>

          <section className="setup-block" id="cfg-section-packages">
            <h3><span className="section-glyph" aria-hidden="true">＋</span>add-ons for this agent</h3>
            <input id="cfg-include" type="hidden" value={form.include} readOnly />
            <input id="cfg-exclude" type="hidden" value={form.exclude} readOnly />
            <div className="setup-row cfg-package-toolbar"><IconButton id="cfg-kit-add-toggle" label="add add-ons" className="cfg-icon-btn" disabled={disabled} onClick={openKitModal}>＋</IconButton><span className="dim-note">copy or link add-ons to change what {agentName} can use</span></div>
            <div id="cfg-package-configs" className="cfg-tree">
              <PackageTree packages={cfgPackages.filter((p: any) => skillIncluded(p))} skillExcluded={skillExcluded} setSkillExcluded={setSkillExcluded} setKitPackagesExcluded={setKitPackagesExcluded} cfgConfigPackages={cfgConfigPackages} cfgSharedConfigRows={cfgSharedConfigRows} setCfgSharedConfigRows={setCfgSharedConfigRows} cfgProfile={cfgProfile} cfgParsed={cfgParsed} setCfgContextVarEdits={setCfgContextVarEdits} />
            </div>
          </section>

          <details className="setup-block cfg-advanced" id="cfg-section-advanced">
            <summary><h3><span className="section-glyph" aria-hidden="true">⚙</span>advanced</h3><span className="dim-note">context program, provider plumbing, sandbox prefixes, package paths, throttles, and raw settings</span></summary>
            <section id="cfg-section-provider">
              <h4><span className="section-glyph" aria-hidden="true">⛁</span>provider</h4>
              <div className="cfg-grid">
                <label>named provider
                  <select id="cfg-provider" disabled={disabled} value={form.provider} onChange={(e) => setForm({ provider: e.target.value })}>
                    <option value="">(none — inline / default below)</option>
                    {providers.map((p: any) => <option key={p.name} value={p.name}>{p.name} ({p.kind === 'native_login' ? 'native login' : p.wire || 'api key'})</option>)}
                  </select>
                  <span className="cfg-field-hint">a named provider (encrypted vault) wins over the inline fields. <button type="button" className="cfg-link" onClick={selectProviders}>manage providers →</button></span>
                </label>
              </div>
              <p className="dim-note">The fields below are the deprecated inline override, kept for back-compat; a named provider above supersedes them.</p>
              <div className="cfg-grid">
                <label>base URL <input id="cfg-base-url" disabled={disabled || !!form.provider} spellCheck={false} placeholder="provider default" value={form.baseUrl} onChange={(e) => setForm({ baseUrl: e.target.value })} /></label>
                <label>API key env <input id="cfg-api-key-env" disabled={disabled || !!form.provider} spellCheck={false} placeholder="adapter default" value={form.apiKeyEnv} onChange={(e) => setForm({ apiKeyEnv: e.target.value })} /></label>
              </div>
              <div className="setup-row">
                <button id="cfg-provider-save" type="button" disabled={disabled} onClick={saveConfigure}>save provider</button>
                <span id="cfg-provider-note" className="dim-note">{cfgNote}</span>
              </div>
            </section>

            <section id="cfg-section-paths">
              <h4>package path</h4>
              <div className="cfg-grid">
                <label>owner <input id="cfg-owner" disabled={disabled} spellCheck={false} value={form.owner} onChange={(e) => setForm({ owner: e.target.value })} /></label>
                <label>parent <input id="cfg-parent" disabled={disabled} spellCheck={false} placeholder="default" value={form.parent} onChange={(e) => setForm({ parent: e.target.value })} /></label>
                <label>prepend path <input id="cfg-package-path" disabled={disabled} spellCheck={false} placeholder="kits/dev, /opt/lanius/packages" value={form.packagePath} onChange={(e) => setForm({ packagePath: e.target.value })} /></label>
                <label className="cfg-check"><input id="cfg-path-inherit" disabled={disabled} type="checkbox" checked={form.pathInherit} onChange={(e) => setForm({ pathInherit: e.target.checked })} /> include inherited path</label>
                <label>effective path <input id="cfg-effective-path" disabled={disabled} spellCheck={false} readOnly value={form.effectivePath} /></label>
              </div>
            </section>

          <section className="setup-block" id="cfg-section-context">
            <h3>context program</h3>
            <p className="dim-note">These settings apply to {agentName} only.</p>
            <div className="cfg-grid">
              <label>program <input id="cfg-context-program" disabled={disabled} spellCheck={false} value={form.contextProgram} onChange={(e) => setForm({ contextProgram: e.target.value })} /></label>
              <label>max context ms <input id="cfg-context-max-ms" disabled={disabled} type="number" min="1" value={form.contextMaxMs} onChange={(e) => setForm({ contextMaxMs: e.target.value })} /></label>
            </div>
            <div className="cfg-context-head"><h4>context steps</h4><div className="cfg-context-add"><button id="cfg-context-add" className="ghost" type="button" onClick={() => setContextAssistantOpen(true)}>+ New</button></div></div>
            <div id="cfg-context-chain" className="cfg-context-chain">
              <div className="cfg-context-stage cfg-context-seed" aria-label="built-in seed context">
                <div className="cfg-context-stage-head"><div className="cfg-context-stage-title"><strong>built-in seed</strong><span className="cfg-config-help">always first · identity, system blocks, and the current conversation</span></div><span className="badge">fixed</span></div>
                <div className="cfg-context-narrative">This is the base context every turn starts with before add-on blocks run.</div>
              </div>
              {!contextDefs.length ? <div className="dim-note">no visible add-on context steps</div> : !cfgContextChain.length ? <div className="dim-note">all visible context steps are removed for this agent</div> : cfgContextChain.map((stage: any, index: number) => (
                <ContextStageTile
                  key={`${stage.package}/${stage.name}`}
                  stage={stage}
                  index={index}
                  chainLength={cfgContextChain.length}
                  disabled={disabled}
                  move={moveContextStage}
                  remove={removeContextStage}
                  setChain={setCfgContextChain}
                  cfgParsed={cfgParsed}
                  cfgProfile={cfgProfile}
                  sharedRows={cfgSharedConfigRows.get(stage.package) ?? new Map()}
                  cfgContextVarEdits={cfgContextVarEdits}
                  setCfgContextVarEdits={setCfgContextVarEdits}
                  draggable={!disabled}
                  onDragStart={() => { draggingContextIndex.current = index; }}
                  onDragOver={(e: any) => e.preventDefault()}
                  onDrop={() => { if (draggingContextIndex.current != null) reorderContextStage(draggingContextIndex.current, index); draggingContextIndex.current = null; }}
                  onDragEnd={() => { draggingContextIndex.current = null; }}
                />
              ))}
            </div>
            <p className="dim-note">Stages run top to bottom after the built-in seed. Reorder or remove stages here; raw TOML stores this as the <code>context.stage</code> array.</p>
            <dialog id="cfg-context-assistant-modal" className="cfg-modal cfg-assistant-modal" ref={contextAssistantRef} onClick={(e) => { if (e.target === e.currentTarget) setContextAssistantOpen(false); }}>
              <div className="cfg-modal-head"><div><h3>new context step</h3><p className="dim-note">The assistant can inspect available blocks and save one for this agent.</p></div><button className="cfg-icon-btn" type="button" aria-label="close context assistant" onClick={() => setContextAssistantOpen(false)}>×</button></div>
              {/* Only mount the assistant while the modal is open: the views are
                  rendered-and-[hidden], so an always-mounted assistant would fire
                  its opening publish on every page load. */}
              {contextAssistantOpen && <AgentAssistant
                title="Add a prompt step"
                intro={`Help add one useful prompt step for ${agentName}. Look up the available steps, then save the one that fits.`}
                tools={contextAuthorTools}
                onDone={() => setContextAssistantOpen(false)}
              />}
            </dialog>
          </section>

          <section className="setup-block" id="cfg-section-sandbox">
            <h3><span className="section-glyph" aria-hidden="true">□</span>sandbox</h3>
            <p className="dim-note">These settings apply to {agentName} only. Working directory is in essentials; prefixes are advanced.</p>
            <div className="cfg-grid">
              <label>writable prefixes <input id="cfg-fs-write" disabled={disabled} spellCheck={false} placeholder="comma separated" value={form.fsWrite} onChange={(e) => setForm({ fsWrite: e.target.value })} /></label>
              <label>capture exclude <input id="cfg-capture-exclude" disabled={disabled} spellCheck={false} placeholder="comma separated" value={form.captureExclude} onChange={(e) => setForm({ captureExclude: e.target.value })} /></label>
              <label>network <select id="cfg-network" disabled={disabled} value={form.network} onChange={(e) => setForm({ network: e.target.value })}>
                <option value="open">open</option>
                <option value="loopback">this machine only</option>
                <option value="none">off</option>
              </select></label>
              <label>hidden folders <input id="cfg-fs-read-deny" disabled={disabled} spellCheck={false} placeholder="comma separated — folders this agent may not read" value={form.fsReadDeny} onChange={(e) => setForm({ fsReadDeny: e.target.value })} /></label>
            </div>
            <details className="cfg-sandbox-advanced">
              <summary className="dim-note">advanced — experimental</summary>
              <p id="cfg-fs-read-allow-warning" className="cfg-warn" role="alert">Danger: this is an allow-list — it hides everything not listed. A list that's even slightly too tight will hide the interpreters, libraries, and files this agent needs, and break every task it runs. Leave it empty unless you know exactly what must stay readable.</p>
              <label>what this agent may read <input id="cfg-fs-read-allow" disabled={disabled} spellCheck={false} placeholder="comma separated — leave empty for open reads" value={form.fsReadAllow} onChange={(e) => setForm({ fsReadAllow: e.target.value })} /></label>
            </details>
            {/* M3: per-agent posture, server-computed (profile get → cfgParsed.cage,
                one shared product-word mapping). Reflects the last save, not each
                keystroke. Distinct from the setup screen's install-default cards. */}
            <div id="cfg-cage" className="setup-health-grid" aria-label="this agent's posture">
              <div id="cfg-cage-write" className={`setup-health-card is-${cfgParsed.cage?.available === false ? 'warn' : 'ok'}`}><span>reads/writes</span><strong>{cfgParsed.cage?.write ?? 'checking...'}</strong></div>
              <div id="cfg-cage-read" className={`setup-health-card is-${cfgParsed.cage?.available === false ? 'warn' : 'ok'}`}><span>what this agent may read</span><strong>{cfgParsed.cage?.read ?? 'checking...'}</strong></div>
              <div id="cfg-cage-network" className={`setup-health-card is-${cfgParsed.cage?.available === false ? 'warn' : 'ok'}`}><span>network</span><strong>{cfgParsed.cage?.network ?? 'checking...'}</strong></div>
            </div>
          </section>

          <section className="setup-block" id="cfg-section-throttle">
            <h3><span className="section-glyph" aria-hidden="true">⏱</span>throttle</h3>
            <p className="dim-note">These settings apply to {agentName} only.</p>
            <div id="cfg-throttle" className="cfg-table">
              {form.throttleRows.map((r: any) => <div key={r.id} className="cfg-throttle-row"><input className="cfg-throttle-name" disabled={disabled} placeholder="name" spellCheck={false} value={r.name} onChange={(e) => updateThrottle(r.id, { name: e.target.value })} /><input className="cfg-throttle-max" disabled={disabled} type="number" placeholder="max concurrent" value={r.max} onChange={(e) => updateThrottle(r.id, { max: e.target.value })} /><input className="cfg-throttle-rate" disabled={disabled} type="number" placeholder="rate/min" value={r.rate} onChange={(e) => updateThrottle(r.id, { rate: e.target.value })} /><input className="cfg-throttle-tokens" disabled={disabled} type="number" placeholder="tokens/hour" value={r.tokens} onChange={(e) => updateThrottle(r.id, { tokens: e.target.value })} /><label className="cfg-check"><input className="cfg-throttle-coalesce" disabled={disabled} type="checkbox" checked={r.coalesce} onChange={(e) => updateThrottle(r.id, { coalesce: e.target.checked })} /> coalesce</label></div>)}
            </div>
            <IconButton id="cfg-throttle-add" label="add throttle" className="ghost cfg-icon-btn" disabled={disabled} onClick={() => setForm({ throttleRows: [...form.throttleRows, { id: uid(), name: '', max: '', rate: '', tokens: '', coalesce: false }] })}>＋</IconButton>
          </section>

          <section className="setup-block" id="cfg-section-raw">
            <h3><span className="section-glyph" aria-hidden="true">{'{}'}</span>raw settings</h3>
            <p className="dim-note">These advanced settings apply to {agentName} only.</p>
            <details><summary className="dim-note">advanced context parameters</summary><p className="dim-note">Advanced values for context and templates; prefer add-on settings when available. Saved by the main save button above.</p><div id="cfg-vars" className="cfg-table">{form.varsRows.map((r: any) => <div key={r.id} className="cfg-var-row"><input className="cfg-var-key" disabled={disabled} placeholder="name" spellCheck={false} value={r.key} onChange={(e) => updateVar(r.id, { key: e.target.value })} /><input className="cfg-var-value" disabled={disabled} placeholder="value" spellCheck={false} value={r.value} onChange={(e) => updateVar(r.id, { value: e.target.value })} /></div>)}</div><IconButton id="cfg-var-add" label="add context parameter" className="ghost cfg-icon-btn" disabled={disabled} onClick={() => setForm({ varsRows: [...form.varsRows, { id: uid(), key: '', value: '' }] })}>＋</IconButton></details>
            <details><summary className="dim-note">the raw settings file</summary><textarea id="cfg-toml" disabled={disabled} spellCheck={false} rows={14} value={cfgToml} onChange={(e) => setCfgToml(e.target.value)} /><div className="setup-row"><button id="cfg-toml-save" disabled={disabled} onClick={saveRawToml}>save raw file</button><span id="cfg-toml-note" className="dim-note">{cfgTomlNote}</span></div></details>
          </section>
          </details>
        </div>
      </div>
    </div>
  );
}

function ContextStageTile({ stage, index, chainLength, disabled, move, remove, setChain, cfgParsed, cfgProfile, sharedRows, cfgContextVarEdits, setCfgContextVarEdits, draggable, onDragStart, onDragOver, onDrop, onDragEnd }: any) {
  const key = `${stage.package}/${stage.name}`;
  const params = (stage.config ?? []).filter((p: any) => p.key).map((p: any) => ({ key: p.key, type: p.type ?? 'string', label: p.label || p.key, help: p.help || '', default: p.default, options: p.options ?? [], agent_tunable: p.agent_tunable === true, agentScoped: true, source: `agent context ${stage.name}` }));
  const updateStage = (patch: any) => setChain((prev: any[]) => prev.map((s) => `${s.package}/${s.name}` === key ? { ...s, ...patch } : s));
  const modeText = stage.mode === 'resident' ? 'resident block stays warm and injects live context' : 'exec block runs while assembling this turn';
  const enabledText = stage.enabled === false ? 'disabled' : 'enabled';
  const injects = stage.description || stage.injects || stage.summary || (params.length ? `injects context using ${params.length} configurable setting${params.length === 1 ? '' : 's'}` : 'injects package-provided context');
  return (
    <div className="cfg-context-stage" data-stage={key} draggable={draggable} onDragStart={onDragStart} onDragOver={onDragOver} onDrop={onDrop} onDragEnd={onDragEnd}>
      <div className="cfg-context-stage-head"><div className="cfg-context-stage-title"><strong>{stage.name}</strong><span className="cfg-config-help">{stage.package} · {enabledText} · order {stage.order}</span></div><div className="cfg-context-stage-actions"><IconButton label={`move ${key} up`} disabled={disabled || index === 0} onClick={() => move(index, -1)}>↑</IconButton><IconButton label={`move ${key} down`} disabled={disabled || index === chainLength - 1} onClick={() => move(index, 1)}>↓</IconButton><IconButton label={`remove ${key}`} disabled={disabled} onClick={() => remove(index)}>×</IconButton></div></div>
      <div className="cfg-context-narrative">{injects}</div>
      <div className="cfg-context-stage-meta"><span className="badge">{stage.mode || 'exec'}</span><span>{modeText}</span><span>{params.length} setting{params.length === 1 ? '' : 's'}</span></div>
      <div className="cfg-context-stage-grid"><label>timeout ms<input type="number" min="1" data-context-field="timeout_ms" disabled={disabled} value={stage.timeout_ms} onChange={(e) => updateStage({ timeout_ms: Number(e.target.value || stage.timeout_ms) })} /></label></div>
      {!!params.length && <div className="cfg-context-stage-config"><div className="cfg-context-stage-subhead">settings for {cfgParsed.agent || cfgProfile || 'this agent'} only</div>{params.map((param: any) => {
        const value = cfgContextVarEdits.get(param.key) ?? cfgParsed.vars?.[param.key] ?? tomlDisplayValue(param.default, param.type);
        const setValue = (v: string) => setCfgContextVarEdits((old: Map<string, string>) => new Map(old).set(param.key, v));
        const agentName = cfgParsed.agent || cfgProfile || 'this agent';
        const pendingVars = Object.fromEntries(cfgContextVarEdits);
        const effective = effectiveConfigValue(param, sharedRows ?? new Map(), { ...(cfgParsed.vars ?? {}), ...pendingVars }, agentName);
        return <ConfigInputRow key={param.key} param={{ ...param, scope: `applies to ${agentName} only`, effective }} value={value} setValue={setValue} contextVar={param.key} contextStage={key} />;
      })}</div>}
    </div>
  );
}

function ConfigInputRow({ param, value, setValue, contextVar, contextStage, save, secondarySave }: any) {
  const scope = param.scope || '';
  const effective = param.effective;
  return (
    <div className="cfg-config-row">
      <label title={param.key}>{param.label || param.key}<span className="cfg-config-help">{[param.source, param.type ? `type: ${param.type}` : '', scope, param.help].filter(Boolean).join(' · ')}</span></label>
      {param.type === 'boolean' ? <label className="cfg-check" data-value-input="1"><input type="checkbox" checked={/^(true|1)$/i.test(String(value))} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.checked ? 'true' : 'false')} /> enabled</label>
        : param.type === 'enum' && (param.options ?? []).length ? <select value={value || param.options[0]} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.value)}>{param.options.map((o: string) => <option key={o} value={o}>{o}</option>)}</select>
          : <input type={param.type === 'number' ? 'number' : 'text'} spellCheck={false} value={value ?? ''} placeholder={param.default == null ? 'value' : `default ${tomlDisplayValue(param.default, param.type)}`} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.value)} />}
      <div className="cfg-config-actions">{save ?? <span />}{secondarySave ?? null}</div>
      <span className="cfg-effective">{effective ? `effective here: ${effective.value || '(empty)'} · ${effective.label}` : ''}</span>
    </div>
  );
}

function PackageTree({ packages, skillExcluded, setSkillExcluded, setKitPackagesExcluded, cfgConfigPackages, cfgSharedConfigRows, setCfgSharedConfigRows, cfgProfile, cfgParsed, setCfgContextVarEdits }: any) {
  if (!packages.length) return <div className="dim-note">no packages found</div>;
  const groups = new Map();
  for (const p of packages) {
    const kit = kitNameFor(p);
    if (!groups.has(kit)) groups.set(kit, []);
    groups.get(kit).push(p);
  }
  return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([kit, pkgs]: any) => {
    const disabledCount = pkgs.filter((p: any) => skillExcluded(p)).length;
    return (
      <details key={kit} className="cfg-package-group" data-kit={kit} open>
        <summary className="cfg-kit-summary cfg-kit-head"><span className="cfg-disclosure">▸</span><span className="cfg-kit-name">{kit}</span><span className="cfg-pkg-desc">{pkgs.length} package{pkgs.length === 1 ? '' : 's'}</span><IconButton label={disabledCount === pkgs.length ? `enable all ${kit} packages` : `disable all ${kit} packages`} className={disabledCount === pkgs.length ? 'ghost cfg-icon-btn cfg-kit-toggle' : 'cfg-icon-btn cfg-kit-toggle'} onClick={(e) => { e.preventDefault(); setKitPackagesExcluded(pkgs, disabledCount !== pkgs.length); }}>{disabledCount === pkgs.length ? '✓' : '⊘'}</IconButton></summary>
        <div className="cfg-package-table">{[...pkgs].sort((a, b) => a.name.localeCompare(b.name)).map((p) => {
          const disabled = skillExcluded(p);
          return <PackageCard key={`${cfgProfile || cfgParsed.agent || 'agent'}-${p.name}-${disabled ? 'disabled' : 'enabled'}`} pkg={p} disabled={disabled} canConfigure={declaredConfigParams(p).length > 0 || cfgConfigPackages.has(p.name)} toggle={() => setSkillExcluded(p.name, !disabled)} sharedRows={cfgSharedConfigRows.get(p.name) ?? new Map()} setCfgSharedConfigRows={setCfgSharedConfigRows} setCfgContextVarEdits={setCfgContextVarEdits} cfgProfile={cfgProfile} cfgParsed={cfgParsed} />;
        })}</div>
      </details>
    );
  });
}

function PackageCard({ pkg, disabled, canConfigure, toggle, sharedRows, setCfgSharedConfigRows, setCfgContextVarEdits, cfgProfile, cfgParsed }: any) {
  const [panelOpen, setPanelOpen] = useState(false);
  const [rows, setRows] = useState<any[] | null>(null);
  const source = packageSource(pkg);
  const declared = declaredConfigParams(pkg);
  useEffect(() => {
    setPanelOpen(false);
    setRows(null);
  }, [cfgProfile, cfgParsed.agent, pkg.name]);
  const load = async () => {
    setPanelOpen((v) => !v);
    if (rows) return;
    const r = await adminGet(`configs?package=${encodeURIComponent(pkg.name)}`);
    const raw = r.ok ? (r.config?.toml || '') : '';
    const current = parseConfigRows(raw);
    const byKey: Map<string, string> = current.length
      ? new Map(current.map((row) => [row.key, row.value] as [string, string]))
      : new Map<string, string>(sharedRows ?? []);
    const params = new Map(declared.map((param: any) => [param.key, param]));
    for (const param of declared) if (!byKey.has(param.key)) byKey.set(param.key, '');
    for (const key of byKey.keys()) if (!params.has(key)) params.set(key, { key, type: 'string', label: key, help: 'Existing package setting not declared in the manifest.', source: 'current settings' });
    setRows([...params.values()].sort((a: any, b: any) => a.key.localeCompare(b.key)).map((param: any) => {
      const agentName = cfgParsed.agent || cfgProfile || 'this agent';
      return {
        param: {
          ...param,
          scope: 'shared default for every agent',
          effective: effectiveConfigValue(param, byKey, cfgParsed.vars ?? {}, agentName),
        },
        value: byKey.get(param.key) || tomlDisplayValue(param.default, param.type),
        note: '',
        agentNote: '',
      };
    }));
  };
  const saveRow = async (idx: number) => {
    const row = rows![idx];
    setRows(rows!.map((r, i) => i === idx ? { ...r, note: 'saving...' } : r));
    const write = await adminPost('configs/set', { package: pkg.name, key: row.param.key, value: row.value });
    const agentName = cfgParsed.agent || cfgProfile || 'this agent';
    const nextShared = new Map<string, string>(sharedRows ?? []);
    nextShared.set(row.param.key, row.value);
    if (write.ok) setCfgSharedConfigRows((old: Map<string, Map<string, string>>) => {
      const next = new Map(old);
      next.set(pkg.name, nextShared);
      return next;
    });
    setRows((old) => old!.map((r, i) => i === idx ? {
      ...r,
      note: write.ok ? 'saved for every agent' : (write.error ?? 'save failed'),
      param: {
        ...r.param,
        effective: write.ok ? effectiveConfigValue(row.param, nextShared, row.param.effective?.source === 'agent' ? { [row.param.key]: row.param.effective.value } : (cfgParsed.vars ?? {}), agentName) : r.param.effective,
      },
    } : r));
  };
  const saveAgentRow = async (idx: number) => {
    if (!cfgProfile) return;
    const row = rows![idx];
    const agentName = cfgParsed.agent || cfgProfile;
    setRows(rows!.map((r, i) => i === idx ? { ...r, agentNote: 'saving...' } : r));
    const write = await adminPost('agents/set', { name: cfgProfile, set: { [`vars.${row.param.key}`]: row.value } });
    if (write.ok) setCfgContextVarEdits((old: Map<string, string>) => new Map(old).set(row.param.key, row.value));
    setRows((old) => old!.map((r, i) => i === idx ? {
      ...r,
      agentNote: write.ok ? `saved for ${agentName}` : (write.error ?? 'save failed'),
      param: {
        ...r.param,
        effective: write.ok ? { value: row.value, source: 'agent', label: valueSourceLabel('agent', agentName) } : r.param.effective,
      },
    } : r));
  };
  return (
    <details className={`cfg-package-card${disabled ? ' is-disabled' : ''}`} data-package={pkg.name}>
      <summary className="cfg-package-head"><span className="cfg-disclosure">▸</span><span className={`cfg-source-icon source-${source.kind}`} title={`${source.kind}: ${source.label}`}>{source.icon}</span><span className="cfg-package-title"><span className="setup-kit-name">{pkg.name}</span><span className="cfg-pkg-desc">{packageDescription(pkg)}</span>{canConfigure && <span className="cfg-pkg-desc">{packageHasAgentScopedSettings(pkg) ? `settings can be saved for every agent or for ${cfgParsed.agent || cfgProfile || 'this agent'} only` : 'settings save for every agent'}</span>}</span></summary>
      <div className="cfg-package-body"><div className="cfg-package-detail">{actorDetail(pkg)}</div><div className="cfg-package-meta">{packageBadges(pkg).map((b) => <span key={b.text} className={b.cls}>{b.text}</span>)}{riskBadges(pkg).map((b) => <span key={`risk-${b}`} className="badge badge-wait">{b}</span>)}</div><div className="cfg-package-controls"><span className="dim-note">{disabled ? 'disabled for this agent' : 'enabled for this agent'} · {grantState(pkg)}</span><button className={disabled ? 'ghost cfg-package-disable' : 'cfg-package-disable'} title={disabled ? `remove ${pkg.name} from skills.exclude` : `add ${pkg.name} to skills.exclude`} onClick={(e) => { e.preventDefault(); toggle(); }}>{disabled ? 'enable' : 'disable'}</button><button className="ghost cfg-package-config-toggle" hidden={!canConfigure} onClick={(e) => { e.preventDefault(); load(); }}>settings</button></div>
        <div className="cfg-package-config-panel" hidden={!panelOpen}>{rows === null ? 'loading...' : !rows.length ? <div className="dim-note">no configurable settings declared</div> : rows.map((row, idx) => <ConfigInputRow key={row.param.key} param={row.param} value={row.value} setValue={(v: string) => setRows(rows.map((r, i) => i === idx ? { ...r, value: v } : r))} save={<><IconButton label={`save ${pkg.name}.${row.param.key} for every agent — affects every agent using this add-on`} className="cfg-icon-btn cfg-shared-save" onClick={() => saveRow(idx)}>⚑ every agent</IconButton><span className="dim-note">{row.note}</span></>} secondarySave={row.param.agentScoped ? <><IconButton label={`save ${pkg.name}.${row.param.key} for ${cfgParsed.agent || cfgProfile || 'this agent'} only`} className="cfg-icon-btn cfg-agent-save" onClick={() => saveAgentRow(idx)}>this agent</IconButton><span className="dim-note">{row.agentNote}</span></> : null} />)}</div>
      </div>
    </details>
  );
}

function KitModal({ modalRef, open, close, kits, cfgForm, cfgPackages, cfgKitDetails, loadKitDetail, installKitForAgent }: any) {
  return (
    <dialog id="cfg-kit-add-modal" className="cfg-modal" ref={modalRef} onClick={(e) => { if (e.target === e.currentTarget) close(); }}>
      <div className="cfg-modal-head"><div><h3>add capability</h3><p className="dim-note">A capability adds a ready-made bundle of abilities to this agent.</p></div><button id="cfg-kit-add-close" className="cfg-icon-btn" type="button" aria-label="close add capability" onClick={close}>×</button></div>
      <div id="cfg-kit-add-list" className="cfg-tree cfg-kit-add-list">{!kits.length ? <div className="dim-note">no kits found</div> : kits.map((k: any) => <KitAddRow key={k.name} kit={k} cfgForm={cfgForm} cfgPackages={cfgPackages} detail={cfgKitDetails.get(k.name)} loadKitDetail={loadKitDetail} installKitForAgent={installKitForAgent} />)}</div>
    </dialog>
  );
}

function KitAddRow({ kit, cfgForm, cfgPackages, detail, loadKitDetail, installKitForAgent }: any) {
  const [note, setNote] = useState('');
  const [menu, setMenu] = useState(false);
  const packageDir = `${String(kit.dir ?? '').replace(/[\\/]$/, '')}${kit.dir ? '/' : ''}packages`;
  const installed = (kit.dir && arr(cfgForm.effectivePath).some((p) => p === kit.dir || p === packageDir)) || ((detail?.packages ?? []).length > 0 && cfgPackages.some((p: any) => new Set((detail?.packages ?? []).map((x: any) => x.name)).has(p.name)));
  return (
    <details className="cfg-add-kit" onToggle={(e) => e.currentTarget.open && !detail && void loadKitDetail(kit)}>
      <summary className="cfg-kit-summary cfg-kit-head"><span>{kit.name}</span><span className="cfg-pkg-desc">{kit.hook || ''}</span><span className="cfg-kit-actions"><span className="cfg-split-action" hidden={installed}><button className="cfg-split-primary cfg-kit-add-btn" type="button" aria-label={`link ${kit.name}`} title={`link ${kit.name}`} onClick={(e) => { e.preventDefault(); installKitForAgent(kit, 'link', setNote); }}>link</button><button className="cfg-split-caret" type="button" aria-label={`more add actions for ${kit.name}`} title={`more add actions for ${kit.name}`} onClick={(e) => { e.preventDefault(); setMenu(!menu); }}>⌄</button><div className="cfg-action-menu" hidden={!menu}>{['link', 'copy'].map((action) => <button key={action} type="button" aria-label={`${action} ${kit.name}`} onClick={(e) => { e.preventDefault(); setMenu(false); installKitForAgent(kit, action, setNote); }}>{action}</button>)}</div></span><span className="badge" hidden={!installed}>installed</span><button className="cfg-icon-btn cfg-kit-gear" type="button" aria-label={`configure ${kit.name}`} hidden={!installed}>⚙</button></span></summary>
      <div className="cfg-skill-table">{!detail ? <div className="dim-note">expand to load abilities</div> : !(detail.packages ?? []).length ? <div className="dim-note">{detail.error ?? 'no abilities in this capability'}</div> : detail.packages.map((p: any) => <div key={p.name} className="cfg-skill-row cfg-kit-preview-row"><span className="cfg-pkg-name">{p.name}<span className="cfg-preview-badges">{p.skill && <span className="badge">skill</span>}{p.manifest?.actor && <span className="badge badge-wait">{p.manifest.actor}</span>}</span></span><span className="cfg-pkg-desc">{actorDetail(p.manifest?.process || !p.manifest?.mode ? p : { ...p, manifest: { ...p.manifest, process: { mode: p.manifest.mode, run: p.manifest.run, http: p.manifest.http } } })}</span><span className="cfg-skill-actions" /></div>)}</div>
      <div className="dim-note">{note}</div>
    </details>
  );
}

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

function SessionsView({ hidden, state, agent, openTranscript, loadSessions }: any) {
  return (
    <div id="view-sessions" className="view" hidden={hidden}>
      <div id="sessions-pane" className="sessions-pane">
        {state.status === 'loading' && <div className="dim-note">asking the history view…</div>}
        {state.status === 'error' && <div className="dim-note"><div>transcripts unavailable — live view only.</div>{state.error && <div className="dim-sub">{state.error}</div>}</div>}
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
