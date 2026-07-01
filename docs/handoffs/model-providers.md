---
status: done
author: Opus 4.8 in Claude Code on Elanus
last-updated: 2026-06-26
---
# Handoff: model providers as a first-class, protected resource

Make a **provider** a named, encrypted credential that elanus can point any LLM
consumer at — the genai **dispatcher** *or* a coding **harness** (Claude Code,
Codex, opencode). The credential is a **sum type** whose variants are **not valid
everywhere** (an API key feeds both; a "use the tool's own login" feeds only a
harness, never the dispatcher), so resolution is a **partial** function
`materialize(credential, consumer)` that fails closed with a legible message.
Providers are selected at launch by an **elanus-level option that sits before the
tool token** so it never collides with forwarded tool args:
`elanus code --provider deepseek claude --resume`.

This answers two `_questions.md` items that are really **one missing primitive**
seen from two subsystems:

- **"model config error: provider list unavailable — I should just have a link to
  click to get me to where model providers are set up."** (the dispatcher/web
  side — there is no providers surface to link to, and nothing to enumerate.)
- **"set a model provider on a subagent … Claude Code on the Claude.AI login
  launches a subagent that uses DeepSeek's ANTHROPIC_URL and API key … annoying
  af rn."** (the harness side — currently *impossible*, not just annoying.)

## Tim's framing (2026-06-25)

- **The credential is a sum type, and that's the subtle part.** "API credentials
  are simple on the surface, just KEY + URL, but sometimes you want to send
  additional headers (e.g. LiteLLM). But then, other credentials aren't directly
  secret, like Claude Code with the Claude.AI login, or Codex with the ChatGPT
  login. You can't use those in a normal genai client … The sum type framing is
  wrong because **not every variant is valid everywhere**." (The fix isn't to drop
  the sum type — it's to pair it with a per-consumer **validity matrix**.)
- **The payoff is composition.** "This lets you run codex on ChatGPT inside Codex
  on GLM-5.2, which is honestly a bit crazy. That would be amazing."
- **Don't touch spawn; add an elanus option before the tool.** "The current ones
  work great, the problem is you need new options. Why not just do
  `elanus code --provider deepseek claude --resume`. Then the provider option is
  handed to elanus, while all the claude options are forwarded unambiguously to
  claude code."
- **Storage: encryption + SQL.** "ideally we could store it in the OSX keyring,
  whatever is on Linux … But if that isn't consistent x-plat … I favor encryption
  + SQL. My reasoning is more about where the encrypted bits would go."
- **It's not an identity.** "It's not an authority or an identity. It's just a
  resource. Protected, sure, but that's it."

## Why this is one missing primitive, not two features

There is **no first-class provider** in elanus. A provider today is two loose
fields — `base_url` + `api_key_env` on `ModelCfg` (src/profile.rs ~95) — honored
in **exactly one place**: the dispatcher's genai client (`build_client`,
src/exec.rs ~1078), where a `ServiceTargetResolver` rewrites the endpoint and
`AuthData::from_env`. Everywhere else the coordinates are either reconstructed ad
hoc or deliberately thrown away. That single absence explains both questions, and
why they look unrelated:

- **#4 (web/dispatcher).** The "provider list unavailable" warning
  (ui/web/src/components/primitives.tsx ~82, `ModelField`) fires when
  `/api/admin/models` returns empty. That endpoint (src/web.rs ~891) shells
  `elanus models --json`, which **live-probes** the provider's `/v1/models`
  (src/models.rs ~14–100). It comes back empty whenever the key isn't set, the
  host has no `/models`, or — the case Tim hit — you're on a **Claude.AI OAuth
  login**, which has no API-key `/models` to probe. The warning isn't "no models";
  it's "I have no provider to ask." There is **nowhere to link to** because there
  is no providers surface — coordinates are buried per-agent under Configure →
  Advanced → Provider (`#cfg-section-provider`, App.tsx ~1716–1741).
- **#5 (harness).** Every harness launch **scrubs** provider creds first —
  `PROVIDER_CRED_VARS` (src/codeagent.rs ~146) → `scrub_provider_creds`
  (~164), called before identity on both the headless (~3077) and TUI (~3129)
  paths — so a spawned tool uses its **own** login, not the parent's keys. Correct
  and intentional. But there is **no opt-in** to override it, so
  "parent-claude-on-Claude.AI spawns child-claude-on-DeepSeek" can't be expressed
  at all. Model flags are parsed (`extract_model_effort` ~1613) but only land in
  telemetry; they never touch the child's provider.

## What already holds today (build on it, don't re-pave it)

Be accurate about the starting point — three pieces are already right:

- **The dispatcher already does per-profile providers.** `build_client`
  (src/exec.rs ~1078) reads `prof.model.base_url` / `prof.model.api_key_env` and
  builds a genai `ServiceTargetResolver` (endpoint override + `AuthData::from_env`,
  adapter-aware via `AdapterKind`). And `subagents.allow_profiles` already lets a
  parent spawn a child profile with its own `[model]`. So **per-subagent provider
  exists on the dispatcher side** — it's just verbose (coordinates re-typed per
  profile) and has no UI. This milestone gives it a name; M3 makes it reference one.
- **The scrub is the *enabler* of nesting, not the blocker.** Because every launch
  wipes inherited creds to a clean slate and *then* (in M2) injects the named
  provider, the variants **compose under nesting**: "codex-on-ChatGPT inside
  codex-on-GLM-5.2" works precisely because the inner launch scrubs the outer's
  `OPENAI_*` and re-establishes from a `NativeLogin` provider → the inner codex
  falls through to its own ChatGPT login. Keep the scrub; add the opt-in injection.
- **Adapter knowledge already lives at launch.** Each harness has an adapter
  (`Harness` trait, src/codeagent.rs ~338; claude/codex/opencode impls), and the
  env-set sites are known (claude headless ~3084, codex ~3372, opencode ~4169).
  Injecting `ANTHROPIC_*` vs `OPENAI_*` slots in right after the scrub.

The gaps:

1. **No named provider.** Coordinates are duplicated per profile and absent for
   harnesses. Nothing for the UI to enumerate (#4's empty list).
2. **No injection opt-in at the harness.** Scrub-and-never-reinject ⇒ #5 impossible.
3. **No managed secret store.** The key is an ambient env var named by
   `api_key_env` (read out of the daemon's environment, src/envcompat.rs) — fine
   as a pointer, but there is no place to *define, list, test, or protect* a
   credential.
4. **The credential is assumed uniform.** A Claude.AI / ChatGPT login is **not**
   `base_url + key` and **can't** feed a genai client — it can only mean "leave the
   harness's own login alone." That variant has nowhere to live today.

## The model

### A provider is a credential + how to consume it

```rust
enum Credential {
    /// KEY + URL (+ optional extra headers: LiteLLM, OpenRouter, …).
    /// Consumable by: genai dispatcher AND coding harnesses (per-harness injection).
    ApiKey {
        wire: Wire,                       // Anthropic | OpenAI — which env vars / adapter
        base_url: String,
        key: Secret,                      // encrypted at rest
        headers: Vec<(String, Secret)>,   // optional; values may be secret
    },
    /// "Use the coding agent's own login; inject nothing." The current default
    /// behavior, *named* and *selectable*. No secret.
    /// Consumable by: coding harnesses ONLY — the dispatcher can't touch it.
    NativeLogin { tool: Option<HarnessId> },
}
```

`materialize(credential, consumer) -> Result<Injection>` is **partial** — the
validity matrix is the crux:

| | dispatcher | claude | codex | opencode |
|---|---|---|---|---|
| `ApiKey{Anthropic}` | ✅ genai resolver | ✅ env | ❌ codex is OpenAI-wire only | ✅ config |
| `ApiKey{OpenAI}` | ✅ genai resolver | ❌ | ✅ config | ✅ config |
| `NativeLogin` | ❌ *(no secret to feed)* | ✅ scrub-only | ✅ scrub-only | ✅ scrub-only |

**Injection is per-harness, not uniform env** — the 2026-06-25 spike (see Log)
corrected this. For a harness consumer `materialize` returns a
`HarnessInjection { env: Vec<(String,String)>, args: Vec<String> }` whose shape
differs by tool:
- **claude** — env: `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` (overrides the
  Claude.AI OAuth login — Tim-confirmed). No extra args.
- **codex** — **config, not env.** `OPENAI_BASE_URL` is *ignored* and the built-in
  `openai` provider id is *locked*, so inject a **custom `model_provider`** via `-c`
  args (`model_provider=<id>`, `model_providers.<id>.base_url`,
  `.wire_api="responses"`, `.env_key=<VAR>`) + the secret in `<VAR>`. `responses`-wire
  only.
- **opencode** — **config outranks a stored login.** Precedence is
  config > auth.json > env, so env-only is silently overridden by a same-provider
  stored login. Inject `OPENCODE_CONFIG_CONTENT` (inline JSON provider:
  `options.baseURL` + `options.apiKey`) and select `--model <id>/<model>`. opencode
  is multi-wire (it can also consume `ApiKey{Anthropic}`).

`NativeLogin` = scrub-only for any harness (the named default). The provider-defined
model is orthogonal — the user's tool args still pick the model.

An invalid pair is **refused with a legible message** ("provider `chatgpt` is a
native-login credential and can't drive the elanus dispatcher"; "provider
`deepseek-anthropic` speaks the Anthropic wire — codex needs an OpenAI-wire
provider"), never silently degraded. Two consequences worth stating:

- **`NativeLogin` is "the absence of injection, named."** It *is* today's default
  (scrub, inject nothing). Naming it makes it **selectable** — so a child can
  explicitly differ from its parent — and explains why the dispatcher rejecting it
  is correct, not a limitation: there's nothing to feed a genai client.
- **The dispatcher only ever accepts `ApiKey`.** That's not a special case; it's
  the matrix.

### Storage: encrypted at rest in SQL; the honest part is the master key

OS keyring is the wrong dependency for elanus: it runs as a **daemon**, often
headless Linux, where Secret Service / gnome-keyring / dbus is exactly what isn't
running or unlocked (the `keyring` crate is lovely on a desktop, inert under
systemd). So the **data** lives in the ledger, encrypted:

- A `providers` table (db.rs / ledger): **clear columns** for non-secret metadata
  (name, variant, wire, base_url, header *names*, tool) so `list` / UI / `test`
  are plain queries, and **one encrypted blob column** for the secret bytes (key +
  secret header values). `NativeLogin` rows carry no blob.
- AEAD — XChaCha20-Poly1305 (`chacha20poly1305`), or a vetted container
  (`age` / `cocoon`) if preferred — random per-row nonce stored beside the
  ciphertext. Decrypted **only transiently in daemon memory** at materialize time;
  **never** to obs, the config git, or logs.

**Threat model (record as a security.md entry).** Be honest about what this
defends: Tim's machine, Tim's agents, "safety = audit, not restriction." The real
threats are **accidental disclosure** — a key landing in git, a `.db` backup, an
obs stream, a `SELECT *` an agent runs, a screen-share. Encryption-at-rest closes
all of those. It does **not** defend against an attacker who already has full FS
read as the elanus user, and chasing that would be theater. The master key
(32 bytes) lives in `<root>/secret.key`, `0600`, generated on first use — see Open
Decisions for the keyring-upgrade / passphrase alternatives.

### A provider is a resource, not config and not identity

Per Tim's "it's just a resource": providers do **not** live in the config repo
(src/config_repo.rs) — they want neither the proposal/acceptance/autonomy
machinery nor git history; they're operator resources with a secret. And a
provider is **not** an authority-delegation dimension and **not** a phonebook
identity (docs/identity.md). "Protected" means **encrypted at rest + not freely
readable**, full stop — no narrowing, no `child ⊆ parent`. Choosing a provider for
a child is **audited** on the session-start obs (which session pointed which child
at which provider), **not gated**. Surface is a flat resource verb:
`elanus provider add | list | test | rm` (`test` runs the existing `/models`
probe so reachability is checked at definition time).

### Launch grammar: an elanus option before the tool token

`elanus code --provider <name> <tool> [tool args…]`. The provider is consumed by
elanus; **everything after the tool token forwards verbatim** to the tool — no
collision, no new spawn semantics. It works identically for interactive /
`--headless` / `spawn` because they're all launches downstream of the same option
parse. (Drop the earlier idea of a `spawn --provider` flag — Tim's call, and it's
cleaner.)

## Milestones

### M1 — the vault + the credential model (the resource & store)
The `Credential` sum type; the `providers` SQL table with the encrypted blob
column + master-key bootstrap (`<root>/secret.key`, `0600`, first-use generation);
`materialize(credential, consumer)` as a pure, fully-tested partial function with
the validity matrix; and `elanus provider add | list | test | rm`. No consumer is
wired yet — this ships the primitive and proves the matrix in isolation.
**Acceptance:** a provider can be defined, listed (secret never printed), and
`test`ed (reachability via the `/models` probe); secret bytes are encrypted at
rest (a raw `SELECT` on the table reveals no key); `materialize` returns the right
injection for every valid `(variant, consumer)` and a legible refusal for every
invalid one — covered by a unit test over the whole matrix.

### M2 — harness consumption + launch grammar (delivers #5 + the nesting payoff)
`elanus code --provider <name> <tool> …` parsed before the tool token (sibling of
`take_grants_flags`, src/codeagent.rs); after the existing `scrub_provider_creds`,
apply `materialize(cred, harness)`'s `HarnessInjection` — **per-tool** (claude→env,
codex→`-c` custom-provider flags, opencode→`OPENCODE_CONFIG_CONTENT`; see the model
section). `NativeLogin` = scrub-only. Wire/harness mismatch refused at launch.
**Nesting caveat (spike):** `PROVIDER_CRED_VARS` does *not* cover `CODEX_API_KEY` /
`CODEX_HOME` / `OPENCODE_CONFIG*`, so the inject step must **overwrite** them (not
rely on the scrub) for inner-launch composition to hold. **This is the slice that
unblocks Tim's scenario** and the codex-on-ChatGPT-inside-codex-on-GLM nesting.
**Acceptance:** a parent Claude Code session on its Claude.AI login runs
`elanus code --provider deepseek claude "…"` and the child talks to DeepSeek
(verified from the child's traffic), with the parent's login untouched; the same
holds for a codex child (via the `-c` custom-provider config) and an opencode child
(via `OPENCODE_CONFIG_CONTENT`) — env-only injection is proven insufficient for
both, so the test must exercise the config path; a nested launch with a different
provider composes (inner overrides outer, proven by the inner's traffic);
`--provider` on a wire-incompatible harness is refused with a clear message; no
`--provider` ⇒ byte-identical to today's scrub-only launch.

### M3 — dispatcher consumption
`[model].provider = "<name>"` on a profile; `build_client` (src/exec.rs ~1078)
resolves it via `materialize(cred, Dispatcher)` (ApiKey only; `NativeLogin`
refused with the legible message) instead of re-deriving `base_url`/`api_key_env`.
The inline `base_url` / `api_key_env` fields stay as a **deprecated** ad-hoc
override (back-compat; inline wins, or is removed per Open Decisions). An ApiKey
provider's `headers` are wired here via `ChatOptions::with_extra_headers` (additive —
preserves genai's adapter auth), carried on the **per-call** `ChatOptions` passed to
`exec_chat` — NOT client-wide on the `ClientConfig`: genai 0.6.5's Anthropic/OpenAI
adapters merge `extra_headers` only from the per-call `options` argument
(`client_impl.rs:110`), never the client config (the `or_else(client)` getter is dead
code for those adapters — this **corrects** the spike-2 client-level recommendation,
found in M3 validation). Reserve `AuthData::RequestOverride` for unorthodox auth that
must replace the auth header too.
**Acceptance:** a dispatcher agent whose profile names an ApiKey provider routes
through it (existing genai behavior, now by reference); a profile naming a
`NativeLogin` provider fails to start with the legible refusal; profiles with the
old inline fields keep working unchanged.

### M4 — the #4 UI
A **Providers** settings page + route (list / add / `test`); the `ModelField`
warning (primitives.tsx ~82) gains `… or [set up a provider →]` linking to it; the
model dropdown sources its list from the **selected named provider** rather than an
ambient probe. `NativeLogin` providers show no model list **and no warning** —
which is the actual fix for the spurious "provider list unavailable" on a Claude.AI
login (there was never a provider to probe).
**Acceptance:** from the model-config error a user reaches a page that lists
providers and adds/tests one; selecting an ApiKey provider populates the model
dropdown; selecting (or being on) a NativeLogin shows neither list nor warning.

## Open decisions (Tim's calls; recommend, don't assume)

1. **Master-key location.** Recommend **file-key** (`<root>/secret.key`, `0600`,
   first-use generation) as the default — it matches the real threat (accidental
   disclosure) and keeps the headless daemon auto-starting. Optional upgrade:
   keyring-backed *when a keyring is actually present and unlocked*, falling back
   to the file. Third option if the key should never touch disk:
   `ELANUS_SECRET_KEY` / passphrase at daemon start — but then no unattended
   auto-start. Recommend file-key now, keyring-upgrade later.
2. **Crypto primitive.** Recommend raw AEAD (XChaCha20-Poly1305) for a single
   encrypted column — minimal surface, no framing. `age`/`cocoon` if you'd rather
   lean on a vetted container with a key-file story. Tim's call on dependency taste.
3. **Inline `base_url`/`api_key_env`.** Keep as deprecated override (smoothest
   migration) or hard-cut to `provider =` only? Recommend **keep + deprecate**;
   revisit once a few profiles have migrated.
4. **Extra headers (LiteLLM) scope — RESOLVED by the 2026-06-25 spike.** genai
   0.6.5 supports arbitrary headers, so dispatcher headers need **not** be deferred:
   wire them in M3 via the **additive** `ChatOptions::with_extra_headers` (keeps the
   adapter's API-key auth), not `AuthData::RequestOverride` (which replaces *all*
   headers including auth — reserve for genuinely custom auth schemes). Harness
   headers ride the same per-tool config (codex `model_providers.<id>.http_headers`,
   opencode provider `options.headers`). Field modeled in M1.
5. **`NativeLogin.tool` pinning.** Tool-agnostic (valid for whatever harness you
   launch) vs pinned to a named tool's login. Recommend **agnostic by default**,
   optional pin — a pin is over-constraint until a reason appears.

## Verified (2026-06-25 spikes — see Log for evidence)

- **Harness auth precedence.** claude: injected `ANTHROPIC_AUTH_TOKEN`+`base_url`
  overrides the Claude.AI login (Tim-confirmed). codex: env injection **fails**
  (`OPENAI_BASE_URL` ignored, built-in `openai` locked) — must use `-c` custom-provider
  config. opencode: config > stored-login > env — must use `OPENCODE_CONFIG_CONTENT`.
  Encoded as `HarnessInjection` in the model section + M2.
- **genai 0.6.5 custom headers.** Supported and compiled — `ChatOptions::with_extra_headers`
  (additive). Open Decision 4 resolved; dispatcher headers land in M3.

## Read these first

- [../config.md](../config.md) — the config model. Read it to see **why providers
  are *not* here**: they're a protected resource with a secret, not git-tracked,
  agent-proposable settings.
- [../identity.md](../identity.md) — *who you are*. A provider is **not** an
  identity and **not** an authority-delegation dimension (cf.
  [authority-delegation.md](authority-delegation.md)) — Tim's "just a resource."
- [../security.md](../security.md) — the credential-vault threat model lands as a
  new entry (accidental-disclosure scope; the scrub at src/codeagent.rs ~146/164 is
  the existing harness-side defense this builds the opt-in onto).
- [../../src/exec.rs](../../src/exec.rs) — `build_client` (~1078): the **one** place
  a provider is honored today (genai `ServiceTargetResolver`, `AdapterKind`,
  `AuthData::from_env`, `normalize_base_url`). M3 attaches here.
- [../../src/codeagent.rs](../../src/codeagent.rs) — `PROVIDER_CRED_VARS` (~146) +
  `scrub_provider_creds` (~164) and the launch scrub sites (~3077 headless / ~3129
  TUI); the env-set sites (claude ~3084, codex ~3372, opencode ~4169); the
  `Harness` trait (~338); `take_grants_flags` as the model for the new
  `--provider` parse. M2 attaches here.
- [../../src/profile.rs](../../src/profile.rs) — `ModelCfg` (~95): where
  `provider = "<name>"` joins the existing `base_url`/`api_key_env`.
- [../../src/models.rs](../../src/models.rs) — the `/v1/models` probe (~14–100)
  that `test` and the M4 model dropdown reuse.
- [../../ui/web/src/components/primitives.tsx](../../ui/web/src/components/primitives.tsx)
  — `ModelField` (~82), the warning that gets the link.
- [../_questions.md](../_questions.md) — the two source items (provider-setup link;
  per-subagent provider) this collapses into one primitive.
- [deepseek-anthropic-endpoint] (project memory) — the live DeepSeek/genai facts
  (genai 0.6.5, `ServiceTargetResolver`, `AuthData::from_env`, the
  `api.deepseek.com/anthropic` base) the acceptance scenario runs against.

## Log

- 2026-06-25 — Written from a design conversation with Tim. Origin: predicting the
  `_questions.md` item he'd take first surfaced that the "provider-setup link" (#4)
  and "per-subagent provider" (#5) items are **one missing primitive** across two
  subsystems. Key turns in the design: (1) the credential is a **sum type with a
  per-consumer validity matrix** — `NativeLogin` (Claude.AI / ChatGPT login) can't
  feed the genai dispatcher, only a harness; "the sum type framing is wrong because
  not every variant is valid everywhere," resolved as a partial `materialize`. (2)
  The harness **scrub** is the *enabler* of nesting, not the blocker — add the
  opt-in injection half and "codex-on-ChatGPT-inside-codex-on-GLM" composes. (3)
  Storage = **encryption + SQL** (keyring too inconsistent for a headless daemon);
  master key in a `0600` file, threat model = accidental disclosure not local
  attacker. (4) A provider is **a resource, not an identity/authority** — out of the
  config repo, audited not gated. (5) Launch grammar `elanus code --provider <name>
  <tool> …` (option before the tool token) — the `spawn --provider` idea dropped.
  Scope confirmed: **M1→M2 is the spine** (vault + the launch path that delivers
  #5); M3 dispatcher, M4 UI follow. Two items flagged to **spike before trusting**:
  OAuth-vs-injected-cred precedence in a child Claude Code, and genai 0.6.5 custom
  headers.

- 2026-06-25 — **Spikes run (Opus, high effort, sequential) — both resolved
  high-confidence; M2 reshaped.** Spike 1 (harness auth precedence): the
  validity-matrix "codex/opencode → `OPENAI_*` env" row was **wrong**. codex
  *ignores* `OPENAI_BASE_URL` and locks the built-in `openai` id; the working
  override is a **custom `model_provider` via `-c` config** (proven — a local stub
  received `POST /v1/responses` with the injected Bearer), `responses`-wire only.
  opencode honors env *but* config > auth.json > env, so a stored same-provider login
  silently wins; robust path is **`OPENCODE_CONFIG_CONTENT`** (proven). Net: harness
  injection is **per-tool `HarnessInjection {env, args}`**, not uniform env — matrix,
  model section, and M2 updated; noted the scrub gap (`CODEX_*`/`OPENCODE_CONFIG*`
  absent from `PROVIDER_CRED_VARS`) means the inject step must *overwrite* for
  nesting. Spike 2 (genai 0.6.5 headers): **supported + compiled** —
  `ChatOptions::with_extra_headers` (additive, preserves adapter auth) is the
  dispatcher path; `AuthData::RequestOverride` is the full-override escape hatch;
  avoid `WebConfig::with_default_headers` (needs a direct reqwest dep). Open
  Decision 4 resolved (dispatcher headers in M3). Both spikes empirical (local stub
  server, throwaway `CODEX_HOME`, compile-checked crate) and repo-contained (no
  pollution). Decisions taken to unblock M1: **file-key master secret**
  (`<root>/secret.key`, `0600`, keyring-upgrade deferred) and **XChaCha20-Poly1305**
  (`chacha20poly1305` crate) per Open Decisions 1–2. **Next: M1 implementation.**

- 2026-06-25 — **M1 landed (impl Opus-med → validate Opus-high, no blocking
  findings).** The vault primitive, unwired (consumers are M2/M3): `src/provider.rs`
  (Credential sum type, `materialize` + validity matrix, per-harness
  `HarnessInjection`, master-key bootstrap + XChaCha20-Poly1305 seal/open, the
  encrypted `providers` table), `src/providercli.rs` (`elanus provider
  add|list|get|test|rm`), `src/models.rs` (extracted `probe` so `test` reuses the
  `/models` candidates), `src/main.rs` wiring, `chacha20poly1305` dep, security.md
  **entry 23** (vault threat model). Orchestrator review hardened three nonblocking
  findings before commit: a **redacting `Debug` for `HarnessInjection`** (its env
  values carry the decrypted key — a stray `{:?}` in M2 would leak), **dropped the
  unused `Serialize`/`Deserialize` on `Secret`** (latent clear-emit footgun), and a
  **`valid_name` gate at `add`** (`[a-z0-9][a-z0-9-]*`, ≤64 — keeps names safe as
  env tokens / codex TOML keys, disallows the `-`/`_` collision and dot-breakage);
  added two tests (name rejection, `get` fail-closed on a corrupt blob). `cargo
  test` **370 pass**, clean build. Carried to M2 (flagged, non-blocking): verify the
  codex `-c` flag grammar (incl. hyphenated ids and `env_http_headers`) against a
  real codex binary; confirm opencode routes through the injected custom-provider id
  when `--model <id>/<model>` is appended; the `add` upsert silently overwrites a
  same-named provider. **Next: M2 (harness consumption + `--provider` launch).**

- 2026-06-26 — **M2 landed (impl Opus-med → empirical validate Opus-high → fix →
  re-validate, pass).** `elanus code --provider <name> <tool> …` is parsed before the
  tool token (`take_provider_flag`, src/codeagent.rs ~224; threaded via main.rs to
  `launch`/`spawn`) and gated entirely on the flag — no `--provider` ⇒ byte-identical
  launch. After `scrub_provider_creds`, `apply_provider_injection_env` (~206) clears
  the scrub-gap vars (`HARNESS_CONFIG_VARS`: `CODEX_*`/`OPENCODE_CONFIG*`, even for a
  `NativeLogin` clean child) then sets `materialize(name,cred,Harness(h),model)`'s env;
  codex `-c` args append before the prompt; the secret rides env, never argv. Provider
  NAME (not the secret) is recorded on session/start obs (audit, not gate). Wire/harness
  mismatch + unknown provider are refused before any child spawns.
  **Empirical validation earned its keep — two real bugs code-review would have missed,
  both caught by running the real binaries against a 127.0.0.1 stub:** (1) **M1's codex
  injection was silently NON-FUNCTIONAL** — codex 0.141 requires
  `model_providers.<id>.name`; without it every `codex exec` failed config load
  (`provider name must not be empty`). Added the `name` field (+ TOML-quoted
  `model_provider` value so hyphenated ids parse); a full
  `elanus code --provider deepseek-test codex --headless` then hit the stub with the
  injected bearer on the `responses` wire. (2) **opencode config was incomplete** —
  `options` alone yields `ProviderModelNotFoundError` and zero connection for any
  non-models.dev-slug name; the config must also carry `npm`
  (`@ai-sdk/openai-compatible` / `@ai-sdk/anthropic`) AND a `models.<model>` entry.
  Fixed by threading the launch `--model` into `materialize` (new `model: Option<&str>`,
  read only by the opencode arm; no model + opencode ⇒ legible refusal); re-verified
  end-to-end (routes with the injected bearer). codex + opencode empirically confirmed;
  claude sound-by-construction + Tim-confirmed OAuth precedence (no Anthropic creds in
  the sandbox to live-launch). `cargo test` **378 pass** (single-threaded/isolated;
  see flaky note). Notes: a **pre-existing flaky test** surfaced —
  `secrets::tests::migration_folds_legacy_human_into_owner_without_orphan` fails ~1/4
  parallel runs (green single-threaded + isolated) because `secrets::owner_name`/`ensure`
  reads process-global state (`ELANUS_OWNER` env + a CWD/shared-relative `profile::load`)
  a parallel test races — **unrelated to model-providers, not introduced here** (M2
  tests mutate no process env/CWD); flagged for a separate hygiene fix. Minor M2 UX
  carried: `--provider=<name>` (equals form) isn't parsed (space form only); the
  `CODEX_HOME`/`CODEX_API_KEY` clear under `--provider` is opt-in (doc note); a stale
  `ELANUS_PV_<other>_KEY` from an outer launch is inherited (harmless — codex reads only
  its configured `env_key`, homogeneous authority). **Next: M3 (dispatcher
  `[model].provider` in `build_client`, src/exec.rs).**

- 2026-06-26 — **M3 landed (impl Opus-med → validate Opus-high → fix →
  re-validate, pass).** `provider: Option<String>` on `ModelCfg` (src/profile.rs);
  `build_client` (src/exec.rs) resolves a named provider via a testable
  `dispatcher_plan` seam (`Default | Inline | Provider`): a `Provider` sets the
  endpoint from the provider's wire + literal-key auth (`AuthData::from_single`, not
  `from_env` — the vault stores the secret itself) + extra headers; `NativeLogin`
  propagates the legible refusal so the agent fails to start; `provider` wins
  wholesale over the (deprecated, untouched) inline `base_url`/`api_key_env`;
  `provider=None` is byte-identical to pre-M3. **Validation overturned a spike-2
  claim** (a genuine catch): the impl wired headers **client-level** per spike-2's
  recommendation, but a source read of genai 0.6.5 showed the Anthropic/OpenAI
  adapters merge `extra_headers` ONLY from the **per-call** `options` arg
  (`client_impl.rs:110`) — the client-level `or_else` getter is dead code for those
  adapters — so the LiteLLM/OpenRouter headers would be **silently dropped**. Fixed:
  `build_client` now returns `(Client, Option<ChatOptions>)`, the headers ride the
  per-call `ChatOptions` threaded into the sole `exec_chat` call site (exec.rs ~510),
  with a unit test asserting the header survives onto that path (the handoff M3 body +
  Open Decision 4 corrected to match). `cargo test` **384 pass**. Carried non-blocking:
  the header-survival test is unit-level (the genai per-call merge itself is
  spike-established, not re-exercised against a live endpoint); endpoint normalization
  uses the provider's wire while genai still picks the request adapter from the model
  name (a model/wire mismatch is user misconfig, same exposure as the inline path).
  **Next: M4 (the #4 UI — Providers page + route + ModelField link).**

- 2026-06-26 — **M4 landed (the #4 UI) — impl complete + orchestrator-verified; the
  validation WORKFLOW died on a reporting error, not the work.** The Workflow's impl
  agent produced a complete, coherent M4 in the tree but exceeded the StructuredOutput
  retry cap on its FINAL report (no valid structured output after 5 tries), so the
  validate phase never ran — the work was salvaged and verified directly by the
  orchestrator instead (no rogue commits, no repo pollution; tree clean). Delivered:
  backend admin endpoints (src/web.rs) — GET /api/admin/providers (shells `provider
  list --json`), POST add, POST rm, GET providers/test (passes through `provider test
  --json`); the api KEY rides the child's STDIN via a new `cli_stdin` helper, NEVER
  argv/weblog/obs, and a `valid_provider_name` gate (`[a-z0-9][a-z0-9-]*`, ≤64) runs
  before every shell-out. `provider test --json` added to the CLI (src/providercli.rs)
  with in-band reachability (`{reachable:false,error}`) and the native-login case
  (`{native:true, reachable:null}`). Frontend (ui/web): a new `ProvidersView.tsx`
  (list/add/test/rm; key as a password field; redaction-only display), a nav entry,
  the ModelField "… or set up a provider →" link (the literal #4 ask) + a `native`
  prop that SUPPRESSES the "provider list unavailable" warning for a native-login
  provider (the real fix for the spurious warning on a Claude.AI OAuth login), and a
  Configure "named provider" dropdown sourced from the vault that writes
  `[model].provider` + sources the model list from the chosen provider (inline
  base_url/api_key_env disabled when a provider is set, kept for back-compat).
  **Verification (orchestrator, empirical):** `cargo test` 386 pass (+2 web route
  guards incl. one asserting `provider_add` keeps the key OFF argv); `tsc --noEmit`
  clean; `vite build` clean; `cargo build` re-embeds the SPA (`include_dir!` over the
  gitignored `ui/web/dist`); `ui.spec.mjs` **ALL PASS** with 15 new provider
  assertions — add(api-key+native)/list/redaction/native-test-not-error/
  page-opens-from-nav/link-navigates/dropdown-from-vault/native-no-warning/rm/
  **cross-origin-add-refused-403**/no-console-errors. Scope note: M4 did NOT take on
  full browser-history routing (a separate `_questions.md` item) — the Providers page
  is a nav view, consistent with the current app. **All four milestones (M1 vault, M2
  launch, M3 dispatcher, M4 UI) are implemented + verified on branch `model-providers`;
  status `verifying` — awaiting Tim's review + merge to `main`. The whole arc delivers
  both source `_questions.md` items: #4 (provider-setup link/UI) and #5 (per-subagent
  provider, incl. the codex-on-ChatGPT-inside-codex-on-GLM nesting).**
- 2026-07-01 — Merged: `git log main..model-providers` is empty and all M1–M4
  commits (af75d13, 8921cf7, 7ef154a, 31cc440) are in main's history; frontmatter
  flipped to `done` (small-fixes M3 truth sweep).
