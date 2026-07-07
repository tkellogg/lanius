#!/usr/bin/env node
// lanius TUI entry point. Run: node ui/tui/index.js --root /tmp/lanius-live
import React from 'react';
import { render } from 'ink';
import App from './app.js';
import { brokerUrl, parseArgs } from './config.js';

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  console.log(`lanius tui — pure MQTT 5 client for the lanius bus

usage: node ui/tui/index.js [--root <harness-root>] [--url mqtt://host:port] [--agent <noun>]

broker discovery: --url wins; else <root>/bus.toml with root from --root or $LANIUS_ROOT.
keys: q quit · tab cycle panes · a/t/w/s stream filters · ↑/↓ + enter in asks pane`);
  process.exit(0);
}

let url;
try {
  url = brokerUrl(args);
} catch (e) {
  console.error(e.message);
  process.exit(1);
}

render(React.createElement(App, { url, agent: args.agent ?? 'main', root: args.root ?? process.env.LANIUS_ROOT ?? process.env.HARNESS_ROOT ?? null }));
