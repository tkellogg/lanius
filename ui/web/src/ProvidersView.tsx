// model-providers M4: the Providers page — the human's seat for the named,
// encrypted credential vault (docs/handoffs/model-providers.md). It lists every
// provider (metadata only; the secret is shown as a redaction the backend prints,
// never the bytes), adds one (the key posts to the safe backend path that pipes
// it on the CLI's stdin — never argv), tests reachability (the same `/models`
// probe the dispatcher uses, which also sources the model dropdown for a named
// provider), and removes one.
//
// This is the surface the "provider list unavailable → set up a provider →" link
// in ModelField navigates to (the literal #4 ask). A provider is a RESOURCE, not
// an identity or an authority — "protected" means encrypted at rest, full stop;
// choosing one is audited on the session-start obs, never gated.
import { useEffect, useState } from 'react';
import { adminGet, adminPost } from './api';

type Provider = {
  name: string;
  kind: string;
  wire: string | null;
  base_url: string | null;
  tool: string | null;
  headers: string[];
  secret: string | null;
};

type TestResult = {
  ok?: boolean;
  native?: boolean;
  reachable?: boolean | null;
  count?: number;
  error?: string;
  pending?: boolean;
};

const emptyForm = {
  name: '',
  kind: 'apikey' as 'apikey' | 'native',
  wire: 'anthropic',
  base_url: '',
  key: '',
  tool: '',
  headers: [] as { id: string; name: string; value: string }[],
};

const uid = () => Math.random().toString(36).slice(2);

export default function ProvidersView() {
  const [list, setList] = useState<Provider[]>([]);
  const [note, setNote] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [form, setForm] = useState({ ...emptyForm });
  const [addNote, setAddNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [tests, setTests] = useState<Map<string, TestResult>>(new Map());

  const load = async () => {
    const j = await adminGet('providers');
    setLoaded(true);
    if (j.ok) {
      setList(j.providers ?? []);
      setNote('');
    } else {
      setList([]);
      setNote(j.error ?? 'provider list unavailable');
    }
  };
  useEffect(() => { void load(); }, []);

  const setF = (patch: Partial<typeof emptyForm>) => setForm((f) => ({ ...f, ...patch }));

  const addProvider = async () => {
    const name = form.name.trim();
    if (!name) { setAddNote('a provider needs a name'); return; }
    if (form.kind === 'apikey' && !form.base_url.trim()) { setAddNote('an api-key provider needs a base URL'); return; }
    if (form.kind === 'apikey' && !form.key) { setAddNote('an api-key provider needs a key'); return; }
    setBusy(true);
    setAddNote('saving…');
    const headers = form.headers
      .filter((h) => h.name.trim())
      .map((h) => ({ name: h.name.trim(), value: h.value }));
    const body: any = form.kind === 'native'
      ? { name, kind: 'native', ...(form.tool.trim() ? { tool: form.tool.trim() } : {}) }
      : { name, kind: 'apikey', wire: form.wire, base_url: form.base_url.trim(), key: form.key, headers };
    const r = await adminPost('providers', body);
    setBusy(false);
    if (!r.ok) { setAddNote(r.error ?? 'add failed'); return; }
    setAddNote('');
    setForm({ ...emptyForm, headers: [] });
    await load();
  };

  const removeProvider = async (name: string) => {
    if (!window.confirm(`Remove provider ${name}? This deletes its stored credential.`)) return;
    const r = await adminPost('providers/rm', { name });
    if (!r.ok) { setNote(r.error ?? 'remove failed'); return; }
    setTests((prev) => { const next = new Map(prev); next.delete(name); return next; });
    await load();
  };

  const testProvider = async (name: string) => {
    setTests((prev) => new Map(prev).set(name, { pending: true }));
    const j = await adminGet(`providers/test?name=${encodeURIComponent(name)}`);
    setTests((prev) => new Map(prev).set(name, j as TestResult));
  };

  const addHeader = () => setF({ headers: [...form.headers, { id: uid(), name: '', value: '' }] });
  const updateHeader = (id: string, patch: any) => setF({ headers: form.headers.map((h) => h.id === id ? { ...h, ...patch } : h) });
  const removeHeader = (id: string) => setF({ headers: form.headers.filter((h) => h.id !== id) });

  return (
    <div id="view-providers" className="view pv-wrap">
      <style>{PV_STYLE}</style>
      <div className="pv-list-pane">
        <h3 className="pv-h">Model providers</h3>
        <p className="pv-dim">A provider is a named, encrypted credential the dispatcher or a coding tool can point at. The key is stored encrypted at rest; only its redaction is shown here.</p>
        {note && <div className="pv-err">{note}</div>}
        {loaded && !note && list.length === 0 && (
          <div className="pv-dim pv-empty">No providers yet. Add one on the right — an api-key provider (KEY + base URL, e.g. DeepSeek or a LiteLLM gateway) or a native-login provider ("use the coding tool's own login; inject nothing").</div>
        )}
        <div className="pv-list">
          {list.map((p) => {
            const t = tests.get(p.name);
            return (
              <div key={p.name} className="pv-row" data-provider={p.name}>
                <div className="pv-row-head">
                  <span className="pv-name">{p.name}</span>
                  <span className={`pv-kind pv-kind-${p.kind}`}>{p.kind === 'api_key' ? 'api key' : 'native login'}</span>
                  {p.wire && <span className="pv-wire">{p.wire}</span>}
                  <span className="pv-actions">
                    <button type="button" className="pv-btn" data-test-provider={p.name} onClick={() => testProvider(p.name)}>test</button>
                    <button type="button" className="pv-btn pv-btn-danger" onClick={() => removeProvider(p.name)}>remove</button>
                  </span>
                </div>
                {p.base_url && <div className="pv-meta"><span className="pv-meta-k">base URL</span><code>{p.base_url}</code></div>}
                {p.tool && <div className="pv-meta"><span className="pv-meta-k">tool</span><code>{p.tool}</code></div>}
                {p.secret && <div className="pv-meta"><span className="pv-meta-k">key</span><code className="pv-redacted">{p.secret}</code></div>}
                {p.headers?.length > 0 && <div className="pv-meta"><span className="pv-meta-k">headers</span><code>{p.headers.join(', ')}</code></div>}
                {t && (
                  <div className={`pv-test${t.reachable === false ? ' pv-test-bad' : ''}`} data-test-result={p.name}>
                    {t.pending ? 'testing…'
                      : t.native ? 'native login — nothing to probe (the tool uses its own login)'
                      : t.reachable ? `reachable — ${t.count ?? 0} model${t.count === 1 ? '' : 's'}`
                      : `unreachable — ${t.error ?? 'no /models endpoint answered'}`}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      <div className="pv-add-pane">
        <h3 className="pv-h">Add a provider</h3>
        <div className="pv-form">
          <label>name <input id="pv-name" spellCheck={false} placeholder="deepseek" value={form.name} onChange={(e) => setF({ name: e.target.value })} /><span className="pv-hint">lowercase letters, digits, hyphens</span></label>
          <label>kind
            <select id="pv-kind" value={form.kind} onChange={(e) => setF({ kind: e.target.value as any })}>
              <option value="apikey">api key (KEY + base URL)</option>
              <option value="native">native login (use the tool's own login)</option>
            </select>
          </label>
          {form.kind === 'apikey' ? (
            <>
              <label>wire
                <select id="pv-wire" value={form.wire} onChange={(e) => setF({ wire: e.target.value })}>
                  <option value="anthropic">anthropic</option>
                  <option value="openai">openai</option>
                </select>
              </label>
              <label>base URL <input id="pv-base-url" spellCheck={false} placeholder="https://api.deepseek.com/anthropic" value={form.base_url} onChange={(e) => setF({ base_url: e.target.value })} /></label>
              <label>API key <input id="pv-key" type="password" autoComplete="off" spellCheck={false} placeholder="sk-…" value={form.key} onChange={(e) => setF({ key: e.target.value })} /><span className="pv-hint">stored encrypted; sent once over loopback, never placed on the command line</span></label>
              <div className="pv-headers">
                <div className="pv-headers-head"><span>extra headers (optional)</span><button type="button" className="pv-btn" onClick={addHeader}>add header</button></div>
                {form.headers.map((h) => (
                  <div key={h.id} className="pv-header-row">
                    <input className="pv-header-name" spellCheck={false} placeholder="Name" value={h.name} onChange={(e) => updateHeader(h.id, { name: e.target.value })} />
                    <input className="pv-header-value" spellCheck={false} placeholder="Value" value={h.value} onChange={(e) => updateHeader(h.id, { value: e.target.value })} />
                    <button type="button" className="pv-btn pv-btn-danger" onClick={() => removeHeader(h.id)}>×</button>
                  </div>
                ))}
              </div>
            </>
          ) : (
            <label>tool (optional pin) <input id="pv-tool" spellCheck={false} placeholder="claude | codex | opencode" value={form.tool} onChange={(e) => setF({ tool: e.target.value })} /><span className="pv-hint">leave blank to keep it tool-agnostic</span></label>
          )}
          <div className="pv-form-foot">
            <button id="pv-add" type="button" disabled={busy} onClick={addProvider}>add provider</button>
            <span className="pv-note">{addNote}</span>
          </div>
        </div>
      </div>
    </div>
  );
}

const PV_STYLE = `
.pv-wrap { display: flex; gap: 16px; align-items: flex-start; }
.pv-list-pane { flex: 1 1 55%; min-width: 0; }
.pv-add-pane { flex: 1 1 45%; min-width: 0; border-left: 1px solid #2a2a2a; padding-left: 16px; }
.pv-h { margin: 0 0 8px; font-size: 14px; }
.pv-dim { color: #8a8a8a; font-size: 12px; max-width: 60ch; }
.pv-err { color: #ff8a8a; font-size: 12px; margin: 6px 0; }
.pv-empty { font-size: 12px; margin-top: 8px; }
.pv-list { display: flex; flex-direction: column; gap: 8px; margin-top: 10px; }
.pv-row { border: 1px solid #2a2a2a; border-radius: 6px; padding: 8px 10px; font-size: 12px; }
.pv-row-head { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.pv-name { font-family: ui-monospace, monospace; font-weight: 600; }
.pv-kind { font-size: 10px; padding: 1px 6px; border-radius: 8px; }
.pv-kind-api_key { background: #1f4a6f; color: #cfe6ff; }
.pv-kind-native_login { background: #4a3a1f; color: #ffe9a8; }
.pv-wire { font-size: 10px; padding: 1px 6px; border-radius: 8px; background: #2a2a2a; color: #bbb; }
.pv-actions { margin-left: auto; display: flex; gap: 6px; }
.pv-btn { font-size: 11px; padding: 2px 8px; border-radius: 4px; border: 1px solid #3a3a3a; background: #222; color: #ddd; cursor: pointer; }
.pv-btn:hover { background: #2c2c2c; }
.pv-btn-danger { color: #ffb4b4; border-color: #5a2a2a; }
.pv-meta { display: flex; gap: 8px; margin-top: 3px; align-items: baseline; }
.pv-meta-k { color: #7a7a7a; min-width: 64px; }
.pv-meta code { font-family: ui-monospace, monospace; color: #c8c8c8; word-break: break-all; }
.pv-redacted { color: #8a8a8a; }
.pv-test { margin-top: 6px; padding: 4px 6px; border-radius: 4px; background: rgba(31,111,63,0.18); color: #cfe; font-size: 11px; }
.pv-test-bad { background: rgba(122,31,31,0.22); color: #ffd2d2; }
.pv-form { display: flex; flex-direction: column; gap: 10px; }
.pv-form label { display: flex; flex-direction: column; gap: 4px; font-size: 11px; color: #9a9a9a; letter-spacing: 0.04em; }
.pv-form input, .pv-form select { font-size: 12px; padding: 4px 6px; background: #1a1a1a; border: 1px solid #333; border-radius: 4px; color: #e8e8e8; }
.pv-hint { color: #6a6a6a; font-size: 10.5px; letter-spacing: 0; }
.pv-headers { display: flex; flex-direction: column; gap: 4px; }
.pv-headers-head { display: flex; justify-content: space-between; align-items: center; font-size: 11px; color: #9a9a9a; }
.pv-header-row { display: flex; gap: 6px; }
.pv-header-name { flex: 0 0 35%; }
.pv-header-value { flex: 1 1 auto; }
.pv-form-foot { display: flex; gap: 10px; align-items: center; }
.pv-form-foot button { font-size: 12px; padding: 5px 12px; border-radius: 4px; border: 1px solid #3a5a3a; background: #1f3a24; color: #d8ffe8; cursor: pointer; }
.pv-form-foot button:disabled { opacity: 0.5; cursor: default; }
.pv-note { color: #9a9a9a; font-size: 11px; }
`;
