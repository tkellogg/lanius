import { useMemo } from 'react';

// docs/handoffs/chat-liveness.md M1 (wonky bit 5): the shared health projection.
// One module that reads the facts the SPA already polls — the `/api/status`
// object (App's `systemStatus`, refreshed every 10s) and `/api/liveness`
// (App's `liveness`) — and exposes them as ONE object. It is a projection, not
// a fetcher: no new server endpoint, no new polling loop. H3 (package-truth) and
// H4 (helper-first-encounter) consume the same hook.
//
// The honest vocabulary lives here too: a stopped actor must never look like a
// running one, so an actor with no reported status reads as 'not-started'
// (distinct from 'running'/'failed'), mirroring the server's liveness contract
// (src/web.rs `liveness_product_word`).

export type LlmWorld = 'a' | 'b' | 'c' | null;
export type ActorStatus =
  | 'running'
  | 'stopped'
  | 'failed'
  | 'restarting'
  | 'unknown'
  | 'not-started';

export type SystemHealth = {
  /** Is the web server's own bus connection live? Fire-and-forget publish can
   *  still not confirm delivery — this only says a broker is reachable. */
  brokerConnected: boolean;
  /** LLM-path world: 'a' native provider, 'b' a harness CLI on PATH, 'c' the
   *  dead-end (nothing that can answer). null = status not yet loaded. */
  llmWorld: LlmWorld;
  historyAvailable: boolean;
  commsAvailable: boolean;
  /** The grant-review word for the history / comms packages, read straight off
   *  /api/status (server `package_grant_words`). package-truth.md M2: `available`
   *  can read true while a package is parked after a revoke, so a pane that has no
   *  package list of its own (the sessions tab) reads this to tell a revoked pane
   *  (terminal — no repair) apart from a merely-unreachable one, keeping it in
   *  agreement with the configure row. '' when status has not loaded. */
  historyGrant: string;
  commsGrant: string;
  /** Has ANY actor reported a liveness status? A real stack always has resident
   *  actors (history/comms/…), so an empty map means liveness has not loaded or
   *  hit the retained-status replay gap seen across a daemon restart
   *  (package-truth.md M2 side-observation) — a running actor then reads "status
   *  unknown" honestly rather than "not started". */
  anyActorReported: boolean;
  /** The product word for a capability actor, or 'not-started' when the actor
   *  has never reported a status (never conflated with 'running'). */
  actorStatus: (name: string) => ActorStatus;
};

/** Pure projection of (status, liveness) → the shared health facts. Exported for
 *  direct unit exercise (there is no unit runner; ui.spec.mjs drives it through
 *  the test seam below). No React, no I/O — the same inputs always map to the
 *  same object. */
export function systemHealth(status: any, liveness: any): SystemHealth {
  const actors = liveness && typeof liveness === 'object' ? liveness.actors ?? {} : {};
  const world = status?.llm?.world;
  return {
    brokerConnected: status?.broker_connected === true,
    llmWorld: world === 'a' || world === 'b' || world === 'c' ? world : null,
    historyAvailable: status?.history?.available === true,
    commsAvailable: status?.comms?.available === true,
    historyGrant: typeof status?.history?.grant === 'string' ? status.history.grant : '',
    commsGrant: typeof status?.comms?.grant === 'string' ? status.comms.grant : '',
    anyActorReported: Object.keys(actors).length > 0,
    actorStatus: (name: string) => {
      const a = actors?.[name];
      const s = a?.status;
      // A capability with no retained status simply does not appear in the
      // liveness map — the honest word is 'not-started', not 'running'.
      if (s === 'running' || s === 'stopped' || s === 'failed' || s === 'restarting') return s;
      if (a == null) return 'not-started';
      return 'unknown';
    },
  };
}

/** The hook H3/H4 consume: a memoized projection over App's already-polled
 *  `systemStatus` and `liveness` state. It takes those as inputs (App owns the
 *  polling) — it never fetches. */
export function useSystemHealth(status: any, liveness: any): SystemHealth {
  return useMemo(() => systemHealth(status, liveness), [status, liveness]);
}

// Test seam (no unit runner in this repo — see M1 acceptance). A pure, secret-
// free projection exposed on window so ui.spec.mjs can exercise every branch
// (broker down, world c, history unavailable, actor running/failed/not-started)
// against the SHIPPED function rather than a reimplementation.
if (typeof window !== 'undefined') {
  (window as any).__systemHealth = systemHealth;
}
