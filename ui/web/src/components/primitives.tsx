import * as Tooltip from '@radix-ui/react-tooltip';
import { useEffect, useState } from 'react';
import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { adminGet } from '../api';

export function Button(props: ButtonHTMLAttributes<HTMLButtonElement>) {
  return <button type={props.type ?? 'button'} {...props} />;
}

export function IconButton({
  label,
  children,
  className = 'cfg-icon-btn',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return (
    <button type="button" className={className} title={label} aria-label={label} {...props}>
      {children}
    </button>
  );
}

export function HelpTip({ children, tip }: { children: ReactNode; tip?: string }) {
  if (!tip) return <>{children}</>;
  return (
    <Tooltip.Provider delayDuration={250}>
      <Tooltip.Root>
        <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content className="tooltip" sideOffset={6}>
            {tip}
            <Tooltip.Arrow className="tooltip-arrow" />
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
    </Tooltip.Provider>
  );
}

export function Notice({ children, kind }: { children: ReactNode; kind?: 'ok' | 'err' }) {
  return <div className={`setup-status${kind ? ` status-${kind}` : ''}`}>{children}</div>;
}

const COST_HINT = (m: string) => {
  const s = String(m ?? '').toLowerCase();
  if (!s) return '';
  if (/haiku|mini|small|cheap|fast/.test(s)) return 'cheap';
  if (/sonnet|balanced|medium/.test(s)) return 'balanced';
  if (/opus|gpt-5|large|pro|max|power/.test(s)) return 'powerful';
  return 'unknown';
};
const MODEL_LABEL = (m: any) => {
  const id = typeof m === 'string' ? m : m.id;
  const display = typeof m === 'string' ? '' : m.display_name;
  const cost = COST_HINT(id);
  const tail = display && display !== id ? ` — ${display}` : '';
  return cost ? `${id}${tail} (${cost})` : `${id}${tail}`;
};

// Model picker: a real <select> over the provider's model list with a single
// "custom…" escape row (so Tim can still type a typo-prone model id without
// fighting the picker). When the provider list is empty, the field shows an
// honest "provider unavailable" state at the field instead of silently
// degrading to free text. ui-preferences.md: a text box is almost always the
// worst choice for what is in fact a closed set.
export function ModelField({ id, value, onChange, models, disabled, hint, native, onSetupProvider }: {
  id?: string;
  value: string;
  onChange: (v: string) => void;
  models: any[];
  disabled?: boolean;
  hint?: string;
  // A native-login provider (Claude.AI / ChatGPT login) has no API-key /models
  // endpoint to probe — the coding tool picks the model. Suppress the "provider
  // list unavailable" warning entirely; it was never a provider to probe (the
  // real fix for the spurious warning on an OAuth login — model-providers M4).
  native?: boolean;
  // When set, the empty-list state offers a link to the Providers page so a
  // person can define/select one (the literal #4 ask).
  onSetupProvider?: () => void;
}) {
  const list = models ?? [];
  const inList = list.some((m) => anyId(m) === value);
  const [custom, setCustom] = useState(false);
  const showCustom = custom || (!!value && !inList && !!list.length);
  if (!list.length) {
    return (
      <>
        <input id={id} disabled={disabled} spellCheck={false} value={value} onChange={(e) => onChange(e.target.value)} placeholder="model id" />
        {!native && (
          <span className="cfg-field-hint cfg-field-warn">
            provider list unavailable — type a model id
            {onSetupProvider
              ? <> or <button type="button" className="cfg-link" data-providers-link onClick={onSetupProvider}>set up a provider →</button></>
              : ' or set an API key'}
          </span>
        )}
        {hint && <span className="cfg-field-hint">{hint}</span>}
        <UnderpoweredHint value={value} />
      </>
    );
  }
  if (showCustom) {
    return (
      <>
        <select id={id} disabled={disabled} value="__custom__" onChange={(e) => { if (e.target.value !== '__custom__') { setCustom(false); onChange(e.target.value); } }}>
          <option value="__custom__">custom…</option>
          {list.map((m) => <option key={anyId(m)} value={anyId(m)}>{MODEL_LABEL(m)}</option>)}
        </select>
        <input disabled={disabled} spellCheck={false} value={value} onChange={(e) => onChange(e.target.value)} placeholder="model id" />
        {hint && <span className="cfg-field-hint">{hint}</span>}
        <UnderpoweredHint value={value} />
      </>
    );
  }
  return (
    <>
      <select id={id} disabled={disabled} value={value} onChange={(e) => { if (e.target.value === '__custom__') { setCustom(true); return; } onChange(e.target.value); }}>
        <option value="">(provider default)</option>
        {list.map((m) => <option key={anyId(m)} value={anyId(m)}>{MODEL_LABEL(m)}</option>)}
        <option value="__custom__">custom…</option>
      </select>
      {hint && <span className="cfg-field-hint">{hint}</span>}
      <UnderpoweredHint value={value} />
    </>
  );
}
const anyId = (m: any) => (typeof m === 'string' ? m : m.id);

// Underpowered-model advisory (docs/handoffs/agentic-configuration.md M3): a
// soft, non-blocking heuristic — small parameter tiers (`4b`..`14b`) or a
// reduced product tier (mini/nano/lite/tiny/flash-lite) may struggle with
// multi-step agent work. Mirrors the SAME heuristic the helper's model-guidance
// KB page states (kits/helper/packages/kb-lanius/kb/model-guidance.md), so the
// agent and the UI tell one story. Never blocks — advisory only.
const UNDERPOWERED_TIER_RE = /(mini|nano|lite|tiny|flash-lite)/i;
const UNDERPOWERED_SIZE_RE = /\b(\d{1,3})b\b/i;
export function isUnderpoweredModel(model: string): boolean {
  const s = String(model ?? '').trim();
  if (!s) return false;
  if (UNDERPOWERED_TIER_RE.test(s)) return true;
  const m = s.match(UNDERPOWERED_SIZE_RE);
  if (!m) return false;
  const n = parseInt(m[1], 10);
  return !Number.isNaN(n) && n <= 14;
}
function UnderpoweredHint({ value }: { value: string }) {
  if (!isUnderpoweredModel(value)) return null;
  return <span className="cfg-field-hint cfg-field-warn">may struggle with agent work (advisory only) — consider a stronger model if setup feels erratic</span>;
}

// Workdir/path input with a server-side exists/writable check on blur. A typo'd
// workdir silently runs tools in the lanius root today; this flags it before
// save. Text stays as the input — the picker is the inline validation state.
export function WorkdirInput({ id, value, onChange, disabled, placeholder }: {
  id?: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
  placeholder?: string;
}) {
  const [check, setCheck] = useState<'checking' | 'ok' | 'missing' | 'notdir' | 'readonly' | ''>('');
  // Clear the check whenever the value changes so we never show a stale verdict.
  useEffect(() => { setCheck(''); }, [value]);
  const onBlur = async () => {
    const v = value.trim();
    if (!v) { setCheck(''); return; }
    setCheck('checking');
    let r: any;
    try { r = await adminGet(`path-check?path=${encodeURIComponent(v)}`); } catch { setCheck(''); return; }
    if (!r || !r.ok || r.empty) { setCheck(''); return; }
    if (!r.exists) setCheck('missing');
    else if (!r.isDir) setCheck('notdir');
    else if (r.writable === false) setCheck('readonly');
    else setCheck('ok');
  };
  return (
    <>
      <input id={id} disabled={disabled} spellCheck={false} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />
      {check === 'checking' && <span className="cfg-field-hint">checking…</span>}
      {check === 'missing' && <span className="cfg-field-hint cfg-field-warn">path does not exist</span>}
      {check === 'notdir' && <span className="cfg-field-hint cfg-field-warn">path is not a directory</span>}
      {check === 'readonly' && <span className="cfg-field-hint cfg-field-warn">not writable by the agent</span>}
      {check === 'ok' && <span className="cfg-field-hint cfg-field-ok">exists</span>}
    </>
  );
}
