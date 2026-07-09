import { firstSentence, shortList } from './format';

export function packageSource(pkg: any) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return { kind: 'linked', label: parts[kits + 1], icon: '↗' };
  const packages = parts.lastIndexOf('packages');
  if (packages >= 0) return { kind: 'copied', label: 'instance', icon: '⬚' };
  return { kind: 'path', label: 'path entry', icon: '•' };
}
export function kitNameFor(pkg: any) {
  const parts = String(pkg.dir ?? '').split(/[\\/]/);
  const kits = parts.lastIndexOf('kits');
  if (kits >= 0 && parts[kits + 1]) return parts[kits + 1];
  const source = packageSource(pkg);
  return source.kind === 'copied' ? 'instance' : source.label;
}
export function packageDescription(pkg: any) {
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
export function actorDetail(pkg: any) {
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
export function packageBadges(pkg: any) {
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
export function grantState(pkg: any) {
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
export function livenessState(liveness: any, name: string) {
  const status = liveness?.actors?.[name]?.status;
  if (!status) return { label: 'not started', cls: 'idle' };
  if (status === 'running') return { label: 'running', cls: 'ok' };
  if (status === 'failed') return { label: 'failed', cls: 'bad' };
  if (status === 'stopped') return { label: 'stopped', cls: 'idle' };
  if (status === 'restarting') return { label: 'restarting', cls: 'warn' };
  return { label: status, cls: 'idle' };
}
export function riskBadges(pkg: any) {
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
export function capabilityOutcome(kit: any) {
  const hook = String(kit.hook ?? '').trim();
  if (hook) return hook;
  if (/core/i.test(kit.name)) return 'core agent behaviors and skills';
  if (/dev/i.test(kit.name)) return 'developer safety and coding-workflow helpers';
  if (/funnel/i.test(kit.name)) return 'turn incoming work into structured agent tasks';
  return 'adds reusable behavior to your agents';
}
export function declaredConfigParams(pkg: any) {
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
export function packageHasAgentScopedSettings(pkg: any) {
  return declaredConfigParams(pkg).some((param: any) => param.agentScoped);
}
export function tomlDisplayValue(value: any, type = 'string') {
  if (value === undefined || value === null) return '';
  if (type === 'array') return JSON.stringify(value);
  if (type === 'boolean') return value ? 'true' : 'false';
  return String(value);
}
export function parseConfigRows(raw = '') {
  const rows = [];
  for (const line of String(raw).split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('[')) continue;
    const m = trimmed.match(/^([A-Za-z0-9_.-]+)\s*=\s*(.*)$/);
    if (m) rows.push({ key: m[1], value: m[2] });
  }
  return rows;
}
export function configRowMap(raw = ''): Map<string, string> {
  return new Map(parseConfigRows(raw).map((row) => [row.key, row.value] as [string, string]));
}
export function displayConfigValue(raw: any, type = 'string') {
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
export function valueSourceLabel(source: string, agentName: string) {
  if (source === 'agent') return `overridden here for ${agentName || 'this agent'}`;
  if (source === 'shared') return 'from the shared default';
  if (source === 'package') return 'from the package default';
  return 'not set yet';
}
export function effectiveConfigValue(param: any, sharedRows: Map<string, string>, profileVars: any = {}, agentName = '') {
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
export function prunedSet(set: Record<string, any>) {
  const out: Record<string, any> = {};
  for (const [k, v] of Object.entries(set)) {
    if (v === '' && k !== 'sandbox.workdir' && k !== 'parent') continue;
    out[k] = v;
  }
  return out;
}
