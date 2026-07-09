import { useState } from 'react';
import { adminGet, adminPost } from '../api';
import { IconButton, ModelField, WorkdirInput } from '../components/primitives';
import { costSummary } from '../lib/cost';
import { capabilityOutcome, declaredConfigParams, packageSource, livenessState, parseConfigRows, tomlDisplayValue, grantState, packageDescription, riskBadges } from '../lib/packages';
import { ConfigInputRow } from './ConfigureView';

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

export default SetupView;
