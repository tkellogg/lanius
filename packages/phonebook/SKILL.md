# phonebook — who is who, across channels

The phonebook is the directory of **identities**, the **channels** that
reach them, and the **names** they go by. A person (or an agent) is one
stable identity reachable many ways and called many things; the phonebook
records which channel belongs to whom, so you can pull a scattered
conversation into one frame and so the whole fleet shares one answer.

It does **not** solve the matching problem for you — deciding that
`@tim.bsky` on Bluesky and `tim#1234` on Discord are the same person is
your judgement. The phonebook gives you the place to *record* that
judgement, with a confidence and a provenance, and to revise it later.

## Reading — HTTP, `POST /query`

Discover the port from harness state, then post a JSON body with a `kind`:

```sh
PORT=$(python3 -c 'import json,os;print(json.load(open((os.environ.get("ELANUS_ROOT") or os.environ["HARNESS_ROOT"])+"/run/pkg-phonebook/http.json"))["port"])')
curl -s "http://127.0.0.1:$PORT/healthz"
curl -s "http://127.0.0.1:$PORT/query" -d '{"kind":"resolve","channel_kind":"bluesky","address":"@tim.bsky"}'
```

Query kinds:

- `resolve {channel_kind, address}` — the core lookup: which identity does
  this channel belong to? Follows merges. Returns `{found, resolved,
  identity, confidence, provenance}`. `resolved:false` means the channel is
  known but not yet matched to anyone.
- `identity {id}` — one identity with every channel and name that resolves
  to it (including through a merge), plus its merged members.
- `identities {}` — the whole book (id, kind, canonical name, merged_into).
- `channels {resolved?: true|false}` — list channels; `resolved:false` is
  the **work queue** of sightings nobody has matched yet.
- `whois {name}` — who is called this? Names are non-unique, so this can
  return several identities; matches both aliases and canonical names.

## Writing — over the bus, `in/package/phonebook/<op>`

Writes go over the authenticated bus, **not** HTTP, on purpose: the broker
stamps the message with your verified identity, and that becomes the
link's `provenance`. You cannot write a link that claims to come from
someone else — if you are an agent, your proposals are stamped as you.

```sh
elanus bus pub in/package/phonebook/identity '{"id":"tim","kind":"human","canonical":"Tim"}'      --qos 1
elanus bus pub in/package/phonebook/channel  '{"channel_kind":"bluesky","address":"@tim.bsky","identity":"tim","confidence":1.0}' --qos 1
elanus bus pub in/package/phonebook/channel  '{"channel_kind":"discord","address":"tim#1234"}'    --qos 1   # seen, unresolved
elanus bus pub in/package/phonebook/link     '{"channel_kind":"discord","address":"tim#1234","identity":"tim","confidence":0.7}' --qos 1
elanus bus pub in/package/phonebook/alias    '{"identity":"tim","name":"tk"}'                       --qos 1
```

Operations:

- `identity {id?, kind, canonical}` — create or update an identity. `kind`
  is `human|agent|script|external`. Omit `id` to mint one.
- `channel {channel_kind, address, identity?, confidence?}` — record a
  channel sighting. **Omit `identity`** to log it unresolved (the honest
  thing to do when you do not yet know who it is); add it later with
  `link`.
- `link {channel_kind, address, identity, confidence?}` — attach a channel
  to an identity with a confidence (0..1, default 0.5).
- `alias {identity, name, context?}` — record a name for an identity.
- `merge {from, into}` — declare two identities are one. Non-destructive:
  `from`'s channels and names now resolve to `into`, but nothing is
  deleted, so a `split` can undo it.
- `split {id}` — undo a merge: `id` becomes its own identity again and its
  original channels resolve back to it.

`elanus bus pub` returns as soon as the broker accepts the event onto the
ledger — that is *not* the op's outcome. The phonebook runs the op and fans
its result out on `obs/package/phonebook/result` as `{ok, op, by, correlation,
...}` (errors too, with `{ok:false, error}`), echoing the request's
correlation id. To see whether a write actually succeeded, **subscribe before
you publish**:

```sh
elanus bus sub obs/package/phonebook/result --count 1 --timeout 5 &
elanus bus pub in/package/phonebook/link '{"channel_kind":"discord","address":"tim#1234","identity":"tim","confidence":0.7}' --qos 1
wait   # prints {ok:true,...} or {ok:false,error:...}
```

A malformed write (bad confidence, unknown identity, wrong field type) is
answered with `{ok:false, error}` on that channel — it never crashes the
daemon. Two things to know about writes:

- **Authenticated, not authorized.** Any actor that may publish here can
  create/merge/split/overwrite identities; the protection is that provenance
  is the broker-verified sender (you act as yourself) and every write is a
  ledgered event. Per-sender access control (e.g. only the human may confirm)
  is deferred to the identity-model work.
- **Addresses and names match exactly (verbatim).** `@Tim` and `@tim` are
  distinct channels; normalize before writing if you want them unified.
  Confidence must be a number in `0..1`.

## The model, briefly (full design in docs/identity.md)

- **Identity** — the stable who. **Channel** — a `(kind, address)` you can
  reach them at; one identity has many. **Name** — a label, non-unique.
- **Confidence + provenance** on every link: a match is "0.7, proposed by
  agent kestrel" or "1.0, confirmed by Tim." You propose; later policy (or
  a person) decides what to trust.
- A channel can be **recorded before it is resolved** — capture the
  sighting now, decide who it is later.
- Resolution is a **lookup, not a freeze**: fixing a wrong link re-unifies
  history, because nothing baked the guess in.
