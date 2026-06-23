# memory-blocks-demo — a computed block is a vanilla stage

The one-package proof for memory-blocks M3 (docs/handoffs/memory-blocks.md). It
ships a single package, `clock-block`, with two context-pipeline stages:

- **clock** (order 30) appends a `clock` entry to `doc.system` — a *computed
  block*. There is no special block type: it pushes the same `{name, text}` shape
  every static profile block and every persisted memory block uses.
- **clock-reader** (order 40, downstream) reads `doc.system`, finds the `clock`
  entry **by name with no special-casing**, and appends a `clock-echo` entry
  quoting it. It cannot tell — and does not care — that `clock` was computed this
  run versus shipped statically versus persisted in the block store.

Install and approve:

```
elanus kit add memory-blocks-demo
elanus approve clock-block
elanus context render --profile default --session demo
```

The render shows both `clock` and `clock-echo` in the system text, and
`context_build_log` has an `add` row per stage attributed to `clock-block` —
"which component added which block" stays reconstructable.
