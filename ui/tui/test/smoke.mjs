// Smoke test: a REAL daemon on a throwaway root, the TUI as a pure MQTT
// client against it. Proves the four paths: receive (stream), ask (asks
// pane), answer (observed by a second MQTT client), compose (PUBACK).
// Run: npm test  (from ui/tui/)
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import React from 'react';
import { render } from 'ink-testing-library';
import mqtt from 'mqtt';
import App from '../app.js';

const h = React.createElement;
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const BIN = path.join(REPO, 'target/debug');
const TMP = fs.mkdtempSync('/tmp/elanus-tui-smoke.');
const PORT = 18000 + (process.pid % 2000);
const URL = `mqtt://127.0.0.1:${PORT}`;
const ENV = { ...process.env, HARNESS_ROOT: TMP, PATH: `${BIN}:${process.env.PATH}` };

let failures = 0;
const ok = (msg) => console.log(`  ok: ${msg}`);
const fail = (msg) => {
  console.error(`FAIL: ${msg}`);
  failures++;
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(desc, fn, timeoutMs = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (await fn()) {
      ok(desc);
      return true;
    }
    await sleep(100);
  }
  fail(`${desc} (timed out)`);
  return false;
}

function elanus(...args) {
  return execFileSync(path.join(BIN, 'elanus'), args, { env: ENV, encoding: 'utf8' });
}

/** Type text one character at a time, verifying each lands in a rendered
 * frame (after `marker`, e.g. "answer> ") before sending the next; retries a
 * character once if no frame ever showed it. Blind chunk writes are flaky
 * under rapid bus-driven re-renders. */
async function typeVerified(marker, text) {
  let prefix = '';
  for (const ch of text) {
    prefix += ch;
    for (let attempt = 0; attempt < 2; attempt++) {
      tui.stdin.write(ch);
      const t0 = Date.now();
      let seen = false;
      while (Date.now() - t0 < 1500) {
        if (frame().includes(marker + prefix)) {
          seen = true;
          break;
        }
        await sleep(25);
      }
      if (seen) break;
    }
  }
}

/** Press a key, polling for `pred`; re-press if a frame never reflected it. */
async function pressUntil(desc, key, pred, attempts = 3) {
  for (let i = 0; i < attempts; i++) {
    tui.stdin.write(key);
    const t0 = Date.now();
    while (Date.now() - t0 < 2000) {
      if (pred()) {
        ok(desc);
        return true;
      }
      await sleep(50);
    }
  }
  fail(`${desc} (no frame reflected the keypress)`);
  return false;
}

// -- build the binary if missing (cargo build, e2e-style PATH) --
if (!fs.existsSync(path.join(BIN, 'elanus'))) {
  console.log('building elanus...');
  execFileSync('cargo', ['build'], { cwd: REPO, stdio: 'inherit' });
}

// -- throwaway root + daemon (the e2e.sh recipe) --
elanus('init', TMP);
fs.writeFileSync(path.join(TMP, 'bus.toml'), `enabled = true\nbind = "127.0.0.1:${PORT}"\n`);
const daemonLog = fs.openSync(path.join(TMP, 'daemon.log'), 'w');
const daemon = spawn(path.join(BIN, 'elanus'), ['daemon'], {
  env: ENV,
  stdio: ['ignore', daemonLog, daemonLog],
});

let tui = null;
let observer = null;
process.on('exit', () => {
  try {
    tui?.unmount();
  } catch {}
  observer?.end(true);
  daemon.kill();
});

await waitFor(
  'daemon listener bound',
  () => fs.readFileSync(path.join(TMP, 'daemon.log'), 'utf8').includes(`mqtt listener on 127.0.0.1:${PORT}`)
);

// -- a second, independent MQTT client to observe what the TUI publishes --
// Both the observer and the TUI act as the owner; present the owner
// credential (minted at init) so they are accepted once anonymous is denied.
const humanSecret = fs.readFileSync(path.join(TMP, '.secrets', 'owner'), 'utf8').trim();
const observed = [];
observer = mqtt.connect(URL, { protocolVersion: 5, clean: true, clientId: `el-tui-observer-${process.pid}`, username: 'owner', password: humanSecret });
await new Promise((resolve, reject) => {
  observer.on('connect', () => observer.subscribe({ 'in/agent/#': { qos: 1 } }, resolve));
  observer.on('error', reject);
});
observer.on('message', (topic, payload) => {
  try {
    observed.push({ topic, env: JSON.parse(payload.toString()) });
  } catch {
    observed.push({ topic, env: null });
  }
});

// -- the TUI under test: ink-testing-library drives a real render --
tui = render(h(App, { url: URL, agent: 'main', root: TMP }));
const frame = () => tui.lastFrame() ?? '';
await waitFor('tui connected', () => frame().includes('● connected'));

// 1. receive path: a published event appears in the stream pane
elanus('bus', 'pub', 'obs/test/tui', '{"msg":"tui-smoke"}');
await waitFor('stream shows obs/test/tui', () => frame().includes('obs/test/tui') && frame().includes('tui-smoke'));

// 2. ask path: an ask (with correlation, like `elanus ask`) lands in the asks pane
elanus('emit', 'in/human/owner', '--correlation', 'tui-corr-1', '--payload', '{"question":"ship it?","options":["yes","no"]}');
// The ask reaches the TUI via the daemon's announce sweep, a tick AFTER the
// instant obs/harness/ledger/emit echo hits the stream pane — so the guard
// must check the asks-pane header, not just grep the frame for the question
// (that false-positives on the stream line + the "(0 pending)" header).
await waitFor('ask landed in asks pane', () => frame().includes('asks (1 pending)'));

// 3. answer path: tab to asks, enter to edit, type, enter to publish;
//    the second client must observe the in/agent/main publish.
tui.stdin.write('\t'); // stream -> asks
await waitFor('asks pane focused', () => frame().includes('↑/↓ select'));
tui.stdin.write('\r'); // start answering selected ask
await waitFor('answer editor open', () => frame().includes('answer>'));
await typeVerified('answer> ', 'yes');
await waitFor('typed answer visible', () => frame().includes('answer> yes'));
tui.stdin.write('\r'); // publish
await waitFor('observer saw the answer on in/agent/main', () =>
  observed.some((m) => m.topic === 'in/agent/main' && m.env?.payload?.answer === 'yes')
);
// The answer must carry the ask's correlation INTO THE LEDGER (that's what
// resumes a suspended asker) — the broker materializes the el-correlation
// user property and echoes correlation_id on the announced line.
await waitFor('answer carries correlation into the ledger', () =>
  observed.some(
    (m) =>
      m.topic === 'in/agent/main' &&
      m.env?.payload?.answer === 'yes' &&
      m.env?.correlation_id === 'tui-corr-1'
  )
);
await waitFor('ask marked answered in pane', () => frame().includes('answered: yes'));

// 4. CLI-answer reflection: an in/agent/# event with a pending ask's
//    correlation (published by anyone) marks it answered in the pane.
elanus('emit', 'in/human/owner', '--correlation', 'tui-corr-2', '--payload', '{"question":"second ask?"}');
await waitFor('second ask pending', () => frame().includes('second ask?'));
elanus('emit', 'in/agent/main', '--correlation', 'tui-corr-2', '--payload', '{"answer":"from-cli"}');
await waitFor('CLI answer reflected', () => frame().includes('answered: from-cli'));

// 4b. agent REPLIES are human mail too ({text}, not {question}) — they render
// in the pane as un-answerable items and never open the editor.
elanus('emit', 'in/human/owner', '--correlation', 'tui-conv-9', '--payload', '{"text":"here is your answer"}');
await waitFor('agent reply rendered as mail', () => frame().includes('here is your answer'));

// 5. compose path: publish new work, PUBACK = accepted indicator
tui.stdin.write('\t'); // asks -> compose
await waitFor('compose focused', () => frame().includes('prompt>'));
await typeVerified('prompt> ', 'do the thing');
await waitFor('typed prompt visible', () => frame().includes('prompt> do the thing'));
tui.stdin.write('\r');
await waitFor('compose accepted (PUBACK)', () => frame().includes('accepted ✓'));
await waitFor('observer saw composed work on in/agent/main', () =>
  observed.some((m) => m.topic === 'in/agent/main' && m.env?.payload?.prompt === 'do the thing')
);

// 6. signal loudness sanity: a signal/# event reaches the stream
elanus('bus', 'pub', 'signal/pain', '{"why":"smoke"}');
await waitFor('signal in stream', () => frame().includes('signal/pain'));

if (failures === 0) {
  console.log('PASS — receive, ask, answer, CLI-answer reflection, compose, signal');
  process.exit(0);
} else {
  console.error(`${failures} failure(s)\n--- last frame ---\n${frame()}`);
  process.exit(1);
}
