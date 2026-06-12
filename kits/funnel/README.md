# funnel kit — the variety ladder, pointed at a firehose

Point this at a stream of text lines and let each rung absorb what it can.
Only the residue costs cheap-model tokens; only the residue of THAT costs
human attention.

```
*.line files in funnel-intake's scratch inbox
  │  one event per line, QoS 1, delete-after-PUBACK
  ▼
in/package/funnel/sift          (every arrival ledgered)
  │
  ├─ funnel-sift: regex rules, first match wins, DEFAULT DROP   $0, 0 tokens
  │    dropped → absorbed here (verdict on stdout, exit on obs/)
  ▼  pass
in/agent/scout                  (uncorrelated emit — no reply mail)
  │
  ├─ funnel-scout: one cheap-model run per survivor             cheap tokens
  │    DROP → absorbed here (transcript on obs/agent/scout/...)
  ▼  KEEP — the scout itself emits
in/human/owner                  mail: "KEEP: <item> — <reason>"  your attention
```

## Setup

1. `elanus init <dir> --kit funnel`
2. Configure the scout's model in `profiles/scout/profile.toml`. It ships
   pointing at `claude-3-5-haiku-latest`; the commented lines show the
   anthropic-compatible-provider convention (DeepSeek-style `base_url` +
   `api_key_env`) if you want this rung even cheaper.
3. Start the daemon (`elanus daemon`), then drop files:

   ```
   printf 'ALERT: disk almost full\n' > <root>/run/pkg-funnel-intake/inbox/x.line
   ```

   Every non-empty line of a `*.line` file becomes one work item. The file
   is deleted only after every line is PUBACKed (at-least-once: a crash
   mid-file re-publishes its earlier lines).

## Tuning the rules (packages/funnel-sift/rules.txt)

Line-oriented: `drop <regex>` or `pass <regex>`, first match wins, Python
regex, case-insensitive, `#` comments. **No match means drop** — the funnel
is default-closed, so write `pass` rules for what you care about; noise is
infinite, interest is enumerable. Put cheap `drop` rules for high-volume
chatter above the `pass` rules so they short-circuit. Editing rules.txt
changes the package's code hash and re-enters review: `elanus approve
funnel-sift` after each edit.

Nothing is silent: dropped lines were ledgered on arrival
(`in/package/funnel/sift`), the sift's verdict goes to its captured stdout
(`run/d<N>.out`) and its exit echoes to `obs/harness/dispatch/exit`.

## How a KEEP reaches you (the escalation choice)

The scout itself emits the mail, via its `emit_event` tool: its system block
instructs it to emit `in/human/owner` with `{"text": "KEEP: <item> —
<reason>"}` before answering KEEP. We chose this over a watcher package
parsing `obs/agent/scout/+/llm/response` because:

- it uses the ladder's own machinery — the agent rung escalating to the
  human rung is exactly what `emit_event` is for, and causality threads
  automatically (the mail's cause chain runs back to the intake line);
- `emit_event` runs inside the agent run, under the `funnel-scout` package's
  whole-agent `publish = ["#"]` grant (the chat-package pattern) — no extra
  package, no transcript-parsing fragility.

The trade: a scout that answers KEEP but forgets the emit escalates nothing.
The verdict is still on the flight recorder (`obs/agent/scout/...`), so the
failure is observable, and the sift's emit is deliberately uncorrelated so
scout chatter can never reach your inbox as reply mail.

## Cost story

- Heartbeats, keepalives, anything unmatched: ledger row + one script run.
  Zero tokens, zero attention.
- Regex survivors: one 2-turn cheap-model run each. The `pass` rules are
  your token budget knob.
- Scout KEEPs: one mail each, original item + one-line reason attached.
  That is the only thing that spends your attention.
