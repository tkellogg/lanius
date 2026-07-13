# Broker: `CONNECT refused: bad credential for identity "<name>"`

## Symptom
The broker log (e.g. `~/.lanius/root/lanius-serve.log`) repeats, often a few times
every few seconds:
```
[bus] CONNECT refused: bad credential for identity "owner"
```
or:
```
[bus] CONNECT refused: bad token / unknown identity (user Some("resident-gate"))
```
`<name>` is usually `owner` but can be any human/kernel identity.

## What it means
**The broker is doing its job — this is a rejection, not a crash.** On every MQTT
CONNECT the broker validates the client's username/password against the secret it
reads **live from its OWN root** (`<root>/.secrets/<identity>`) — see
`src/broker.rs` (the identity/credential block ~416–460; `crate::secrets::read`).
A human/owner/kernel identity must present that exact secret; a `code-*` session
presents a scoped session token instead. A mismatch → refused + this log line. The
client then retries on its keepalive, which is why the line *repeats*.

So a steady stream of refusals means **some client keeps connecting with the wrong
secret for that identity.**

For a package/resident name such as `resident-gate`, `bad token / unknown identity`
means the broker did not recognize the supervisor-minted package token for that
connection. That is still a refused CONNECT, not broker authority leakage.

## Most common cause: a stray client on the wrong root
A process is connecting to *this* broker while holding a *different* root's owner
secret. The classic source is an **orphaned `lanius web` (or daemon) left by a
test/probe/workflow run**: it was started with a `/tmp` `--root` but the **default
broker address** `mqtt://127.0.0.1:1883`, so it knocks on your *real* broker
presenting its *own* root's owner secret. The web server reads its owner secret
**once at startup and caches it** (`src/web.rs:149`, `opts.set_credentials(owner,
secret)`), so it never recovers — it just reconnect-loops forever.

Less common: a rotated/restored `.secrets` out of sync with running clients, or a
real client genuinely misconfigured with the wrong `--root`.

For `resident-*` package names, the common source is an orphaned daemon-mode
package from `tests/e2e.sh` whose temp package root was removed while its shell
loop kept running. Its own stderr may grow quickly under
`/private/tmp/<root>/run/pkg-<name>/err.log`.

## Diagnose
1. Watch the rate: `tail -f ~/.lanius/root/lanius-serve.log` (or your serve log).
2. Enumerate and split legit vs stray (see SKILL.md):
   ```sh
   ps -eo pid,ppid,command | grep '[e]lanus'
   ```
   Legit stack is under your real root (`serve` → `daemon` → `web`). Suspects are
   `lanius web`/`daemon` on a `/tmp` or `/private/tmp` root (often `ppid 1`).
   Also look for orphaned daemon packages such as
   `/private/tmp/.../packages/resident-gate/scripts/main`.
3. Confirm a stray is the culprit (optional, very convincing): kill ONE stray and
   watch the refusal rate in the log drop. Starting a foreign-root `lanius web` on
   `:1883` makes the rate jump; killing it restores baseline.

## Fix
Kill the strays — **never** the legit `serve`/`daemon`/`web`. Heads-up: `lanius
web` **ignores SIGTERM**, so a plain `kill` does nothing — use `kill -9`:
```sh
# verify each pid first, then:
kill -9 <stray-pid> <stray-pid> …
```
The refusals stop immediately. **No daemon restart and no re-mint is needed** — the
broker's check was correct the whole time; you only removed the bad clients.

If it's genuinely an operational secret mismatch (not a stray): make sure the
clients and the broker share one root, or restart `lanius serve` so everything
re-reads the same `.secrets`.

## Prevent
- Probe / UI-test / adversarial **workflows must bind their broker + web to a
  per-root NON-default port** (or disable the bus) so a stray can never reach the
  real broker on `:1883`. Reap with `kill -9` (SIGTERM is ignored).
- See [stray-workflow-processes.md](stray-workflow-processes.md) for the general
  cleanup, and the `adversarial-workflow-containment` memory for the containment
  rules around dispatched workers.
