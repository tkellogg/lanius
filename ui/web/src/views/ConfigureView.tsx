import { useState, useEffect, useRef, useMemo } from 'react';
import { adminGet, adminPost } from '../api';
import AgentAssistant, { ClientTool } from '../components/AgentAssistant';
import { IconButton, ModelField, WorkdirInput } from '../components/primitives';
import { arr, uid } from '../lib/format';
import { costSummary, autonomyConsequence, modelCostHint } from '../lib/cost';
import { kitNameFor, declaredConfigParams, packageSource, effectiveConfigValue, tomlDisplayValue, valueSourceLabel, parseConfigRows, packageDescription, packageHasAgentScopedSettings, actorDetail, packageBadges, riskBadges, grantState } from '../lib/packages';

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
                  rendered-and-[hidden], so this keeps the closed modal free of a
                  live SSE subscription. (Mounting no longer publishes anything —
                  helper-first-encounter M1 removed the auto-send; the intro is a
                  static bubble and the first send is the user's.) */}
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
      <summary className="cfg-kit-summary cfg-kit-head"><span>{kit.name}</span><span className="cfg-pkg-desc">{kit.hook || ''}</span><span className="cfg-kit-actions"><span className="cfg-split-action" hidden={installed}><button className="cfg-split-primary cfg-kit-add-btn" type="button" aria-label={`link ${kit.name}`} title={`link ${kit.name}`} onClick={(e) => { e.preventDefault(); installKitForAgent(kit, 'link', setNote); }}>link</button><button className="cfg-split-caret" type="button" aria-label={`more add actions for ${kit.name}`} title={`more add actions for ${kit.name}`} onClick={(e) => { e.preventDefault(); setMenu(!menu); }}>⌄</button><div className="cfg-action-menu" hidden={!menu}>{['link', 'copy'].map((action) => <button key={action} type="button" aria-label={`${action} ${kit.name}`} onClick={(e) => { e.preventDefault(); setMenu(false); installKitForAgent(kit, action, setNote); }}>{action}</button>)}</div></span><span className="badge" hidden={!installed}>installed</span></span></summary>
      <div className="cfg-skill-table">{!detail ? <div className="dim-note">expand to load abilities</div> : !(detail.packages ?? []).length ? <div className="dim-note">{detail.error ?? 'no abilities in this capability'}</div> : detail.packages.map((p: any) => <div key={p.name} className="cfg-skill-row cfg-kit-preview-row"><span className="cfg-pkg-name">{p.name}<span className="cfg-preview-badges">{p.skill && <span className="badge">skill</span>}{p.manifest?.actor && <span className="badge badge-wait">{p.manifest.actor}</span>}</span></span><span className="cfg-pkg-desc">{actorDetail(p.manifest?.process || !p.manifest?.mode ? p : { ...p, manifest: { ...p.manifest, process: { mode: p.manifest.mode, run: p.manifest.run, http: p.manifest.http } } })}</span><span className="cfg-skill-actions" /></div>)}</div>
      <div className="dim-note">{note}</div>
    </details>
  );
}

export default ConfigureView;
export { ConfigInputRow, KitModal };
