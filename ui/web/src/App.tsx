import { useEffect, useMemo, useRef, useState } from 'react';
import { adminGet, adminPost, adminPut, history, publish } from './api';
import { openLiveStream } from './live';
import { Button, IconButton } from './components/primitives';

// Product setup language is guided by docs/journeys/README.md and
// docs/layering.md. Durable browser-flow expectations live in
// docs/ui-flows/README.md and docs/ui-flows/configuration.md.
const BUFFER_CAP = 2000;
const PARENT_PATH = '$parent';

const arr = (v: unknown) => String(v ?? '').split(',').map((x) => x.trim()).filter(Boolean);
const csv = (values: unknown) => Array.isArray(values) ? values.join(', ') : '';
const shortTs = (t: unknown) => (typeof t === 'string' ? t.replace('T', ' ').slice(0, 19) : '');
const timeOf = (env: any) => {
  const d = new Date(env?.ts ?? Date.now());
  return isNaN(d.getTime()) ? '--:--:--' : d.toTimeString().slice(0, 8);
};
const summarize = (p: unknown, max = 110) => {
  if (p == null) return '';
  const s = typeof p === 'string' ? p : JSON.stringify(p);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
};
const agentOf = (topic: string) => topic.match(/^(?:in|obs)\/agent\/([^/]+)/)?.[1] ?? null;
const uid = () => Math.random().toString(36).slice(2);

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
  if ((manifest.stages ?? []).length) return 'context stage package';
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
  if (process?.http) bits.push('serves an approved local HTTP endpoint');
  if (manifest.hooks) bits.push(`declares ${manifest.hooks} hook${manifest.hooks === 1 ? '' : 's'}`);
  if ((manifest.stages ?? []).length) bits.push(`contributes ${manifest.stages.length} context stage${manifest.stages.length === 1 ? '' : 's'}`);
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
  if (manifest.process?.mode) badges.push({ cls: 'badge badge-wait', text: manifest.process.mode === 'daemon' ? 'actor' : manifest.process.mode });
  if (manifest.process?.http) badges.push({ cls: 'badge badge-wait', text: 'http' });
  if (manifest.hooks) badges.push({ cls: 'badge badge-wait', text: 'hook' });
  if (manifest.cron) badges.push({ cls: 'badge badge-wait', text: 'cron' });
  if (manifest.providers) badges.push({ cls: 'badge badge-wait', text: 'provider' });
  if ((manifest.stages ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'stage' });
  if ((manifest.mcp ?? []).length) badges.push({ cls: 'badge badge-wait', text: 'mcp' });
  return badges;
}

function declaredConfigParams(pkg: any) {
  const byKey = new Map();
  for (const key of pkg.manifest?.config?.agent_tunable ?? []) {
    if (!key) continue;
    byKey.set(key, { key, type: 'string', label: key, help: 'Package setting declared agent-tunable by the package manifest.', agent_tunable: true, source: 'package' });
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
        source: `context stage ${stage.name}`,
      });
    }
  }
  return [...byKey.values()].sort((a, b) => a.key.localeCompare(b.key));
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
  baseUrl: '',
  apiKeyEnv: '',
  contextProgram: 'default',
  contextMaxMs: '30000',
  workdir: '',
  fsWrite: '',
  captureExclude: '',
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
  const [agents, setAgents] = useState(new Map());
  const [diskProfiles, setDiskProfiles] = useState<any[]>([]);
  const [buffer, setBuffer] = useState<any[]>([]);
  const [filter, setFilter] = useState('signals');
  const [paused, setPaused] = useState(false);
  const [conv, setConv] = useState(new Map());
  const [sessionsState, setSessionsState] = useState<any>({ status: 'idle', sessions: [], transcript: null, error: '' });
  const [modelOptions, setModelOptions] = useState<any[]>([]);
  const [modelsHint, setModelsHint] = useState('');

  const [setup, setSetup] = useState<any>({ status: '', statusKind: '', kits: null, packages: null, proposals: null, loading: false });
  const [newAgent, setNewAgent] = useState({ name: '', model: '' });
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
  const [cfgKitDetails, setCfgKitDetails] = useState(new Map());
  const [cfgContextChain, setCfgContextChain] = useState<any[]>([]);
  const [cfgContextRemoved, setCfgContextRemoved] = useState(new Set());
  const [cfgContextVarEdits, setCfgContextVarEdits] = useState(new Map());
  const [kitModalOpen, setKitModalOpen] = useState(false);

  const refs = useRef<any>({});
  refs.current = { sel, agents, diskProfiles, defaultAgent, historyOk, filter, paused, cfgForm, cfgPackages, cfgContextChain };
  const corrAgent = useRef(new Map());
  const sentCorrs = useRef(new Set());
  const seenAsks = useRef(new Set());
  const seenFailures = useRef(new Set());
  const agentSessions = useRef(new Map());
  const postConfigureNote = useRef('');
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

  const setHistoryState = (v: boolean) => setHistoryOk((prev) => prev === v ? prev : v);

  const refreshAgents = async () => {
    const j = await history({ kind: 'agents' });
    if (j?.unavailable) setHistoryState(false);
    if (!j?.ok) return;
    setHistoryState(true);
    for (const a of j.agents ?? []) touchAgent(a.agent, { sessions: a.sessions });
  };

  useEffect(() => {
    void loadDiskAgents();
    void refreshAgents();
    const iv = setInterval(refreshAgents, 15000);
    return () => clearInterval(iv);
  }, []);

  useEffect(() => {
    const es = openLiveStream((m: any) => {
      if (m.kind === 'status') {
        if (m.agent) {
          setDefaultAgent(m.agent);
          touchAgent(m.agent);
        }
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
  const selectSetup = (status?: any) => {
    setSel({ kind: 'setup' });
    void loadSetup(status);
  };
  const selectAgent = (agent: string, tab?: string) => {
    setSel((prev: any) => ({ kind: 'agent', agent, tab: tab ?? (prev.kind === 'agent' && prev.agent === agent ? prev.tab : 'converse') }));
  };

  useEffect(() => {
    if (sel.kind === 'setup' && !setup.loading && !setup.kits) void loadSetup();
    if (sel.kind === 'agent' && sel.tab === 'configure') void loadConfigure(sel.agent);
    if (sel.kind === 'agent' && sel.tab === 'sessions') void loadSessions(sel.agent);
    if (sel.kind === 'agent' && sel.tab === 'telemetry') setFilter('all');
  }, [sel.kind, sel.agent, sel.tab]);

  const stageTitle = sel.kind === 'welcome' ? 'welcome'
    : sel.kind === 'signals' ? 'signals'
      : sel.kind === 'setup' ? 'add-ons'
        : sel.agent;
  const stageNote = sel.kind === 'welcome' ? 'orient, then dive in'
    : sel.kind === 'signals' ? 'a live view of everything happening — orange means something needs your attention'
      : sel.kind === 'setup' ? 'add useful behavior and adjust its settings'
        : sel.tab === 'converse' ? `in/agent/${sel.agent} ⇄ in/human — the mailbox view`
          : sel.tab === 'sessions' ? 'your agent’s past conversations'
            : sel.tab === 'configure' ? 'who this agent is — model, mailbox, visibility'
              : `obs/agent/${sel.agent}/# — this agent's telemetry`;

  const loadSetup = async (opts: any = {}) => {
    setSetup((s: any) => ({ ...s, loading: true, status: opts.status ?? s.status, statusKind: opts.statusKind ?? s.statusKind }));
    const [kits, packages, proposals] = await Promise.all([adminGet('kits'), adminGet('packages'), adminGet('proposals')]);
    await loadDiskAgents();
    setSetup({ loading: false, status: opts.status ?? '', statusKind: opts.statusKind ?? '', kits, packages, proposals });
  };

  const createAgent = async () => {
    const name = newAgent.name.trim();
    const model = newAgent.model.trim();
    if (!name) { setNewAgentNote('name it first'); return; }
    setNewAgentNote('creating…');
    const r = await adminPost('agents', { name, ...(model ? { model } : {}) });
    if (!r.ok) { setNewAgentNote(r.error ?? 'failed'); return; }
    setNewAgent({ name: '', model: '' });
    setNewAgentNote('');
    await loadDiskAgents();
    postConfigureNote.current = `created ${name} — set its identity below, then converse`;
    selectAgent(name, 'configure');
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
      baseUrl: d.base_url ?? '',
      apiKeyEnv: d.api_key_env ?? '',
      contextProgram: d.context?.program ?? 'default',
      contextMaxMs: d.context?.max_total_ms ?? 30000,
      workdir: d.workdir ?? '',
      fsWrite: csv(d.fs_write ?? []),
      captureExclude: csv(d.capture_exclude ?? []),
      include: csv(d.skills?.include ?? ['#']),
      exclude: csv(d.skills?.exclude ?? []),
      varsRows: Object.entries(d.vars ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => ({ id: uid(), key, value })) || [],
      throttleRows: Object.entries(d.throttle ?? {}).sort(([a], [b]) => a.localeCompare(b)).map(([name, t]: any) => ({
        id: uid(), name, max: t?.max_concurrent ?? '', rate: t?.rate_per_min ?? '', tokens: t?.llm_tokens_per_hour ?? '', coalesce: t?.coalesce === true,
      })),
    };
    if (!nextForm.varsRows.length) nextForm.varsRows = [{ id: uid(), key: '', value: '' }];
    if (!nextForm.throttleRows.length) nextForm.throttleRows = [{ id: uid(), name: '', max: '', rate: '', tokens: '', coalesce: false }];
    setCfgPackages(packages);
    setCfgKits(kits.ok === false ? [] : (kits.kits ?? []));
    setCfgConfigPackages(new Set((configs.ok === false ? [] : (configs.configs ?? [])).map((c: any) => c.package).filter(Boolean)));
    setCfgToml(r.ok ? r.toml : '');
    await loadDiskAgents();
    setCfgParsed(d);
    setCfgForm(nextForm);
    resetContextChain(d.context ?? {}, packages, nextForm);
    setCfgLoading(false);
    const after = postConfigureNote.current;
    postConfigureNote.current = '';
    setCfgNote(after || (r.ok ? '' : `no settings file for ${profile} — this agent only exists as traffic; create an agent here to configure it`));
  };

  const saveConfigure = async () => {
    if (!cfgProfile) return;
    setCfgNote('saving…');
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
    if (cfgForm.baseUrl.trim()) set['model.base_url'] = cfgForm.baseUrl.trim();
    if (cfgForm.apiKeyEnv.trim()) set['model.api_key_env'] = cfgForm.apiKeyEnv.trim();
    if (cfgForm.contextProgram.trim()) set['context.program'] = cfgForm.contextProgram.trim();
    if (cfgForm.contextMaxMs) set['context.max_total_ms'] = Number(cfgForm.contextMaxMs);
    const chain = new Map(cfgContextChain.map((s) => [contextStageKey(s), s]));
    const stageRows: any[] = cfgContextChain.map((s) => ({
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
  };

  const saveRawToml = async () => {
    if (!cfgProfile) return;
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
      if (p.failed) addFailure(agent, env);
      else if (p.question != null) addAsk(agent, env);
      else if (typeof p.text === 'string') addConv(agent, { who: 'agent', cls: 'agent', text: p.text, corr });
      return;
    }
    if (noun && topic.startsWith('in/agent/')) {
      if (env.correlation_id) corrAgent.current.set(env.correlation_id, noun);
      if (typeof p.prompt === 'string') {
        if (!sentCorrs.current.has(env.correlation_id)) addConv(noun, { who: 'you', cls: 'you', text: p.prompt, corr: env.correlation_id });
      } else if (p.answer != null) {
        closeAskFromOutside(env.correlation_id, p.answer);
      }
    }
  };

  const addConv = (agent: string, message: any) => {
    setConv((prev) => {
      const next = new Map(prev);
      next.set(agent, [...(next.get(agent) ?? []), { id: uid(), type: 'msg', ...message }]);
      return next;
    });
  };
  const addFailure = (agent: string, env: any) => {
    const corr = env.correlation_id;
    if (corr && seenFailures.current.has(corr)) return;
    if (corr) seenFailures.current.add(corr);
    addConv(agent, { who: 'agent failed', cls: 'failed', text: env.payload?.error || 'the agent failed with no detail.', corr, failed: true });
  };
  const addAsk = (agent: string, env: any) => {
    const corr = env.correlation_id;
    if (corr && seenAsks.current.has(corr)) return;
    if (corr) seenAsks.current.add(corr);
    setConv((prev) => {
      const next = new Map(prev);
      next.set(agent, [...(next.get(agent) ?? []), { id: uid(), type: 'ask', corr, payload: env.payload ?? {}, answered: null }]);
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
    const session = agentSessions.current.get(agent) ?? `web-${agent}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    agentSessions.current.set(agent, session);
    sentCorrs.current.add(corr);
    corrAgent.current.set(corr, agent);
    addConv(agent, { who: 'you', cls: 'you', text, corr });
    input.value = '';
    const btn = e.currentTarget.querySelector('#compose-send') as HTMLButtonElement;
    const ok = await publish(`in/agent/${agent}`, { prompt: text, session }, corr);
    btn.textContent = ok ? 'accepted ✓' : 'failed ✕';
    btn.classList.toggle('sent', ok);
    setTimeout(() => { btn.textContent = 'transmit'; btn.classList.remove('sent'); }, 1400);
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

  const openTranscript = async (agent: string, session: string, beforeId?: number, prepend = false) => {
    if (!prepend) setSessionsState({ status: 'transcript-loading', sessions: [], transcript: { session, messages: [], has_more: false }, error: '' });
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
          <span className="kite" aria-hidden="true">⟁</span>
          <h1><em>elanus</em></h1>
          <span className="mast-sub">agent explorer // live</span>
        </button>
        <div className="mast-right">
          <button id="signal-lamp" className={`lamp${signal.lit ? ' lit' : ''}`} title="urgent alerts — click to acknowledge" onClick={() => setSignal({ lit: false, label: 'signal' })}>
            <span className="lamp-dot" /><span id="signal-label">{signal.label}</span>
          </button>
          <span id="conn" className={`conn ${conn.connected ? 'conn-up' : 'conn-down'}`}><span className="conn-dot" /><span id="conn-text">{conn.text}</span></span>
        </div>
      </header>

      <main className="deck">
        <Nav agents={agents} sel={sel} historyOk={historyOk} selectAgent={selectAgent} selectSignals={selectSignals} selectSetup={selectSetup} />

        <section className="stage panel" aria-label="view">
          <div className="panel-head">
            <h2 id="stage-title">{stageTitle}</h2>
            <div id="agent-tabs" className="tabs" role="tablist" hidden={sel.kind !== 'agent'}>
              {['converse', 'sessions', 'telemetry', 'configure'].map((tab) => (
                <button key={tab} data-tab={tab} className={sel.kind === 'agent' && sel.tab === tab ? 'on' : ''} onClick={() => sel.kind === 'agent' && selectAgent(sel.agent, tab)}>{tab}</button>
              ))}
            </div>
            <span id="stage-note" className="panel-note">{stageNote}</span>
          </div>

          <WelcomeView hidden={sel.kind !== 'welcome'} primary={primaryAgent()} historyOk={historyOk} selectAgent={selectAgent} selectSetup={selectSetup} selectSignals={selectSignals} />
          <ConverseView hidden={!(sel.kind === 'agent' && sel.tab === 'converse')} agent={sel.agent} messages={conv.get(sel.agent) ?? []} submitCompose={submitCompose} answerAsk={answerAsk} />
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
            cfgContextChain={cfgContextChain}
            setCfgContextChain={setCfgContextChain}
            cfgContextVarEdits={cfgContextVarEdits}
            setCfgContextVarEdits={setCfgContextVarEdits}
            contextDefs={contextDefs}
            availableContextStages={availableContextStages}
            moveContextStage={moveContextStage}
            removeContextStage={removeContextStage}
            addContextStage={addContextStage}
            skillIncluded={skillIncluded}
            skillExcluded={skillExcluded}
            setSkillExcluded={setSkillExcluded}
            setKitPackagesExcluded={setKitPackagesExcluded}
            openKitModal={openKitModal}
          />
          <SetupView
            hidden={sel.kind !== 'setup'}
            setup={setup}
            provenance={provenance}
            newAgent={newAgent}
            setNewAgent={setNewAgent}
            newAgentNote={newAgentNote}
            createAgent={createAgent}
            modelsHint={modelsHint}
            loadSetup={loadSetup}
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

      <footer className="strip">
        <span id="stat-count">{count} event{count === 1 ? '' : 's'}</span>
        <span id="stat-broker" />
        <span className="strip-note">same authority as your terminal — because it is your terminal</span>
        <span className="strip-right">a live view of your agents</span>
      </footer>
    </div>
  );
}

function Nav({ agents, sel, historyOk, selectAgent, selectSignals, selectSetup }: any) {
  const items = [...agents.keys()].sort();
  const onKey = (e: any) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const navItems = [...document.querySelectorAll<HTMLElement>('#nav-list .nav-item')];
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const i = active ? navItems.indexOf(active) : -1;
    navItems[(i + (e.key === 'ArrowDown' ? 1 : -1) + navItems.length) % navItems.length]?.focus();
  };
  return (
    <nav className="nav panel" aria-label="explorer">
      <div className="panel-head"><h2>instruments</h2></div>
      <div id="nav-list" className="nav-list" onKeyDown={onKey}>
        <button className={`nav-item nav-signals${sel.kind === 'signals' ? ' on' : ''}`} data-sel="signals" onClick={selectSignals}><span className="nav-sigil">◮</span> signals</button>
        <button className={`nav-item nav-setup${sel.kind === 'setup' ? ' on' : ''}`} data-sel="setup" onClick={() => selectSetup()}><span className="nav-sigil">⚒</span> add-ons</button>
        <div className="nav-label">agents</div>
        <div id="nav-agents">
          {items.map((name) => {
            const a = agents.get(name);
            const sessions = [...(a.sessions ?? [])].sort().reverse();
            return (
              <div key={name}>
                <button className={`nav-item nav-agent${sel.kind === 'agent' && sel.agent === name ? ' on' : ''}`} data-sel={`agent:${name}`} onClick={() => selectAgent(name)}>
                  <span className="nav-sigil">⟁</span> {name}{a.live && <span className="nav-live">·live</span>}
                </button>
                {sessions.slice(0, 12).map((s) => <button key={s} className="nav-item nav-session" onClick={() => selectAgent(name, 'sessions')}>{s}</button>)}
                {sessions.length > 12 && <div className="nav-hint">+{sessions.length - 12} more in sessions</div>}
              </div>
            );
          })}
        </div>
        <div id="nav-empty" className="nav-hint" hidden={agents.size > 0}>no agents yet — create one below</div>
        <button id="nav-new-agent" className="nav-item nav-new" onClick={() => selectSetup()}><span className="nav-sigil">＋</span> new agent</button>
      </div>
      <div id="history-hint" className="nav-hint nav-foot" hidden={historyOk !== false}>transcripts unavailable — live view only</div>
    </nav>
  );
}

function WelcomeView({ hidden, primary, historyOk, selectAgent, selectSetup, selectSignals }: any) {
  return (
    <div id="view-welcome" className="view" hidden={hidden}>
      <div className="welcome-pane">
        <p className="welcome-lead">talk to your agents, watch what's happening, and add useful behavior.</p>
        <div id="welcome-agent" className="welcome-agent">
          {!primary ? <div className="dim-note">no agents yet — create your first one.</div> : (
            <>
              <div className="welcome-agent-label">your agent</div>
              <div className="welcome-agent-row">
                <span className="welcome-agent-name">{primary}</span>
                <button onClick={() => selectAgent(primary, 'converse')}>converse with {primary}</button>
                <button className="ghost" onClick={() => selectAgent(primary, 'configure')}>configure</button>
              </div>
            </>
          )}
        </div>
        <div className="welcome-actions">
          <button id="welcome-new" className="ghost" onClick={() => selectSetup()}>＋ new agent</button>
          <button id="welcome-kits" className="ghost" onClick={() => selectSetup()}>⚒ add-ons</button>
          <button id="welcome-signals" className="ghost" onClick={selectSignals}>◮ live signals</button>
        </div>
        <p id="welcome-hint" className="dim-note">{historyOk === false ? 'transcripts are unavailable until the history view is on.' : ''}</p>
      </div>
    </div>
  );
}

function SetupView({ hidden, setup, provenance, newAgent, setNewAgent, newAgentNote, createAgent, modelsHint, loadSetup }: any) {
  const kits = setup.kits;
  const pkgs = setup.packages;
  const proposals = setup.proposals;
  return (
    <div id="view-setup" className="view" hidden={hidden}>
      <div className="setup-pane">
        <div id="setup-status" className={`setup-status${setup.statusKind ? ` status-${setup.statusKind}` : ''}`} role="status" aria-live="polite" hidden={!setup.status}>{setup.status}</div>
        <section className="setup-block">
          <h3>new agent</h3>
          <p className="dim-note">an agent has a name, a model, and an identity you can edit. created instantly — no review needed.</p>
          <div className="setup-row">
            <input id="na-name" placeholder="name (e.g. kestrel)" spellCheck={false} value={newAgent.name} onChange={(e) => setNewAgent({ ...newAgent, name: e.target.value })} />
            <input id="na-model" placeholder="model (default: claude-sonnet-4-6)" spellCheck={false} list="model-suggestions" value={newAgent.model} onChange={(e) => setNewAgent({ ...newAgent, model: e.target.value })} />
            <button id="na-create" onClick={createAgent}>create agent</button>
            <span id="na-note" className="dim-note">{newAgentNote}</span>
          </div>
          <p id="models-hint" className="dim-note" hidden={!modelsHint}>{modelsHint}</p>
        </section>
        <section className="setup-block">
          <h3>available add-ons</h3>
          <p className="dim-note">ready-made behavior shared by all your agents. adding one takes effect right away.</p>
          <div id="setup-kits">
            {setup.loading || !kits ? 'resolving…' : kits.ok === false ? <div className="dim-note">add-ons could not load: {kits.error ?? 'unknown - is the elanus binary on the server PATH current?'}</div>
              : !(kits.kits ?? []).length ? <div className="dim-note">no add-ons found</div>
                : kits.kits.map((k: any) => <SetupKit key={k.name} kit={k} installed={provenance.has(k.name)} loadSetup={loadSetup} />)}
          </div>
        </section>
        <section className="setup-block">
          <h3>installed add-ons</h3>
          <p className="dim-note">adjust settings for the things you have added.</p>
          <div id="setup-configs">
            {setup.loading || !pkgs ? 'checking…' : pkgs.ok === false ? <div className="dim-note">could not load installed add-ons: {pkgs.error ?? 'unknown error'}</div>
              : !(pkgs.packages ?? []).length ? <div className="dim-note">nothing added yet</div>
                : pkgs.packages.map((p: any) => <SetupPackageConfig key={p.name} pkg={p} />)}
          </div>
        </section>
        <section className="setup-block">
          <h3>agent requests</h3>
          <p className="dim-note">when an agent suggests a settings change, accept or decline it here.</p>
          <div id="setup-pending">
            {setup.loading || !proposals ? 'checking…' : proposals.ok === false ? <div className="dim-note">could not load agent requests: {proposals.error ?? 'unknown error'}</div>
              : !(proposals.proposals ?? []).length ? <div className="dim-note">no agent requests</div>
                : proposals.proposals.map((p: any) => <ProposalCard key={p.proposal} proposal={p} loadSetup={loadSetup} />)}
          </div>
        </section>
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
        <span className="setup-kit-hook dim-note">{kit.hook || ''}</span>
        {installed && <span className="badge">installed</span>}
        <button className="ghost" onClick={toggle}>details</button>
        <button onClick={add} disabled={busy}>{busy ? 'adding...' : installed ? 'add again' : 'add'}</button>
      </div>
      <pre className="setup-readme" hidden={!open}>{readme}</pre>
    </div>
  );
}

function SetupPackageConfig({ pkg }: any) {
  const [key, setKey] = useState('');
  const [value, setValue] = useState('');
  const [note, setNote] = useState('');
  const [raw, setRaw] = useState('not loaded');
  const active = (pkg.grants ?? []).some((g: any) => g.state === 'approved');
  const loadRaw = async () => {
    setRaw('loading...');
    const r = await adminGet(`configs?package=${encodeURIComponent(pkg.name)}`);
    setRaw(r.ok ? (r.config?.toml || 'no settings yet') : (r.error ?? 'could not load settings'));
    return r.ok ? (r.config?.toml || 'no settings yet') : null;
  };
  const save = async () => {
    if (!key.trim()) { setNote('name the setting first'); return; }
    setNote('');
    const r = await adminPost('configs/set', { package: pkg.name, key: key.trim(), value });
    if (!r.ok) { setNote(r.error ?? 'save failed'); return; }
    const loaded = await loadRaw();
    setNote(loaded && loaded.includes(key.trim()) ? 'saved' : 'saved, but could not verify the reload');
  };
  return (
    <div className="setup-pending-pkg">
      <div className="setup-kit-head"><span className="setup-kit-name">{pkg.name}</span><span className={active ? 'badge' : 'badge badge-wait'}>{active ? 'on' : 'off'}</span></div>
      <div className="setup-row">
        <input placeholder="setting" spellCheck={false} value={key} onChange={(e) => setKey(e.target.value)} />
        <input placeholder="value, using TOML for arrays or numbers" spellCheck={false} value={value} onChange={(e) => setValue(e.target.value)} />
        <button onClick={save}>save setting</button>
        <span className="dim-note">{note}</span>
      </div>
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
  const { hidden, modelOptions, form, setForm, cfgProfile, cfgParsed, cfgLoading, cfgNote, cfgToml, setCfgToml, cfgTomlNote, saveConfigure, saveRawToml, cfgPackages, cfgKits, cfgConfigPackages, cfgContextChain, setCfgContextChain, cfgContextVarEdits, setCfgContextVarEdits, contextDefs, availableContextStages, moveContextStage, removeContextStage, addContextStage, skillIncluded, skillExcluded, setSkillExcluded, setKitPackagesExcluded, openKitModal } = props;
  const [selectedContextStage, setSelectedContextStage] = useState('');
  const addStageValue = selectedContextStage || (availableContextStages[0] ? `${availableContextStages[0].package}/${availableContextStages[0].name}` : '');
  const disabled = cfgLoading;
  const updateVar = (id: string, patch: any) => setForm({ varsRows: form.varsRows.map((r: any) => r.id === id ? { ...r, ...patch } : r) });
  const updateThrottle = (id: string, patch: any) => setForm({ throttleRows: form.throttleRows.map((r: any) => r.id === id ? { ...r, ...patch } : r) });
  return (
    <div id="view-configure" className="view" hidden={hidden}>
      <div className="setup-pane cfg-pane">
        <aside className="cfg-index" aria-label="configure sections">
          {['agent', 'model', 'context', 'sandbox', 'packages', 'throttle', 'raw'].map((s) => <a key={s} href={`#cfg-section-${s}`}>{s}</a>)}
        </aside>
        <div className="cfg-sections">
          <section className="setup-block" id="cfg-section-agent">
            <h3>agent</h3>
            <p className="dim-note">edits land in <code id="cfg-file">{cfgProfile ? `${cfgProfile} settings` : "this agent's settings file"}</code> and apply on the agent's next run.</p>
            <div className="cfg-grid">
              <label>agent <input id="cfg-agent" disabled={disabled} spellCheck={false} value={form.agent} onChange={(e) => setForm({ agent: e.target.value })} /></label>
              <label>owner <input id="cfg-owner" disabled={disabled} spellCheck={false} value={form.owner} onChange={(e) => setForm({ owner: e.target.value })} /></label>
              <label>autonomy <select id="cfg-autonomy" disabled={disabled} value={form.autonomy} onChange={(e) => setForm({ autonomy: e.target.value })}>{['off', 'manual', 'assisted', 'autonomous'].map((v) => <option key={v} value={v}>{v}</option>)}</select></label>
              <label>parent <input id="cfg-parent" disabled={disabled} spellCheck={false} placeholder="default" value={form.parent} onChange={(e) => setForm({ parent: e.target.value })} /></label>
              <label>prepend path <input id="cfg-package-path" disabled={disabled} spellCheck={false} placeholder="kits/dev, /opt/elanus/packages" value={form.packagePath} onChange={(e) => setForm({ packagePath: e.target.value })} /></label>
              <label className="cfg-check"><input id="cfg-path-inherit" disabled={disabled} type="checkbox" checked={form.pathInherit} onChange={(e) => setForm({ pathInherit: e.target.checked })} /> include inherited path</label>
              <label>effective path <input id="cfg-effective-path" disabled={disabled} spellCheck={false} readOnly value={form.effectivePath} /></label>
            </div>
            <div className="setup-row"><button id="cfg-save" disabled={disabled} onClick={saveConfigure}>save</button><span id="cfg-note" className="dim-note">{cfgNote}</span></div>
            <p className="dim-note">renaming changes the mailbox for future runs; old messages and history stay under the old name.</p>
          </section>

          <section className="setup-block" id="cfg-section-model">
            <h3>model</h3>
            <div className="cfg-grid">
              <label>model <input id="cfg-model" disabled={disabled} spellCheck={false} list="model-suggestions" value={form.model} onChange={(e) => setForm({ model: e.target.value })} /></label>
              <label>max run steps <input id="cfg-turns" disabled={disabled} type="number" min="1" max="200" value={form.turns} onChange={(e) => setForm({ turns: e.target.value })} /></label>
              <label>base URL <input id="cfg-base-url" disabled={disabled} spellCheck={false} placeholder="provider default" value={form.baseUrl} onChange={(e) => setForm({ baseUrl: e.target.value })} /></label>
              <label>API key env <input id="cfg-api-key-env" disabled={disabled} spellCheck={false} placeholder="adapter default" value={form.apiKeyEnv} onChange={(e) => setForm({ apiKeyEnv: e.target.value })} /></label>
            </div>
            <p className="dim-note">caps one activation's model/tool loop, not the lifetime of the conversation.</p>
            <datalist id="model-suggestions">
              {(modelOptions.length ? modelOptions.map((m: any) => ({ value: m.id, label: m.display_name })) : [
                { value: 'claude-fable-5' }, { value: 'claude-sonnet-4-6' }, { value: 'claude-haiku-4-5-20251001' }, { value: 'anthropic::deepseek-chat' },
              ]).map((m: any) => <option key={m.value} value={m.value} label={m.label} />)}
            </datalist>
          </section>

          <section className="setup-block" id="cfg-section-context">
            <h3>context program</h3>
            <div className="cfg-grid">
              <label>program <input id="cfg-context-program" disabled={disabled} spellCheck={false} value={form.contextProgram} onChange={(e) => setForm({ contextProgram: e.target.value })} /></label>
              <label>max context ms <input id="cfg-context-max-ms" disabled={disabled} type="number" min="1" value={form.contextMaxMs} onChange={(e) => setForm({ contextMaxMs: e.target.value })} /></label>
            </div>
            <div className="cfg-context-head"><h4>context stage chain</h4><div className="cfg-context-add"><select id="cfg-context-add-stage" disabled={disabled || !availableContextStages.length} value={addStageValue} onChange={(e) => setSelectedContextStage(e.target.value)}>{availableContextStages.map((s: any) => <option key={`${s.package}/${s.name}`} value={`${s.package}/${s.name}`}>{s.package}/{s.name}</option>)}</select><button id="cfg-context-add" className="ghost" disabled={disabled || !availableContextStages.length} type="button" onClick={() => addContextStage(addStageValue)}>add</button></div></div>
            <div id="cfg-context-chain" className="cfg-context-chain">
              {!contextDefs.length ? <div className="dim-note">no visible package context stages</div> : !cfgContextChain.length ? <div className="dim-note">all visible context stages are removed for this agent</div> : cfgContextChain.map((stage: any, index: number) => (
                <ContextStageTile key={`${stage.package}/${stage.name}`} stage={stage} index={index} chainLength={cfgContextChain.length} disabled={disabled} move={moveContextStage} remove={removeContextStage} setChain={setCfgContextChain} cfgParsed={cfgParsed} cfgContextVarEdits={cfgContextVarEdits} setCfgContextVarEdits={setCfgContextVarEdits} />
              ))}
            </div>
            <p className="dim-note">Stages run top to bottom after the built-in seed. Reorder or remove stages here; raw TOML stores this as the <code>context.stage</code> array.</p>
          </section>

          <section className="setup-block" id="cfg-section-sandbox">
            <h3>sandbox</h3>
            <div className="cfg-grid">
              <label>working directory <input id="cfg-workdir" disabled={disabled} spellCheck={false} placeholder="(elanus root)" value={form.workdir} onChange={(e) => setForm({ workdir: e.target.value })} /></label>
              <label>writable prefixes <input id="cfg-fs-write" disabled={disabled} spellCheck={false} placeholder="comma separated" value={form.fsWrite} onChange={(e) => setForm({ fsWrite: e.target.value })} /></label>
              <label>capture exclude <input id="cfg-capture-exclude" disabled={disabled} spellCheck={false} placeholder="comma separated" value={form.captureExclude} onChange={(e) => setForm({ captureExclude: e.target.value })} /></label>
            </div>
          </section>

          <section className="setup-block" id="cfg-section-throttle">
            <h3>throttle</h3>
            <div id="cfg-throttle" className="cfg-table">
              {form.throttleRows.map((r: any) => <div key={r.id} className="cfg-throttle-row"><input className="cfg-throttle-name" disabled={disabled} placeholder="name" spellCheck={false} value={r.name} onChange={(e) => updateThrottle(r.id, { name: e.target.value })} /><input className="cfg-throttle-max" disabled={disabled} type="number" placeholder="max concurrent" value={r.max} onChange={(e) => updateThrottle(r.id, { max: e.target.value })} /><input className="cfg-throttle-rate" disabled={disabled} type="number" placeholder="rate/min" value={r.rate} onChange={(e) => updateThrottle(r.id, { rate: e.target.value })} /><input className="cfg-throttle-tokens" disabled={disabled} type="number" placeholder="tokens/hour" value={r.tokens} onChange={(e) => updateThrottle(r.id, { tokens: e.target.value })} /><label className="cfg-check"><input className="cfg-throttle-coalesce" disabled={disabled} type="checkbox" checked={r.coalesce} onChange={(e) => updateThrottle(r.id, { coalesce: e.target.checked })} /> coalesce</label></div>)}
            </div>
            <button id="cfg-throttle-add" className="ghost" type="button" disabled={disabled} onClick={() => setForm({ throttleRows: [...form.throttleRows, { id: uid(), name: '', max: '', rate: '', tokens: '', coalesce: false }] })}>add throttle</button>
          </section>

          <section className="setup-block" id="cfg-section-packages">
            <h3>packages</h3>
            <input id="cfg-include" type="hidden" value={form.include} readOnly />
            <input id="cfg-exclude" type="hidden" value={form.exclude} readOnly />
            <div className="setup-row cfg-package-toolbar"><button id="cfg-kit-add-toggle" type="button" disabled={disabled} onClick={openKitModal}>add</button><span className="dim-note">copy or link kits to change available packages</span></div>
            <div id="cfg-package-configs" className="cfg-tree">
              <PackageTree packages={cfgPackages.filter((p: any) => skillIncluded(p))} skillExcluded={skillExcluded} setSkillExcluded={setSkillExcluded} setKitPackagesExcluded={setKitPackagesExcluded} cfgConfigPackages={cfgConfigPackages} />
            </div>
          </section>

          <section className="setup-block" id="cfg-section-raw">
            <details><summary className="dim-note">advanced context parameters</summary><p className="dim-note">legacy <code>[vars]</code> values for context stages and templates; prefer package/context-stage settings when available. Saved by the main save button above.</p><div id="cfg-vars" className="cfg-table">{form.varsRows.map((r: any) => <div key={r.id} className="cfg-var-row"><input className="cfg-var-key" disabled={disabled} placeholder="name" spellCheck={false} value={r.key} onChange={(e) => updateVar(r.id, { key: e.target.value })} /><input className="cfg-var-value" disabled={disabled} placeholder="value" spellCheck={false} value={r.value} onChange={(e) => updateVar(r.id, { value: e.target.value })} /></div>)}</div><button id="cfg-var-add" className="ghost" type="button" disabled={disabled} onClick={() => setForm({ varsRows: [...form.varsRows, { id: uid(), key: '', value: '' }] })}>add context parameter</button></details>
            <details><summary className="dim-note">the raw settings file</summary><textarea id="cfg-toml" disabled={disabled} spellCheck={false} rows={14} value={cfgToml} onChange={(e) => setCfgToml(e.target.value)} /><div className="setup-row"><button id="cfg-toml-save" disabled={disabled} onClick={saveRawToml}>save raw file</button><span id="cfg-toml-note" className="dim-note">{cfgTomlNote}</span></div></details>
          </section>
        </div>
      </div>
    </div>
  );
}

function ContextStageTile({ stage, index, chainLength, disabled, move, remove, setChain, cfgParsed, cfgContextVarEdits, setCfgContextVarEdits }: any) {
  const key = `${stage.package}/${stage.name}`;
  const params = (stage.config ?? []).filter((p: any) => p.key).map((p: any) => ({ key: p.key, type: p.type ?? 'string', label: p.label || p.key, help: p.help || '', default: p.default, options: p.options ?? [], agent_tunable: p.agent_tunable === true, source: `context stage ${stage.name}` }));
  const updateStage = (patch: any) => setChain((prev: any[]) => prev.map((s) => `${s.package}/${s.name}` === key ? { ...s, ...patch } : s));
  return (
    <div className="cfg-context-stage" data-stage={key}>
      <div className="cfg-context-stage-head"><div className="cfg-context-stage-title"><strong>{key}</strong><span className="cfg-config-help">mode: {stage.mode} · {params.length} declared setting{params.length === 1 ? '' : 's'}</span></div><div className="cfg-context-stage-actions"><IconButton label={`move ${key} up`} disabled={disabled || index === 0} onClick={() => move(index, -1)}>↑</IconButton><IconButton label={`move ${key} down`} disabled={disabled || index === chainLength - 1} onClick={() => move(index, 1)}>↓</IconButton><IconButton label={`remove ${key}`} disabled={disabled} onClick={() => remove(index)}>×</IconButton></div></div>
      <div className="cfg-context-stage-grid"><label>order<input type="number" min="1" data-context-field="order" disabled={disabled} value={stage.order} onChange={(e) => updateStage({ order: Number(e.target.value || stage.order) })} /></label><label>timeout ms<input type="number" min="1" data-context-field="timeout_ms" disabled={disabled} value={stage.timeout_ms} onChange={(e) => updateStage({ timeout_ms: Number(e.target.value || stage.timeout_ms) })} /></label></div>
      {!!params.length && <div className="cfg-context-stage-config"><div className="cfg-context-stage-subhead">settings</div>{params.map((param: any) => {
        const value = cfgContextVarEdits.get(param.key) ?? cfgParsed.vars?.[param.key] ?? tomlDisplayValue(param.default, param.type);
        const setValue = (v: string) => setCfgContextVarEdits((old: Map<string, string>) => new Map(old).set(param.key, v));
        return <ConfigInputRow key={param.key} param={param} value={value} setValue={setValue} contextVar={param.key} contextStage={key} />;
      })}</div>}
    </div>
  );
}

function ConfigInputRow({ param, value, setValue, contextVar, contextStage, save }: any) {
  return (
    <div className="cfg-config-row">
      <label title={param.key}>{param.label || param.key}<span className="cfg-config-help">{[param.source, param.type ? `type: ${param.type}` : '', param.agent_tunable ? 'agent-tunable' : '', param.help].filter(Boolean).join(' · ')}</span></label>
      {param.type === 'boolean' ? <label className="cfg-check" data-value-input="1"><input type="checkbox" checked={/^(true|1)$/i.test(String(value))} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.checked ? 'true' : 'false')} /> enabled</label>
        : param.type === 'enum' && (param.options ?? []).length ? <select value={value || param.options[0]} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.value)}>{param.options.map((o: string) => <option key={o} value={o}>{o}</option>)}</select>
          : <input type={param.type === 'number' ? 'number' : 'text'} spellCheck={false} value={value ?? ''} placeholder={param.default == null ? 'value' : `default ${tomlDisplayValue(param.default, param.type)}`} data-context-var={contextVar} data-context-stage={contextStage} data-dirty="1" onChange={(e) => setValue(e.target.value)} />}
      {save ?? <span />}
      <span />
    </div>
  );
}

function PackageTree({ packages, skillExcluded, setSkillExcluded, setKitPackagesExcluded, cfgConfigPackages }: any) {
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
          return <PackageCard key={`${p.name}-${disabled ? 'disabled' : 'enabled'}`} pkg={p} disabled={disabled} canConfigure={declaredConfigParams(p).length > 0 || cfgConfigPackages.has(p.name)} toggle={() => setSkillExcluded(p.name, !disabled)} />;
        })}</div>
      </details>
    );
  });
}

function PackageCard({ pkg, disabled, canConfigure, toggle }: any) {
  const [panelOpen, setPanelOpen] = useState(false);
  const [rows, setRows] = useState<any[] | null>(null);
  const source = packageSource(pkg);
  const declared = declaredConfigParams(pkg);
  const load = async () => {
    setPanelOpen((v) => !v);
    if (rows) return;
    const r = await adminGet(`configs?package=${encodeURIComponent(pkg.name)}`);
    const raw = r.ok ? (r.config?.toml || '') : '';
    const current = parseConfigRows(raw);
    const byKey = new Map(current.map((row) => [row.key, row.value]));
    const params = new Map(declared.map((param: any) => [param.key, param]));
    for (const param of declared) if (!byKey.has(param.key)) byKey.set(param.key, '');
    for (const key of byKey.keys()) if (!params.has(key)) params.set(key, { key, type: 'string', label: key, help: 'Existing package setting not declared in the manifest.', source: 'current settings' });
    setRows([...params.values()].sort((a: any, b: any) => a.key.localeCompare(b.key)).map((param: any) => ({ param, value: byKey.get(param.key) || tomlDisplayValue(param.default, param.type), note: '' })));
  };
  const saveRow = async (idx: number) => {
    const row = rows![idx];
    setRows(rows!.map((r, i) => i === idx ? { ...r, note: 'saving...' } : r));
    const write = await adminPost('configs/set', { package: pkg.name, key: row.param.key, value: row.value });
    setRows((old) => old!.map((r, i) => i === idx ? { ...r, note: write.ok ? 'saved' : (write.error ?? 'save failed') } : r));
  };
  return (
    <details className={`cfg-package-card${disabled ? ' is-disabled' : ''}`} data-package={pkg.name}>
      <summary className="cfg-package-head"><span className="cfg-disclosure">▸</span><span className={`cfg-source-icon source-${source.kind}`} title={`${source.kind}: ${source.label}`}>{source.icon}</span><span className="cfg-package-title"><span className="setup-kit-name">{pkg.name}</span><span className="cfg-pkg-desc">{packageDescription(pkg)}</span></span></summary>
      <div className="cfg-package-body"><div className="cfg-package-detail">{actorDetail(pkg)}</div><div className="cfg-package-meta">{packageBadges(pkg).map((b) => <span key={b.text} className={b.cls}>{b.text}</span>)}</div><div className="cfg-package-controls"><span className="dim-note">{disabled ? 'disabled for this agent' : 'enabled for this agent'}</span><button className={disabled ? 'ghost cfg-package-disable' : 'cfg-package-disable'} title={disabled ? `remove ${pkg.name} from skills.exclude` : `add ${pkg.name} to skills.exclude`} onClick={(e) => { e.preventDefault(); toggle(); }}>{disabled ? 'enable' : 'disable'}</button><button className="ghost cfg-package-config-toggle" hidden={!canConfigure} onClick={(e) => { e.preventDefault(); load(); }}>settings</button></div>
        <div className="cfg-package-config-panel" hidden={!panelOpen}>{rows === null ? 'loading...' : !rows.length ? <div className="dim-note">no configurable settings declared</div> : rows.map((row, idx) => <ConfigInputRow key={row.param.key} param={row.param} value={row.value} setValue={(v: string) => setRows(rows.map((r, i) => i === idx ? { ...r, value: v } : r))} save={<><IconButton label={`save ${pkg.name}.${row.param.key}`} onClick={() => saveRow(idx)}>✓</IconButton><span className="dim-note">{row.note}</span></>} />)}</div>
      </div>
    </details>
  );
}

function KitModal({ modalRef, open, close, kits, cfgForm, cfgPackages, cfgKitDetails, loadKitDetail, installKitForAgent }: any) {
  return (
    <dialog id="cfg-kit-add-modal" className="cfg-modal" ref={modalRef} onClick={(e) => { if (e.target === e.currentTarget) close(); }}>
      <div className="cfg-modal-head"><div><h3>add kit</h3><p className="dim-note">kits expand into packages for this agent.</p></div><button id="cfg-kit-add-close" className="cfg-icon-btn" type="button" aria-label="close add kit" onClick={close}>×</button></div>
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
      <div className="cfg-skill-table">{!detail ? <div className="dim-note">expand to load packages</div> : !(detail.packages ?? []).length ? <div className="dim-note">{detail.error ?? 'no packages in this kit'}</div> : detail.packages.map((p: any) => <div key={p.name} className="cfg-skill-row cfg-kit-preview-row"><span className="cfg-pkg-name">{p.name}<span className="cfg-preview-badges">{p.skill && <span className="badge">skill</span>}{p.manifest?.actor && <span className="badge badge-wait">{p.manifest.actor}</span>}</span></span><span className="cfg-pkg-desc">{actorDetail(p.manifest?.process || !p.manifest?.mode ? p : { ...p, manifest: { ...p.manifest, process: { mode: p.manifest.mode, run: p.manifest.run, http: p.manifest.http } } })}</span><span className="cfg-skill-actions" /></div>)}</div>
      <div className="dim-note">{note}</div>
    </details>
  );
}

function ConverseView({ hidden, agent, messages, submitCompose, answerAsk }: any) {
  return (
    <div id="view-converse" className="view" hidden={hidden}>
      <div id="conv-holder" className="conv-feed-holder">
        <div className="conv-feed">
          {!messages.length && <div className="conv-empty"><p className="conv-empty-mark">⟁</p><p>nothing yet — say something below.<br />asks and replies thread here by correlation.</p></div>}
          {messages.map((m: any) => m.type === 'ask' ? <AskMessage key={m.id} agent={agent} message={m} answerAsk={answerAsk} /> : <div key={m.id} className={`msg ${m.cls}`} title={m.corr ? `correlation ${m.corr}` : ''}><div className="msg-meta"><span className="msg-who">{m.who}</span></div><div className="msg-body">{m.failed ? <><div className="fail-reason">{m.text}</div><div className="fail-hint">check the agent: a model set, the background service running, and the add-on turned on.</div></> : m.text}</div></div>)}
        </div>
      </div>
      <form id="compose" className="compose" autoComplete="off" onSubmit={submitCompose}><span className="compose-sigil">»</span><input id="compose-input" type="text" placeholder="message the agent…" spellCheck={false} /><button type="submit" id="compose-send">transmit</button></form>
    </div>
  );
}

function AskMessage({ agent, message, answerAsk }: any) {
  const [text, setText] = useState('');
  const p = message.payload ?? {};
  const send = (answer: string) => answer && answerAsk(agent, message.id, message.corr, answer);
  return (
    <div className="msg agent ask"><div className="msg-meta"><span className="msg-who">agent asks</span>{message.corr && <span className="msg-corr">{message.corr.slice(0, 18)}</span>}</div><div className="msg-body"><div className="ask-q">{p.question ?? summarize(p)}</div>{message.answered ? <div className="ask-done">{message.answered.includes(':') ? <>{message.answered.split(':')[0]}: <b>{message.answered.split(':').slice(1).join(':').trim()}</b></> : message.answered}</div> : <>{Array.isArray(p.options) && !!p.options.length && <div className="ask-options">{p.options.map((o: any) => <button key={String(o)} onClick={() => send(String(o))}>{String(o)}</button>)}</div>}<div className="ask-row"><input placeholder="answer…" value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter' && text.trim()) send(text.trim()); }} /><button onClick={(e) => { e.preventDefault(); send(text.trim()); }}>answer</button></div></>}</div></div>
  );
}

function RailView({ hidden, filter, setFilter, paused, setPaused, rows }: any) {
  const verbClass = (topic: string) => topic.startsWith('signal/') ? 'v-signal' : topic.startsWith('in/') ? 'v-in' : /^obs\/[^/]+\/[^/]+\/[^/]+\/tool\//.test(topic) ? 'v-tool' : 'v-obs';
  return (
    <div id="view-rail" className="view" hidden={hidden}>
      <div className="rail-bar"><div className="tele-filters" role="tablist">{['all', 'work', 'tools', 'signals'].map((f) => <button key={f} data-f={f} className={filter === f ? 'on' : ''} onClick={() => setFilter(f)}>{f}</button>)}<button id="tele-pause" title="pause the feed" onClick={() => setPaused(!paused)}>{paused ? '▶' : '⏸'}</button></div></div>
      <div id="tele-feed" className="tele-feed">{!paused && rows.map((m: any, i: number) => <div key={`${i}-${m.topic}-${m.env?.id ?? ''}`} className={`row ${verbClass(m.topic)}`}><span className="t">{timeOf(m.env)}</span><span><span className="topic">{m.topic} </span><span className="pay">{summarize(m.env?.payload)}</span></span></div>)}</div>
    </div>
  );
}

function SessionsView({ hidden, state, agent, openTranscript, loadSessions }: any) {
  return (
    <div id="view-sessions" className="view" hidden={hidden}>
      <div id="sessions-pane" className="sessions-pane">
        {state.status === 'loading' && <div className="dim-note">asking the history view…</div>}
        {state.status === 'error' && <div className="dim-note"><div>transcripts unavailable — live view only.</div>{state.error && <div className="dim-sub">{state.error}</div>}</div>}
        {state.status === 'list' && (!state.sessions.length ? <div className="dim-note">no recorded sessions for this agent yet.</div> : <div className="sess-list"><div className="sess-row sess-head">{['session', 'first', 'last', 'msgs', 'events'].map((h) => <span key={h}>{h}</span>)}</div>{state.sessions.map((s: any) => <button key={s.session} className="sess-row" onClick={() => openTranscript(agent, s.session)}><span className="sess-id">{s.session}</span><span>{shortTs(s.first_ts)}</span><span>{shortTs(s.last_ts)}</span><span>{String(s.message_count)}</span><span>{String(s.event_count)}</span></button>)}</div>)}
        {(state.status === 'transcript-loading' || state.status === 'transcript') && <Transcript agent={agent} state={state} openTranscript={openTranscript} loadSessions={loadSessions} />}
      </div>
    </div>
  );
}

function Transcript({ agent, state, openTranscript, loadSessions }: any) {
  const tr = state.transcript;
  if (state.status === 'transcript-loading') return <div className="dim-note">reading transcript {tr?.session}…</div>;
  return (
    <>
      <div className="tr-bar"><button className="tr-back" onClick={() => loadSessions(agent)}>← sessions</button><span className="tr-title">{tr.session}</span></div>
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
