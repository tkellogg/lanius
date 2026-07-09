import { FormEvent, useEffect, useMemo, useRef, useState } from 'react';
import { adminGet, publish } from '../api';
import { openLiveStream } from '../live';
import { STALL_MS, SystemHealth } from '../lib/health';

export type ClientTool = {
  name: string;
  description: string;
  parameters: Record<string, any>;
  handler: (args: any) => Promise<any>;
};

// The concierge seam (helper-first-encounter H4, wonky bit 6). An OPTIONAL,
// documented entry point the Ask-button follow-up will use: when a `context`
// is present it rides the NEXT user-initiated send's payload (alongside
// `client_tools`), so the helper can open already knowing what the person was
// looking at. Nothing wires it yet — the prop exists, is exercised by one
// unit-level assertion (`buildSendPayload` below), and the follow-up plugs view
// context in here without touching the send path again.
export type AssistantContext = { view: string; detail?: Record<string, any> };

type AgentAssistantProps = {
  profile?: string;
  tools: ClientTool[];
  title?: string;
  /** The static first bubble the helper greets with. It is NOT sent — opening
   *  the panel publishes nothing (wonky bit 1); the first live turn starts on
   *  the person's first message. */
  intro?: string;
  onDone?: () => void;
  /** The shared health projection (lib/health.ts). Present ⇒ the send-time
   *  pre-check refuses to publish into a known void (broker down / world c). */
  health?: SystemHealth;
  /** navigate-to-setup for the dead-air recourse ("check status"). Absent ⇒ the
   *  affordance is hidden (retry still works). */
  onCheckStatus?: () => void;
  /** localStorage key for conversation continuity (wonky bit 5). Present ⇒ the
   *  session id persists across close/reopen in this browser. Absent ⇒ a fresh
   *  session per mount (the configure modal's existing behavior). */
  persistKey?: string;
  /** Concierge seam (wonky bit 6) — see AssistantContext above. */
  context?: AssistantContext;
};

type AssistantItem =
  | { id: string; kind: 'user' | 'assistant' | 'status'; text: string }
  | { id: string; kind: 'nopath'; text: string }
  | { id: string; kind: 'tool'; callId: string; name: string; args?: any; result?: any; error?: string; status: 'running' | 'done' | 'error' };

// The in-flight turn's dead-air state (wonky bit 3), mirroring ConverseView's
// three-state machine over the SAME 20s constant and vocabulary:
// sent → thinking (on correlated obs) → stalled (20s of silence). A reply
// resolves it (pending cleared).
type Pending = { corr: string; session: string; state: 'sent' | 'thinking' | 'stalled'; text: string };

const uid = () => Math.random().toString(36).slice(2);
const sessionId = () => `assistant-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
const corrId = () => `assistant-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;

function assistantSchemas(tools: ClientTool[]) {
  return tools.map(({ name, description, parameters }) => ({ name, description, parameters }));
}

/** Pure builder for the `in/agent/<agent>` send payload. Exported (and exposed on
 *  `window.__assistantSendPayload`) so the concierge seam can be asserted at the
 *  unit level: a `context`, when present, attaches; when absent it is omitted
 *  entirely (byte-identical to before the seam existed). */
export function buildSendPayload(input: {
  prompt: string;
  session: string;
  profile: string;
  client_tools: any[];
  context?: AssistantContext;
}): Record<string, any> {
  const { prompt, session, profile, client_tools, context } = input;
  const payload: Record<string, any> = { prompt, session, profile, client_tools };
  if (context) payload.context = context;
  return payload;
}
if (typeof window !== 'undefined') {
  (window as any).__assistantSendPayload = buildSendPayload;
}

/** The send-time pre-check (wonky bit 3): generalizing the panel's old world-c
 *  guard, don't publish into a KNOWN void. Returns the honest in-panel line to
 *  show instead of sending (with a setup pointer alongside), or null when a send
 *  is worth attempting. A pure function of health so it is unit-assertable; the
 *  component uses this same function, so the assertion tests the shipped guard.
 *  Absent health ⇒ never blocks (the configure modal, which passes no health). */
export function preSendBlock(health?: SystemHealth): { text: string } | null {
  if (health && !health.brokerConnected) {
    return { text: 'The message bus is not connected, so this cannot be delivered yet. Start the background service, then try again.' };
  }
  if (health && health.llmWorld === 'c') {
    return { text: 'Nothing is running that can answer this yet. Add a model provider (or sign in to a coding CLI) first.' };
  }
  return null;
}
if (typeof window !== 'undefined') {
  (window as any).__assistantPreSend = preSendBlock;
}

function profileAgent(profiles: any[], profile: string) {
  const row = profiles.find((p) => p.profile === profile) ?? profiles.find((p) => p.profile === 'default') ?? profiles[0];
  return row?.agent || profile;
}

function toolTopicParts(topic: string) {
  const m = topic.match(/^obs\/agent\/([^/]+)\/([^/]+)\/tool\/([^/]+)\/(call|result|await)$/);
  return m ? { agent: m[1], session: m[2], tool: m[3], leaf: m[4] } : null;
}

function obsSession(topic: string): string | null {
  const m = topic.match(/^obs\/agent\/[^/]+\/([^/]+)\//);
  return m ? m[1] : null;
}

export default function AgentAssistant({ profile = 'helper', tools, title = 'Assistant', intro, onDone, health, onCheckStatus, persistKey, context }: AgentAssistantProps) {
  const introText = intro || 'Ask me anything about what you see here — I can look things up and take you where you need to go.';
  const [profiles, setProfiles] = useState<any[]>([]);
  const [selectedProfile, setSelectedProfile] = useState(profile);
  // The intro renders as a STATIC first bubble (wonky bit 1) — zero cost, zero
  // side effects, no publish.
  const [items, setItems] = useState<AssistantItem[]>(() => [{ id: uid(), kind: 'assistant', text: introText }]);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  // Session continuity (wonky bit 5): reuse the persisted id on remount so
  // reopening the panel continues the same helper conversation.
  const sessionRef = useRef<string>('');
  if (!sessionRef.current) {
    let sid = '';
    if (persistKey) {
      try { sid = localStorage.getItem(persistKey) || ''; } catch { /* private mode */ }
    }
    if (!sid) {
      sid = sessionId();
      if (persistKey) { try { localStorage.setItem(persistKey, sid); } catch { /* ignore */ } }
    }
    sessionRef.current = sid;
  }
  // busy as a ref too: the async send/retry paths must not read a stale closure.
  const busyRef = useRef(false);
  const setBusyBoth = (v: boolean) => { busyRef.current = v; setBusy(v); };
  const pendingRef = useRef<Pending | null>(null);
  pendingRef.current = pending;
  const stallTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sentCorrs = useRef(new Set<string>());
  const handledCalls = useRef(new Set<string>());
  // Retry is consume-once: two rapid clicks on the stalled line must re-send
  // exactly once (mirrors ConverseView's retriedCorrs guard).
  const retriedCorrs = useRef(new Set<string>());
  const toolsRef = useRef(tools);
  toolsRef.current = tools;

  const schemas = useMemo(() => assistantSchemas(tools), [tools]);
  const agent = profileAgent(profiles, selectedProfile);

  const addItem = (item: AssistantItem) => setItems((prev) => [...prev, item]);
  const patchTool = (callId: string, patch: Partial<Extract<AssistantItem, { kind: 'tool' }>>) => {
    setItems((prev) => prev.map((item) => item.kind === 'tool' && item.callId === callId ? { ...item, ...patch } : item));
  };

  const clearStall = () => {
    if (stallTimer.current) { clearTimeout(stallTimer.current); stallTimer.current = null; }
  };
  const armStall = (corr: string) => {
    clearStall();
    stallTimer.current = setTimeout(() => {
      setPending((p) => p && p.corr === corr && p.state !== 'stalled' ? { ...p, state: 'stalled' } : p);
    }, STALL_MS);
  };
  // Correlated obs = the agent woke up (wonky bit 3): flip sent/stalled → thinking
  // and stand down the stall timer, trusting the wake-up signal.
  const markThinking = (session: string) => {
    setPending((p) => {
      if (!p || p.session !== session || p.state === 'thinking') return p;
      clearStall();
      return { ...p, state: 'thinking' };
    });
  };
  const resolvePending = (corr: string) => {
    setPending((p) => (p && p.corr === corr ? null : p));
    if (pendingRef.current && pendingRef.current.corr === corr) clearStall();
  };

  // The one send path. The user's message ALWAYS renders first — it must stay
  // visible no matter what follows (wonky bit 3). Then the send-time pre-check:
  // don't publish into a known void.
  const send = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    addItem({ id: uid(), kind: 'user', text: trimmed });
    const block = preSendBlock(health);
    if (block) {
      addItem({ id: uid(), kind: 'nopath', text: block.text });
      return;
    }
    setBusyBoth(true);
    const corr = corrId();
    sentCorrs.current.add(corr);
    const session = sessionRef.current;
    setPending({ corr, session, state: 'sent', text: trimmed });
    armStall(corr);
    const payload = buildSendPayload({ prompt: trimmed, session, profile: selectedProfile, client_tools: schemas, context });
    const ok = await publish(`in/agent/${agent}`, payload, corr);
    if (!ok) {
      addItem({ id: uid(), kind: 'nopath', text: 'The message could not be sent. Check that the background service is running, then try again.' });
      setBusyBoth(false);
      resolvePending(corr);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busyRef.current) return;
    const text = draft;
    setDraft('');
    void send(text);
  };

  // Stop (wonky bit 4): end the wait locally. There is no cancel primitive for a
  // helper turn on the bus, so this is honest local-stop — the corr stays in
  // sentCorrs so a late reply can still render. Never leaves `busy` wedged.
  const stop = () => {
    clearStall();
    setBusyBoth(false);
    setPending(null);
    addItem({ id: uid(), kind: 'status', text: 'Stopped waiting — the agent may still finish in the background.' });
  };

  // Retry (wonky bit 3): re-send the stalled text under a fresh corr. Consume-once.
  const retry = () => {
    const p = pendingRef.current;
    if (!p || retriedCorrs.current.has(p.corr)) return;
    retriedCorrs.current.add(p.corr);
    clearStall();
    setPending(null);
    setBusyBoth(false);
    void send(p.text);
  };

  // New conversation (wonky bit 5): deliberately rotate the session id and clear
  // the feed back to the intro. Ends any in-flight wait first.
  const newConversation = () => {
    clearStall();
    setBusyBoth(false);
    setPending(null);
    sentCorrs.current.clear();
    handledCalls.current.clear();
    retriedCorrs.current.clear();
    const fresh = sessionId();
    sessionRef.current = fresh;
    if (persistKey) { try { localStorage.setItem(persistKey, fresh); } catch { /* ignore */ } }
    setItems([{ id: uid(), kind: 'assistant', text: introText }]);
  };

  useEffect(() => {
    let alive = true;
    (async () => {
      const j = await adminGet('agents');
      if (alive && j.ok) setProfiles(j.profiles ?? []);
    })();
    return () => { alive = false; };
  }, []);

  useEffect(() => () => clearStall(), []);

  useEffect(() => {
    const stream = openLiveStream((event) => {
      if (event.kind !== 'message') return;
      const { topic, env } = event;
      const payload = env?.payload && typeof env.payload === 'object' ? env.payload : {};
      const corr = env?.correlation_id;
      if (topic.startsWith('in/human/') && corr && sentCorrs.current.has(corr)) {
        setBusyBoth(false);
        resolvePending(corr);
        if (payload.failed) addItem({ id: uid(), kind: 'assistant', text: payload.error || 'the assistant failed' });
        else if (typeof payload.text === 'string') addItem({ id: uid(), kind: 'assistant', text: payload.text });
        else if (payload.question) addItem({ id: uid(), kind: 'assistant', text: payload.question });
        return;
      }
      // Any obs telemetry on our session is the wake-up signal → thinking.
      if (topic.startsWith('obs/agent/')) {
        const s = obsSession(topic);
        if (s && s === sessionRef.current) markThinking(s);
      }
      const parts = toolTopicParts(topic);
      if (!parts || parts.session !== sessionRef.current) return;
      const callId = payload.call_id;
      if (!callId) return;
      if (parts.leaf === 'result') {
        patchTool(callId, { result: payload.result, error: payload.error, status: payload.error ? 'error' : 'done' });
        return;
      }
      if (parts.leaf !== 'call' || handledCalls.current.has(callId)) return;
      handledCalls.current.add(callId);
      const name = payload.name || parts.tool;
      const args = payload.args ?? {};
      addItem({ id: uid(), kind: 'tool', callId, name, args, status: 'running' });
      const tool = toolsRef.current.find((t) => t.name === name);
      (async () => {
        let result: any = null;
        let error = '';
        try {
          if (!tool) throw new Error(`unknown client tool ${name}`);
          result = await tool.handler(args);
          patchTool(callId, { result, status: 'done' });
        } catch (err: any) {
          error = String(err?.message ?? err);
          patchTool(callId, { error, status: 'error' });
        }
        const resultTopic = topic.replace(/\/call$/, '/result');
        await publish(resultTopic, error ? { call_id: callId, name, error } : { call_id: callId, name, result });
      })();
    }, () => undefined);
    return () => stream.close();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="agent-assistant" data-session={sessionRef.current}>
      <div className="agent-assistant-head">
        <div>
          <h3>{title}</h3>
        </div>
        <div className="agent-assistant-head-actions">
          {busy && <button type="button" className="ghost" data-sel="assistant-stop" onClick={stop}>stop</button>}
          <button type="button" className="ghost" data-sel="assistant-new" title="start a fresh conversation" onClick={newConversation}>new conversation</button>
          <label>agent
            <select value={selectedProfile} onChange={(e) => setSelectedProfile(e.target.value)}>
              {profiles.map((p) => <option key={p.profile} value={p.profile}>{p.profile}{p.mirrors ? ` (mirrors ${p.mirrors})` : ''}</option>)}
              {!profiles.some((p) => p.profile === selectedProfile) && <option value={selectedProfile}>{selectedProfile}</option>}
            </select>
          </label>
        </div>
      </div>
      <div className="agent-assistant-feed" aria-live="polite">
        {items.map((item) => item.kind === 'tool' ? (
          <details key={item.id} className={`agent-assistant-tool ${item.status}`} open>
            <summary>{item.name} <span>{item.status}</span></summary>
            <pre>{JSON.stringify({ args: item.args, result: item.result, error: item.error }, null, 2)}</pre>
          </details>
        ) : item.kind === 'nopath' ? (
          <div key={item.id} className="agent-assistant-nopath" data-sel="assistant-nopath">
            <span>{item.text}</span>
            {onCheckStatus && <button type="button" className="ghost" data-sel="assistant-nopath-setup" onClick={onCheckStatus}>go to setup →</button>}
          </div>
        ) : (
          <div key={item.id} className={`agent-assistant-msg ${item.kind}`}>{item.text}</div>
        ))}
        {/* Per-message dead-air marks (wonky bit 3), the same vocabulary as the
            main chat: a subtle "sent" mark, a thinking indicator on obs activity,
            and after 20s of silence the honest "No response yet…" line. */}
        {pending && pending.state === 'sent' && (
          <div className="agent-assistant-sent" data-sel="assistant-sent" aria-live="polite">sent</div>
        )}
        {pending && pending.state === 'thinking' && (
          <div className="agent-assistant-thinking" data-sel="assistant-thinking" aria-live="polite">
            <span className="thinking-dots" aria-hidden="true"><i /><i /><i /></span>
            <span className="dim-inline">thinking…</span>
          </div>
        )}
        {pending && pending.state === 'stalled' && (
          <div className="agent-assistant-stalled" data-sel="assistant-stalled" role="status">
            <p className="agent-assistant-stalled-note">No response yet. The agent may not be running.</p>
            <div className="agent-assistant-stalled-actions">
              {onCheckStatus && <button type="button" className="ghost" data-sel="assistant-stalled-status" onClick={onCheckStatus}>check status</button>}
              <button type="button" className="agent-assistant-stalled-retry" data-sel="assistant-stalled-retry" onClick={retry}>retry</button>
            </div>
          </div>
        )}
      </div>
      <form className="agent-assistant-compose" onSubmit={submit}>
        <input value={draft} onChange={(e) => setDraft(e.target.value)} placeholder={busy ? 'waiting for reply...' : 'message the assistant'} />
        <button type="submit" disabled={busy || !draft.trim()}>send</button>
        {onDone && <button type="button" className="ghost" onClick={onDone}>done</button>}
      </form>
    </div>
  );
}
