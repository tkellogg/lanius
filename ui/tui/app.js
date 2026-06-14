// elanus TUI — a PURE MQTT 5 client on the loopback listener.
// No sqlite, no trace.jsonl, no privilege: everything on screen arrived
// over the bus, and everything we do is an ordinary QoS 1 publish.
import React, { useEffect, useRef, useState } from 'react';
import { Box, Text, useApp, useInput } from 'ink';
import mqtt from 'mqtt';
import fs from 'node:fs';
import path from 'node:path';

const h = React.createElement;

// The TUI is a human surface: present the owner identity from the fenced
// store so the broker stamps its events as the owner (docs/identity.md — the
// principal is an identity, default "owner", not the role "human").
// Matches src/secrets.rs valid_principal — keep in sync (see ui/web/server.mjs).
function validPrincipal(name) {
  return !!name && name.length <= 64 && !name.startsWith('.') && !name.includes('/') && !name.includes('\\');
}
function ownerName(root) {
  const env = (process.env.ELANUS_OWNER || '').trim();
  if (validPrincipal(env)) return env;
  try {
    const n = fs.readFileSync(path.join(root, '.secrets', '.owner-name'), 'utf8').trim();
    return validPrincipal(n) ? n : 'owner';
  } catch {
    return 'owner';
  }
}
function humanCredential(root) {
  if (!root) return {};
  try {
    const username = ownerName(root);
    const secret = fs.readFileSync(path.join(root, '.secrets', username), 'utf8').trim();
    return secret ? { username, password: secret } : {};
  } catch {
    return {};
  }
}

const STREAM_CAP = 500; // ring buffer of parsed lines
const STREAM_ROWS = 14; // lines shown in the stream pane

const FILTERS = {
  a: { label: 'all', match: () => true },
  t: {
    // tool calls anywhere: obs/+/+/+/tool/#
    label: 'tools',
    match: (topic) => {
      const s = topic.split('/');
      return s[0] === 'obs' && s.length >= 5 && s[4] === 'tool';
    },
  },
  w: { label: 'work', match: (topic) => topic === 'in' || topic.startsWith('in/') },
  s: { label: 'signals', match: (topic) => topic === 'signal' || topic.startsWith('signal/') },
};

const PANES = ['stream', 'asks', 'compose'];

/** Parse a bus message into the envelope shape the broker actually emits:
 * {ts, kind, event_id, payload, correlation_id?, cause_id?}.
 * (Verified on the wire 2026-06-11; non-envelope payloads degrade gracefully.) */
export function parseEnvelope(buf) {
  let v;
  try {
    v = JSON.parse(buf.toString('utf8'));
  } catch {
    return { payload: buf.toString('utf8') };
  }
  if (v && typeof v === 'object' && ('ts' in v || 'kind' in v || 'event_id' in v)) return v;
  return { payload: v };
}

/** One-line payload summary for the stream pane. */
export function summarize(payload, max = 100) {
  let s;
  if (payload === undefined || payload === null) s = '';
  else if (typeof payload === 'string') s = payload;
  else s = JSON.stringify(payload);
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
}

function timeOf(env) {
  const d = env.ts ? new Date(env.ts) : new Date();
  return Number.isNaN(d.getTime()) ? '--:--:--' : d.toTimeString().slice(0, 8);
}

let seq = 0;

export default function App({ url, agent = 'main', root = null }) {
  const { exit } = useApp();
  const clientRef = useRef(null);
  const [status, setStatus] = useState('connecting');
  const [events, setEvents] = useState([]);
  const [filterKey, setFilterKey] = useState('a');
  const [asks, setAsks] = useState([]); // {corr, question, options, deadline, status, answer}
  const [pane, setPane] = useState('stream');
  const [sel, setSel] = useState(0);
  const [editing, setEditing] = useState(false); // answering a selected ask
  const [buffer, setBuffer] = useState(''); // shared input buffer (answer or compose)
  const [composeNote, setComposeNote] = useState('');

  useEffect(() => {
    const client = mqtt.connect(url, {
      protocolVersion: 5,
      clean: true,
      clientId: `el-tui-${process.pid}`,
      reconnectPeriod: 1000,
      connectTimeout: 5000,
      ...humanCredential(root),
    });
    clientRef.current = client;
    client.on('connect', () => {
      setStatus('connected');
      client.subscribe({
        'obs/#': { qos: 0 },
        'in/#': { qos: 1 },
        'signal/#': { qos: 1 },
      });
    });
    client.on('reconnect', () => setStatus('reconnecting'));
    client.on('close', () => setStatus((s) => (s === 'connected' ? 'disconnected' : s)));
    client.on('error', (e) => setStatus(`error: ${e.message}`));
    client.on('message', (topic, payload) => {
      const env = parseEnvelope(payload);
      setEvents((prev) => {
        const next = prev.concat({
          key: ++seq,
          topic,
          time: timeOf(env),
          summary: summarize(env.payload),
          loud: topic === 'signal' || topic.startsWith('signal/'),
        });
        return next.length > STREAM_CAP ? next.slice(next.length - STREAM_CAP) : next;
      });
      if (topic.startsWith('in/human/')) {
        // Human mail is two kinds: an ASK ({question}, wants an answer) or a
        // REPLY ({text} — the agent's conversation turn, mail per the mailbox
        // model). The wire envelope carries no deadline/default (ledger-only
        // fields today); show them if they ever appear.
        const corr = env.correlation_id;
        const p = env.payload && typeof env.payload === 'object' ? env.payload : {};
        if (!p.question && typeof p.text === 'string') {
          setAsks((prev) =>
            prev.concat({
              k: `reply-${seq}`, // replies share corr with their compose; key must be unique
              corr: corr ?? `reply-${seq}`,
              question: p.text,
              options: null,
              deadline: null,
              status: 'reply', // never answerable, never nagged
              answer: null,
            })
          );
        } else if (corr) {
          setAsks((prev) =>
            prev.some((a) => a.corr === corr)
              ? prev
              : prev.concat({
                  corr,
                  question: p.question ?? summarize(env.payload, 60),
                  options: Array.isArray(p.options) ? p.options : null,
                  deadline: p.deadline ?? env.deadline ?? null,
                  status: 'pending',
                  answer: null,
                })
          );
        }
      } else if (topic.startsWith('in/agent/')) {
        // Someone answered (CLI or another client): correlation closes the ask.
        const corr =
          env.correlation_id ??
          (env.payload && typeof env.payload === 'object' ? env.payload.correlation_id : null);
        if (corr) {
          const ans =
            env.payload && typeof env.payload === 'object' ? env.payload.answer : undefined;
          setAsks((prev) =>
            prev.map((a) =>
              a.corr === corr && a.status !== 'answered' && a.status !== 'reply'
                ? { ...a, status: 'answered', answer: ans ?? a.answer }
                : a
            )
          );
        }
      }
    });
    return () => client.end(true);
  }, [url]);

  const pendingAsks = asks; // answered asks stay visible, greyed

  const publishAnswer = (ask, text) => {
    // Mirror `elanus answer` (src/human.rs): mail to the agent's mailbox,
    // payload {answer}. The envelope correlation rides the el-correlation
    // user property — the broker materializes it into the ledger event's
    // correlation_id, which is what resumes the suspended asker. (MQTT
    // Correlation Data is reserved for the hook round trip; topics.md.)
    clientRef.current?.publish(
      `in/agent/${agent}`,
      JSON.stringify({ answer: text }),
      { qos: 1, properties: { userProperties: { 'el-correlation': ask.corr } } },
      (err) => {
        setAsks((prev) =>
          prev.map((a) =>
            a.corr === ask.corr
              ? err
                ? { ...a, status: 'send failed' }
                : { ...a, status: 'answered', answer: text }
              : a
          )
        );
      }
    );
    setAsks((prev) => prev.map((a) => (a.corr === ask.corr ? { ...a, status: 'sending' } : a)));
  };

  const publishWork = (text) => {
    setComposeNote('sending…');
    // Correlate the work so the conversation threads: the agent's reply
    // comes back as in/human/# mail carrying this same correlation.
    const conv = `tui-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    clientRef.current?.publish(
      `in/agent/${agent}`,
      JSON.stringify({ prompt: text }),
      { qos: 1, properties: { userProperties: { 'el-correlation': conv } } },
      (err) => setComposeNote(err ? `rejected: ${err.message}` : 'accepted ✓ (PUBACK)')
    );
  };

  useInput((input, key) => {
    const typing = pane === 'compose' || (pane === 'asks' && editing);
    if (key.tab) {
      setPane((p) => PANES[(PANES.indexOf(p) + 1) % PANES.length]);
      setEditing(false);
      setBuffer('');
      return;
    }
    if (typing) {
      if (key.escape) {
        setEditing(false);
        setBuffer('');
        return;
      }
      if (key.return) {
        const text = buffer.trim();
        if (text) {
          if (pane === 'compose') publishWork(text);
          else if (pendingAsks[sel]) publishAnswer(pendingAsks[sel], text);
        }
        setEditing(false);
        setBuffer('');
        return;
      }
      if (key.backspace || key.delete) {
        setBuffer((b) => b.slice(0, -1));
        return;
      }
      if (input && !key.ctrl && !key.meta) setBuffer((b) => b + input);
      return;
    }
    if (input === 'q') {
      clientRef.current?.end(true);
      exit();
      return;
    }
    if (FILTERS[input]) {
      setFilterKey(input);
      return;
    }
    if (pane === 'asks') {
      if (key.upArrow || input === 'k') setSel((s) => Math.max(0, s - 1));
      else if (key.downArrow || input === 'j') setSel((s) => Math.min(pendingAsks.length - 1, s + 1));
      else if (key.return && pendingAsks[sel] && pendingAsks[sel].status === 'pending') {
        setEditing(true);
        setBuffer('');
      }
    }
  });

  const filter = FILTERS[filterKey];
  const visible = events.filter((e) => filter.match(e.topic)).slice(-STREAM_ROWS);

  return h(
    Box,
    { flexDirection: 'column' },
    // status line
    h(
      Box,
      null,
      h(Text, { color: status === 'connected' ? 'green' : 'yellow' }, `● ${status}`),
      h(Text, { dimColor: true }, `  ${url}  agent:${agent}  filter:${filter.label}`),
      h(Text, { dimColor: true }, '  [q quit · tab pane · a/t/w/s filter]')
    ),
    // stream pane
    h(
      Box,
      { flexDirection: 'column', borderStyle: 'round', borderColor: pane === 'stream' ? 'cyan' : 'gray' },
      h(Text, { bold: pane === 'stream' }, `stream (${filter.label})`),
      visible.length === 0
        ? h(Text, { dimColor: true }, '… waiting for events')
        : visible.map((e) =>
            h(
              Text,
              { key: e.key, color: e.loud ? 'red' : undefined, bold: e.loud, wrap: 'truncate-end' },
              `${e.time}  ${e.topic}  ${e.summary}`
            )
          )
    ),
    // asks pane
    h(
      Box,
      { flexDirection: 'column', borderStyle: 'round', borderColor: pane === 'asks' ? 'cyan' : 'gray' },
      h(Text, { bold: pane === 'asks' }, `asks (${asks.filter((a) => a.status === 'pending').length} pending)`),
      asks.length === 0
        ? h(Text, { dimColor: true }, 'no asks — inbox zero')
        : asks.map((a, i) =>
            h(
              Text,
              {
                key: a.k ?? a.corr,
                dimColor: a.status === 'answered',
                color: pane === 'asks' && i === sel ? 'cyan' : undefined,
                wrap: 'truncate-end',
              },
              `${pane === 'asks' && i === sel ? '>' : ' '} ${a.question}` +
                (a.options ? `  [${a.options.join(' | ')}]` : '') +
                (a.deadline ? `  ⏱ ${a.deadline}` : '') +
                `  (${a.status}${a.status === 'answered' && a.answer ? `: ${a.answer}` : ''})`
            )
          ),
      pane === 'asks' && editing
        ? h(Text, null, `answer> ${buffer}▌`)
        : pane === 'asks'
          ? h(Text, { dimColor: true }, '↑/↓ select · enter answer')
          : null
    ),
    // compose pane
    h(
      Box,
      { flexDirection: 'column', borderStyle: 'round', borderColor: pane === 'compose' ? 'cyan' : 'gray' },
      h(Text, { bold: pane === 'compose' }, `compose → in/agent/${agent}`),
      pane === 'compose'
        ? h(Text, null, `prompt> ${buffer}▌`)
        : h(Text, { dimColor: true }, 'tab here to publish new work'),
      composeNote ? h(Text, { color: composeNote.startsWith('accepted') ? 'green' : 'yellow' }, composeNote) : null
    )
  );
}
