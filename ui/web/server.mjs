// elanus web dashboard server.
//
// The PURE-MQTT-CLIENT constraint from ui/tui carries over, one hop removed:
// browsers cannot speak raw TCP MQTT, so this process is the ordinary
// anonymous loopback MQTT 5 client, and the browser talks to *it* — bus
// messages relayed over SSE, publishes accepted over POST. No sqlite, no
// trace.jsonl, no privileged access; the only filesystem touches are this
// directory's static files and <root>/bus.toml for broker discovery.
//
//   node server.mjs --root /tmp/elanus-live [--port 7180] [--agent main]
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import mqtt from 'mqtt';
import { brokerUrl, parseArgs } from './config.mjs';

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  console.log('usage: node server.mjs [--root <harness root>] [--url mqtt://...] [--port 7180] [--agent main]');
  process.exit(0);
}
const BROKER = brokerUrl(args);
const PORT = args.port ?? Number(process.env.ELANUS_WEB_PORT ?? 7180);
const AGENT = args.agent ?? 'main';
const PUB = path.join(path.dirname(fileURLToPath(import.meta.url)), 'public');

// ---- bus side -------------------------------------------------------------
const RING_CAP = 1000; // late-joining browsers get recent history
const ring = [];
let seq = 0;
const sseClients = new Set();

const client = mqtt.connect(BROKER, {
  protocolVersion: 5,
  clean: true,
  clientId: `el-web-${process.pid}`,
  reconnectPeriod: 2000,
});
let connected = false;
client.on('connect', () => {
  connected = true;
  client.subscribe({ 'obs/#': { qos: 0 }, 'in/#': { qos: 1 }, 'signal/#': { qos: 1 } });
  broadcast({ kind: 'status', connected: true, broker: BROKER });
});
client.on('close', () => {
  connected = false;
  broadcast({ kind: 'status', connected: false, broker: BROKER });
});
client.on('message', (topic, payload) => {
  let env;
  try {
    env = JSON.parse(payload.toString('utf8'));
  } catch {
    env = { payload: payload.toString('utf8') };
  }
  const msg = { kind: 'message', seq: ++seq, topic, env };
  ring.push(msg);
  if (ring.length > RING_CAP) ring.shift();
  broadcast(msg);
});

function broadcast(obj) {
  const line = `data: ${JSON.stringify(obj)}\n\n`;
  for (const res of sseClients) res.write(line);
}

// ---- http side ------------------------------------------------------------
const MIME = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.mjs': 'text/javascript', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/api/stream') {
    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
      connection: 'keep-alive',
    });
    // catch-up: status first, then the ring
    res.write(`data: ${JSON.stringify({ kind: 'status', connected, broker: BROKER, agent: AGENT })}\n\n`);
    for (const m of ring) res.write(`data: ${JSON.stringify(m)}\n\n`);
    sseClients.add(res);
    req.on('close', () => sseClients.delete(res));
    return;
  }
  if (url.pathname === '/api/publish' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      let j;
      try {
        j = JSON.parse(body);
      } catch {
        res.writeHead(400).end('bad json');
        return;
      }
      const { topic, payload, correlation } = j;
      if (typeof topic !== 'string' || !topic.length || topic.includes('#') || topic.includes('+')) {
        res.writeHead(400).end('bad topic');
        return;
      }
      const props = correlation ? { userProperties: { 'el-correlation': String(correlation) } } : undefined;
      client.publish(topic, JSON.stringify(payload ?? {}), { qos: 1, properties: props }, (err) => {
        if (err) res.writeHead(502, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: false, error: String(err.message ?? err) }));
        else res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ ok: true }));
      });
    });
    return;
  }
  // static
  let file = url.pathname === '/' ? '/index.html' : url.pathname;
  file = path.normalize(file).replace(/^(\.\.[/\\])+/, '');
  const full = path.join(PUB, file);
  if (!full.startsWith(PUB) || !fs.existsSync(full) || !fs.statSync(full).isFile()) {
    res.writeHead(404).end('not found');
    return;
  }
  res.writeHead(200, { 'content-type': MIME[path.extname(full)] ?? 'application/octet-stream' });
  fs.createReadStream(full).pipe(res);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`elanus web on http://127.0.0.1:${PORT}  (broker ${BROKER}, agent ${AGENT})`);
});
