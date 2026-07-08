import { FormEvent, useEffect, useMemo, useRef, useState } from 'react';
import { adminGet, publish } from '../api';
import { openLiveStream } from '../live';

export type ClientTool = {
  name: string;
  description: string;
  parameters: Record<string, any>;
  handler: (args: any) => Promise<any>;
};

type AgentAssistantProps = {
  profile?: string;
  tools: ClientTool[];
  title?: string;
  intro?: string;
  onDone?: () => void;
};

type AssistantItem =
  | { id: string; kind: 'user' | 'assistant' | 'status'; text: string }
  | { id: string; kind: 'tool'; callId: string; name: string; args?: any; result?: any; error?: string; status: 'running' | 'done' | 'error' };

const uid = () => Math.random().toString(36).slice(2);
const sessionId = () => `assistant-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
const corrId = () => `assistant-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;

function assistantSchemas(tools: ClientTool[]) {
  return tools.map(({ name, description, parameters }) => ({ name, description, parameters }));
}

function profileAgent(profiles: any[], profile: string) {
  const row = profiles.find((p) => p.profile === profile) ?? profiles.find((p) => p.profile === 'default') ?? profiles[0];
  return row?.agent || profile;
}

function toolTopicParts(topic: string) {
  const m = topic.match(/^obs\/agent\/([^/]+)\/([^/]+)\/tool\/([^/]+)\/(call|result|await)$/);
  return m ? { agent: m[1], session: m[2], tool: m[3], leaf: m[4] } : null;
}

export default function AgentAssistant({ profile = 'helper', tools, title = 'Assistant', intro, onDone }: AgentAssistantProps) {
  const [profiles, setProfiles] = useState<any[]>([]);
  const [selectedProfile, setSelectedProfile] = useState(profile);
  const [items, setItems] = useState<AssistantItem[]>([]);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const sessionRef = useRef(sessionId());
  const started = useRef(false);
  const sentCorrs = useRef(new Set<string>());
  const handledCalls = useRef(new Set<string>());
  const toolsRef = useRef(tools);
  toolsRef.current = tools;

  const schemas = useMemo(() => assistantSchemas(tools), [tools]);
  const agent = profileAgent(profiles, selectedProfile);
  const opening = intro || 'Help with this configuration task. Use the available tools when you need current options or need to save a choice.';

  const addItem = (item: AssistantItem) => setItems((prev) => [...prev, item]);
  const patchTool = (callId: string, patch: Partial<Extract<AssistantItem, { kind: 'tool' }>>) => {
    setItems((prev) => prev.map((item) => item.kind === 'tool' && item.callId === callId ? { ...item, ...patch } : item));
  };

  const sendPrompt = async (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    const corr = corrId();
    sentCorrs.current.add(corr);
    addItem({ id: uid(), kind: 'user', text: trimmed });
    const ok = await publish(
      `in/agent/${agent}`,
      { prompt: trimmed, session: sessionRef.current, profile: selectedProfile, client_tools: schemas },
      corr,
    );
    if (!ok) {
      addItem({ id: uid(), kind: 'status', text: 'send failed' });
      setBusy(false);
    }
  };

  useEffect(() => {
    let alive = true;
    (async () => {
      const j = await adminGet('agents');
      if (alive && j.ok) setProfiles(j.profiles ?? []);
    })();
    return () => { alive = false; };
  }, []);

  useEffect(() => {
    if (!profiles.length || started.current) return;
    started.current = true;
    void sendPrompt(opening);
  }, [profiles.length, selectedProfile]);

  useEffect(() => {
    const stream = openLiveStream((event) => {
      if (event.kind !== 'message') return;
      const { topic, env } = event;
      const payload = env?.payload && typeof env.payload === 'object' ? env.payload : {};
      const corr = env?.correlation_id;
      if (topic.startsWith('in/human/') && corr && sentCorrs.current.has(corr)) {
        setBusy(false);
        if (payload.failed) addItem({ id: uid(), kind: 'assistant', text: payload.error || 'the assistant failed' });
        else if (typeof payload.text === 'string') addItem({ id: uid(), kind: 'assistant', text: payload.text });
        else if (payload.question) addItem({ id: uid(), kind: 'assistant', text: payload.question });
        return;
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
  }, []);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const text = draft;
    setDraft('');
    void sendPrompt(text);
  };

  return (
    <div className="agent-assistant">
      <div className="agent-assistant-head">
        <div>
          <h3>{title}</h3>
          <p className="dim-note">{sessionRef.current}</p>
        </div>
        <label>agent
          <select value={selectedProfile} onChange={(e) => setSelectedProfile(e.target.value)}>
            {profiles.map((p) => <option key={p.profile} value={p.profile}>{p.profile}{p.mirrors ? ` (mirrors ${p.mirrors})` : ''}</option>)}
            {!profiles.some((p) => p.profile === selectedProfile) && <option value={selectedProfile}>{selectedProfile}</option>}
          </select>
        </label>
      </div>
      <div className="agent-assistant-feed" aria-live="polite">
        {items.map((item) => item.kind === 'tool' ? (
          <details key={item.id} className={`agent-assistant-tool ${item.status}`} open>
            <summary>{item.name} <span>{item.status}</span></summary>
            <pre>{JSON.stringify({ args: item.args, result: item.result, error: item.error }, null, 2)}</pre>
          </details>
        ) : (
          <div key={item.id} className={`agent-assistant-msg ${item.kind}`}>{item.text}</div>
        ))}
      </div>
      <form className="agent-assistant-compose" onSubmit={submit}>
        <input value={draft} onChange={(e) => setDraft(e.target.value)} placeholder={busy ? 'waiting for reply...' : 'message the assistant'} />
        <button type="submit" disabled={busy || !draft.trim()}>send</button>
        {onDone && <button type="button" className="ghost" onClick={onDone}>done</button>}
      </form>
    </div>
  );
}
