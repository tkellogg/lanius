# webhook — send a message out (the egress template)

`webhook` is the worked example of **egress** in elanus (docs/actors.md).
Ingress — messages arriving — is event-shaped and comes *in* over the bus.
Egress — sending one out — is command-shaped (a specific send, with a result),
so it goes **direct**: the bridge calls the outside service itself. It is not
relayed through the bus, because fire-and-forget pub/sub is the wrong tool for
a command that returns a delivery result.

What stays on the bus is the **record**. After the direct send, webhook emits
`obs/channel/webhook/sent {url, ok, status, correlation}` — so the flight
recorder and the provenance trail are whole even though the delivery left the
bus.

## It is a daemon, on purpose

webhook is a **daemon** (a long-running actor), not a per-event exec handler.
That is what makes its receipt trustworthy: a daemon is spawned with its
package token, so its `elanus bus pub` authenticates as `webhook` and the
broker stamps the receipt `sender = webhook`. A per-event exec handler runs
uncaged and tokenless, so it would authenticate as the *owner* and mislabel
every send as owner-originated (docs/security.md entry 16). Any real egress
bridge should be a daemon for this reason.

## Use

```sh
elanus emit in/package/webhook/send --correlation req-7 \
  --payload '{"url":"https://example.com/hook","text":"hello"}'
# -> POSTs {"text":"hello"} to the url directly, then publishes
#    obs/channel/webhook/sent {ok, status, correlation:"req-7"}
elanus bus sub obs/channel/webhook/sent --count 1   # watch the outcome
```

## The shape to copy for a real channel

This is the template for any external-channel egress bridge (Bluesky, Discord,
SMS, email):

1. A **daemon** with its own identity (so its sends attribute to it).
2. The request arrives addressed to the bridge's **inbox**
   (`in/package/<bridge>/send`) — there is no `out/` plane. Sending to another
   elanus actor is just writing *its* inbox; sending to the outside is a direct
   command that gets observed.
3. **Delivery is direct** — replace the `urllib` POST with the service's API or
   SDK (and its credentials, which the bridge holds so no agent has to).
4. **Emit an `obs/channel/<kind>/sent` record** with the outcome and the
   triggering correlation, so the send is auditable and causally linked.

A failed send is a recorded outcome (`{ok:false, detail}`), never a crash.

## Caveat: this is a template, not a hardened bridge

webhook posts to whatever URL the request names, so anything that can publish
`in/package/webhook/send` can make the harness POST to an arbitrary address —
classic SSRF (internal services, cloud metadata). The in-scope adversary is a
**prompt-injected agent** (docs/security.md section 0): if such an agent holds
the publish grant for `in/package/webhook/send`, it can drive arbitrary
outbound requests. The grant is the boundary (entry 12), so:

- Scope the publish grant tightly — never a `#` wildcard (entry 15 residual);
  grant it only to the actors that should be able to send.
- A real bridge **pins its destination** (the service's own endpoint, not a
  caller-supplied URL), holds its own credentials, and allowlists targets.

Treat the publish grant on `in/package/webhook/send` as the egress capability
it is.
