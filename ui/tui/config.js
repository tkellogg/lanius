// Broker discovery — the TUI's one allowed filesystem touch: <root>/bus.toml,
// root from --root or $LANIUS_ROOT (legacy $HARNESS_ROOT), overridable with --url.
import fs from 'node:fs';
import path from 'node:path';

/** Minimal parse of bus.toml: we only need `enabled` and `bind`. */
export function parseBusToml(text) {
  const enabled = !/^\s*enabled\s*=\s*false\s*$/m.test(text);
  const m = text.match(/^\s*bind\s*=\s*"([^"]+)"\s*$/m);
  return { enabled, bind: m ? m[1] : '127.0.0.1:1883' };
}

export function brokerUrl({ root, url } = {}) {
  if (url) return url;
  const r = root ?? process.env.LANIUS_ROOT ?? process.env.HARNESS_ROOT;
  if (!r) throw new Error('no broker: pass --url, --root, or set LANIUS_ROOT');
  const file = path.join(r, 'bus.toml');
  let cfg = { enabled: true, bind: '127.0.0.1:1883' };
  if (fs.existsSync(file)) cfg = parseBusToml(fs.readFileSync(file, 'utf8'));
  if (!cfg.enabled) throw new Error(`bus is disabled in ${file}`);
  const i = cfg.bind.lastIndexOf(':');
  let host = i === -1 ? cfg.bind : cfg.bind.slice(0, i);
  const port = i === -1 ? '1883' : cfg.bind.slice(i + 1);
  if (host === '0.0.0.0' || host === '::') host = '127.0.0.1';
  return `mqtt://${host}:${port}`;
}

export function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--root') out.root = argv[++i];
    else if (a === '--url') out.url = argv[++i];
    else if (a === '--agent') out.agent = argv[++i];
    else if (a === '--help' || a === '-h') out.help = true;
  }
  return out;
}
