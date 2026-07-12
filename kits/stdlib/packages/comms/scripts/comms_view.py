"""comms_view — the chat-shaped conversation projection + introspection.

This is the reader that used to live hard-coded in the core web server
(`src/web.rs` `conversation_rows` / `source_for` / `is_worker_session` and the
threading helpers, docs/handoffs/comms-package.md M1/M2). It has been RELOCATED
here, into the comms package, so adding a channel or changing the chat
projection is a package edit, not a kernel edit (docs/channels.md closing
section: "web.rs is_worker_session/source_for hard-code the channel taxonomy
into the core conversation projection — adding a channel edits the kernel").

Doctrine (docs/bus.md [DECIDED]): reconstruction views are USERLAND subscribers,
never kernel. Like the `history` package this leans on, it holds NO state of its
own — every query is answered from a fresh read-only sqlite connection over the
kernel's ledger (`in/# -> ledger` is the single durable copy; docs/topics.md).
The chat SHAPE and the API are the part that was missing; the durable bytes were
already on disk.

The port is behaviour-preserving: for a seeded ledger the `conversations` query
returns JSON byte-equal (modulo key ordering) to what `conversation_rows`
returned in-core (comms-package M1 golden). The source-labeling fallbacks
(web-/github/jira/linear/cron) traveled here verbatim as a SHRINKING safety net
with the same TODO — they empty only as those sources become packages that stamp
their own `payload.source` (Handoff C's seam), and adding a new channel must NOT
add a branch to `source_for`.

Worker DMs (worker-dm unification M1/M3): a coding session's exchange with the
owner now threads as a first-class conversation instead of being DROPPED. It is
folded from two broker-verified directions — owner→worker deliveries on
`in/agent/<agent>/<session>` (attributed by topic segment + `code-deliver-*`
correlation) and worker→owner sends on `in/human/<owner>` whose sender column is
a coding session of this tool-noun — and carries the honest `"code"` source
label. The former display-routing `is_worker_session` DROPs are gone;
`is_worker_session` now survives only as a durable-kind FACT read (the `"code"`
source label and the spoof guard that rejects a payload `session:` claim not
matching its verified sender), never to segregate a thread out of chat.
"""
import json
import sqlite3

WORKER_PREFIX = "code-"


# ---- generic value helpers (ported from src/web.rs) -------------------------

def parse_payload(raw):
    """Ledger payload text -> dict, or {} for null / non-object / bad json.
    Mirrors web.rs parse_payload (filter is_object)."""
    if raw is None:
        return {}
    try:
        v = json.loads(raw)
    except (ValueError, TypeError):
        return {}
    return v if isinstance(v, dict) else {}


def parse_stored(raw):
    """Transcript content text -> json value, or the raw string on bad json.
    Mirrors web.rs parse_stored."""
    try:
        return json.loads(raw)
    except (ValueError, TypeError):
        return raw


def _compact(value):
    """serde_json::Value::to_string equivalent: compact, keys SORTED (serde uses
    a BTreeMap without preserve_order), UTF-8 kept raw."""
    return json.dumps(value, separators=(",", ":"), sort_keys=True, ensure_ascii=False)


def message_text(content):
    """web.rs message_text: pull the human-readable text out of a stored message."""
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, dict):
        t = content.get("text")
        if isinstance(t, str):
            return t
        c = content.get("content")
        if isinstance(c, str):
            return c
        if content.get("truncated") is True:
            p = content.get("preview")
            if p is not None:
                return p if isinstance(p, str) else _compact(p)
        return _compact(content)
    return _compact(content)


def truncate_text(value, max_chars):
    """web.rs truncate_text: collapse runs of whitespace to single spaces, then
    truncate to `max` code points with a trailing ellipsis. str.split() matches
    Rust split_whitespace (drops empties, unicode-ws aware)."""
    collapsed = " ".join(value.split())
    if len(collapsed) > max_chars:
        kept = collapsed[: max(0, max_chars - 1)]
        return kept + "…"
    return collapsed


def short_iso(value):
    return value.replace("T", " ", 1)[:16]


def normalize_role(role):
    if role == "user":
        return "you"
    if role == "assistant":
        return "agent"
    return role


def truthy(v):
    """web.rs truthy over a json value."""
    if isinstance(v, bool):
        return v
    if v is None:
        return False
    if isinstance(v, (int, float)):
        return v != 0
    if isinstance(v, str):
        return len(v) > 0
    return True


def encode_segment(s):
    """topic::encode_segment — percent-encode the reserved topic chars."""
    out = []
    for c in s:
        out.append({"%": "%25", "+": "%2B", "#": "%23", "/": "%2F"}.get(c, c))
    return "".join(out)


def human_mailbox(owner):
    return "in/human/" + encode_segment(owner)


# ---- worker-session classifier (ported from codesession::is_worker_session) --

def _valid_principal(name):
    """secrets::valid_principal."""
    return (
        bool(name)
        and len(name) <= 64
        and not name.startswith(".")
        and "/" not in name
        and "\\" not in name
    )


def is_worker_session(conn, session):
    """codesession::is_worker_session: decide from the durable stored `kind`
    when `code_sessions` carries it, else fall back to the `code-` name prefix.
    A UI classifier only — grants and gates nothing. Any read fault (missing
    table/column on an old db) collapses to the prefix fallback, exactly as the
    Rust `.optional().ok()` chain does."""
    try:
        row = conn.execute(
            "SELECT kind FROM code_sessions WHERE elanus_session = ?", (session,)
        ).fetchone()
    except sqlite3.Error:
        row = None
    if row is not None and row[0] is not None:
        # PrincipalKind::from_stored: only "session" == a coding worker; any other
        # known/garbage value is NOT a worker (garbage -> None -> prefix in Rust,
        # but a non-"session" known kind is explicitly not a worker).
        stored = row[0]
        if stored in ("session", "human", "kernel", "package"):
            return stored == "session"
        # garbage stored value -> fall through to the prefix test
    return session.startswith(WORKER_PREFIX) and _valid_principal(session)


def worker_session_noun(conn, session):
    """The tool-noun (claude-code / codex) a coding session belongs to, from the
    durable `code_sessions` record; None when the session has no record. Used to
    scope a worker's DM thread to its tool-noun's converse bucket (worker-dm
    unification M1). A durable-fact read, never display routing."""
    try:
        row = conn.execute(
            "SELECT agent_noun FROM code_sessions WHERE elanus_session = ?", (session,)
        ).fetchone()
    except sqlite3.Error:
        return None
    return row[0] if row is not None else None


# ---- source labeling (ported from web.rs source_for) ------------------------

def source_for(session, sender, payload, owner):
    """web.rs source_for. Prefers an explicit stamped `payload.source`; the
    spelling-based guesses below are a SHRINKING safety net.

    TODO(docs/channels.md, closing section): these legacy spelling-based guesses
    are a shrinking net. The right seam is the `payload.source` branch above: a
    channel/package stamps its own source at the source (as the Telegram bridge
    does — docs/handoffs/agent-dm-relay.md M3, and the send/ask reply path via
    exec::reply_source), and these fallbacks wither. Adding a new channel must
    NOT add a branch here. github/jira/linear/cron stay only until they become
    packages that declare their own source."""
    claimed = payload.get("source")
    if isinstance(claimed, str):
        claimed = claimed.strip().lower()
        if claimed:
            return claimed
    s = session.lower()
    frm = (sender or "").lower()
    if s.startswith("web-"):
        return "web"
    if "github" in frm or "jira" in frm or "linear" in frm:
        return "github"
    if "cron" in frm or "timer" in frm or "schedule" in frm:
        return "cron"
    if frm == "" or frm == owner.lower() or frm == "owner":
        return "you"
    cleaned = "".join(
        c if ((c.isascii() and c.isalnum()) or c in "_-") else "-" for c in frm
    )
    cleaned = cleaned[:20]
    return cleaned if cleaned else "you"


def worker_source(conn, payload, subject):
    """Row source for a worker-DM fold (worker-dm unification M1/M3). Prefers the
    stamped `payload.source` (the `"code"` emit path from `lanius code send` /
    `record_send`); for legacy unstamped rows it falls back to the durable-kind
    fact — a coding session ⇒ `"code"`. This is a fact read for the source
    LABEL, never display routing: it decides how a row is BADGED, not whether it
    is shown. Returns None when neither applies, so the caller falls back to its
    normal `source_for` guess."""
    claimed = payload.get("source")
    if isinstance(claimed, str):
        claimed = claimed.strip().lower()
        if claimed:
            return claimed
    if is_worker_session(conn, subject):
        return "code"
    return None


# ---- threading (ported from web.rs Conv / Convs) ----------------------------

class Convs:
    def __init__(self):
        self.map = {}
        self.order = []

    def ensure(self, session, seed_source, seed_ts):
        if session not in self.map:
            self.order.append(session)
            self.map[session] = {
                "title": "",
                "source": seed_source if seed_source is not None else "you",
                "last_ts": seed_ts,
                "first_ts": seed_ts,
                "message_count": 0,
                "preview": "",
                "last_role": "",
                "branched_from": None,
            }
        return self.map[session]

    def touch(self, session, role, text, count, ts):
        if text == "":
            return
        item = self.ensure(session, None, ts)
        if item["title"] == "" and role in ("you", "agent", "ask"):
            item["title"] = truncate_text(text, 72)
        item["preview"] = truncate_text(text, 110)
        item["last_role"] = role
        if ts != "":
            item["last_ts"] = ts
        if item["first_ts"] == "":
            item["first_ts"] = ts
        if count:
            item["message_count"] += 1


def branch_row_summary(payload):
    """web.rs branch_row_summary — flat branch edge for a list row, derived from
    the seeding event's structured `branched_from`, never invented."""
    bf = payload.get("branched_from")
    if not isinstance(bf, dict):
        return None
    event_id = bf.get("event_id")
    session = bf.get("session")
    session = session if isinstance(session, str) else None
    quote = bf.get("quote")
    quote = quote if isinstance(quote, str) else ""
    if event_id is None and session is None:
        return None
    return {
        "session": session,
        "event_id": event_id,
        "preview": truncate_text(quote, 110),
    }


def fold_human_payload(convs, session, payload, created):
    """web.rs fold_human_payload — fold one owner-mailbox event into its
    conversation the same way whether reached by correlation or by session."""
    if truthy(payload.get("failed")):
        err = payload.get("error")
        err = err if isinstance(err, str) else "the agent failed"
        convs.touch(session, "failed", err, True, created)
    elif payload.get("question") is not None:
        q = payload.get("question")
        q = q if isinstance(q, str) else ""
        convs.touch(session, "ask", q, True, created)
    elif isinstance(payload.get("text"), str):
        convs.touch(session, "agent", payload.get("text"), True, created)
    elif payload.get("answer") is not None:
        a = payload.get("answer")
        a = a if isinstance(a, str) else _compact(a)
        convs.touch(session, "you", a, True, created)


def session_for_event(row):
    """web.rs session_for_event: payload.session, else evt-<corr|id>."""
    payload = parse_payload(row["payload"])
    s = payload.get("session")
    if isinstance(s, str):
        return s
    if row["correlation_id"] is not None:
        return "evt-" + str(row["correlation_id"])
    return "evt-" + str(row["id"])


# ---- the conversation-list projection (ported from web.rs conversation_rows) -

def _rows(conn, sql, params):
    conn.row_factory = sqlite3.Row
    return conn.execute(sql, params).fetchall()


def conversation_rows(conn, agent, owner):
    """web.rs conversation_rows, relocated. Reconstruct-on-read over the ledger."""
    convs = Convs()
    corr_to_session = {}

    inbound = _rows(
        conn,
        "SELECT id, type, correlation_id, payload, state, sender, created_at "
        "FROM events WHERE type = ? ORDER BY id ASC LIMIT 5000",
        ("in/agent/" + agent,),
    )
    for row in inbound:
        payload = parse_payload(row["payload"])
        session = session_for_event(row)
        # A worker session on this exact-noun inbound topic (rare; owner/kernel
        # authored, so its payload.session is trusted) is folded like any thread
        # — it just carries the honest `"code"` source label instead of a
        # spelling guess. No worker DROP remains here (worker-dm M3).
        if row["correlation_id"] is not None:
            corr_to_session[row["correlation_id"]] = session
        source = worker_source(conn, payload, session)
        if source is None:
            source = source_for(session, row["sender"], payload, owner)
        created = row["created_at"] or ""
        conv = convs.ensure(session, source, created)
        if conv["branched_from"] is None:
            bf = branch_row_summary(payload)
            if bf is not None:
                conv["branched_from"] = bf
        prompt = payload.get("prompt")
        if not isinstance(prompt, str):
            prompt = payload.get("text")
        if isinstance(prompt, str):
            convs.touch(session, "you", prompt, True, created)

    # Owner → worker deliveries (worker-dm unification M1): a `lanius code
    # deliver` / `/api/code/deliver` prompt lands on the worker's mailbox
    # `in/agent/<agent>/<session>` — the EXTRA trailing segment the exact-noun
    # inbound query above misses. The worker session is the trailing topic
    # segment (a broker-addressed mailbox the owner delivered to — attribution
    # by topic, NOT a payload claim), the correlation is `code-deliver-*`, and
    # the payload carries the delivered prompt. Seed the worker conversation and
    # fold the prompt as the owner's turn; the correlation join below threads the
    # worker's correlated reply into the SAME conversation.
    delivery_prefix = "in/agent/" + agent + "/"
    deliveries = _rows(
        conn,
        "SELECT id, type, correlation_id, payload, state, sender, created_at "
        "FROM events WHERE type LIKE ? ORDER BY id ASC LIMIT 5000",
        (delivery_prefix + "%",),
    )
    for row in deliveries:
        etype = row["type"] or ""
        if not etype.startswith(delivery_prefix):
            continue
        session = etype[len(delivery_prefix):]
        # Only the direct worker mailbox (one trailing segment); no deeper topic.
        if session == "" or "/" in session:
            continue
        payload = parse_payload(row["payload"])
        if row["correlation_id"] is not None:
            corr_to_session[row["correlation_id"]] = session
        created = row["created_at"] or ""
        source = worker_source(conn, payload, session) or "code"
        convs.ensure(session, source, created)
        prompt = payload.get("prompt")
        if not isinstance(prompt, str):
            prompt = payload.get("text")
        if isinstance(prompt, str):
            convs.touch(session, "you", prompt, True, created)

    # Agent-first (ambient) sends: an in/human/<owner> row carrying the run's
    # session with no in/agent seed. Belongs to THIS agent only when THIS agent
    # sent it (broker-verified sender), and only when not already a prompted
    # thread (its correlation is not in the map).
    ambient = _rows(
        conn,
        "SELECT id, type, correlation_id, payload, state, sender, created_at "
        "FROM events WHERE type = ? ORDER BY id ASC LIMIT 5000",
        (human_mailbox(owner),),
    )
    for row in ambient:
        if row["sender"] != agent:
            continue
        if row["correlation_id"] is not None and row["correlation_id"] in corr_to_session:
            continue
        payload = parse_payload(row["payload"])
        session = payload.get("session")
        if not isinstance(session, str):
            continue
        # Attribution integrity (worker-dm M1 invariant): these rows are
        # broker-verified as sent by the agent NOUN. A payload naming a DISTINCT
        # worker principal (`code-*`, its own identity) the noun is not — is a
        # spoof; a worker's own sends are attributed by the sender column in the
        # worker-send pass below, NEVER by an agent-noun row's payload claim. A
        # native run session (sender's own run) is still honored. (A durable-fact
        # read to reject a forged claim, not display routing.)
        if session != row["sender"] and is_worker_session(conn, session):
            continue
        source = source_for(session, row["sender"], payload, owner)
        created = row["created_at"] or ""
        convs.ensure(session, source, created)
        fold_human_payload(convs, session, payload, created)

    # Worker → owner sends (worker-dm unification M1): an in/human/<owner> row
    # whose BROKER-VERIFIED sender is a coding session belonging to THIS
    # tool-noun. Attributed by the sender column (the worker speaks only as
    # itself), never by payload.session — a forged `session:` claim from a
    # non-worker sender is caught above and never reaches this pass. A reply
    # correlated to a delivery is threaded by the correlation join below (its
    # correlation is already in corr_to_session), so skip it here to avoid
    # double-seeding; a bare ambient send threads on its own sender.
    for row in ambient:
        sender = row["sender"]
        if sender is None:
            continue
        if not is_worker_session(conn, sender):
            continue
        if worker_session_noun(conn, sender) != agent:
            continue
        if row["correlation_id"] is not None and row["correlation_id"] in corr_to_session:
            continue
        payload = parse_payload(row["payload"])
        created = row["created_at"] or ""
        source = worker_source(conn, payload, sender) or "code"
        convs.ensure(sender, source, created)
        fold_human_payload(convs, sender, payload, created)

    # Correlation join: replies into a prompted thread.
    if corr_to_session:
        corrs = list(corr_to_session.keys())
        placeholders = ",".join("?" * len(corrs))
        human_rows = _rows(
            conn,
            "SELECT id, type, correlation_id, payload, state, sender, created_at "
            "FROM events WHERE type LIKE 'in/human/%' AND correlation_id IN ("
            + placeholders + ") ORDER BY id ASC LIMIT 5000",
            corrs,
        )
        for row in human_rows:
            corr = row["correlation_id"]
            if corr is None:
                continue
            session = corr_to_session.get(corr)
            if session is None:
                continue
            payload = parse_payload(row["payload"])
            created = row["created_at"] or ""
            fold_human_payload(convs, session, payload, created)

    # Durable transcript backfill for every seeded session (count=false: turns
    # were already counted from the in/agent + in/human events).
    sessions = list(convs.order)
    if sessions:
        placeholders = ",".join("?" * len(sessions))
        msg_rows = _rows(
            conn,
            "SELECT m.id, m.session_id, m.role, m.content, m.event_id, m.created_at, "
            "e.correlation_id, e.type AS event_type "
            "FROM messages m LEFT JOIN events e ON m.event_id = e.id "
            "WHERE m.session_id IN (" + placeholders + ") "
            "ORDER BY m.id ASC LIMIT 5000",
            sessions,
        )
        for r in msg_rows:
            session_id = r["session_id"] or ""
            content = r["content"]
            text = message_text(parse_stored(content)) if content is not None else ""
            role = normalize_role(r["role"] or "")
            convs.touch(session_id, role, text, False, r["created_at"] or "")

    out = []
    for k in convs.order:
        c = convs.map[k]
        source = c["source"] if c["source"] != "" else "you"
        if c["title"] == "":
            first = c["first_ts"] if c["first_ts"] != "" else c["last_ts"]
            title = (source + " conversation " + short_iso(first)).strip()
        else:
            title = c["title"]
        out.append({
            "session": k,
            "agent": agent,
            "title": title,
            "source": source,
            "last_ts": c["last_ts"] if c["last_ts"] != "" else c["first_ts"],
            "message_count": c["message_count"],
            "preview": c["preview"],
            "last_role": c["last_role"],
            "branched_from": c["branched_from"],
        })
    # Stable sort by last_ts DESC, keep the top 100 (Python sort is stable).
    out.sort(key=lambda a: a["last_ts"], reverse=True)
    return out[:100]


# ---- introspection (comms-package M4) ---------------------------------------

def conversation_branched_from(conn, session):
    """web.rs conversation_branched_from — the branch origin for a thread,
    with the full quoted text. Reconstructable straight from the ledger."""
    like = '%"session":"' + session + '"%'
    rows = _rows(
        conn,
        "SELECT payload FROM events WHERE type LIKE 'in/agent/%' AND payload LIKE ? "
        "ORDER BY id ASC LIMIT 200",
        (like,),
    )
    for r in rows:
        payload = parse_payload(r["payload"])
        if payload.get("session") != session:
            continue
        bf = payload.get("branched_from")
        if not isinstance(bf, dict):
            continue
        event_id = bf.get("event_id")
        parent = bf.get("session")
        parent = parent if isinstance(parent, str) else None
        if event_id is None and parent is None:
            continue
        quote = bf.get("quote")
        quote = quote if isinstance(quote, str) else ""
        return {
            "session": parent,
            "event_id": event_id,
            "quote": quote,
            "preview": truncate_text(quote, 110),
        }
    return None


def conversation_info(conn, session, owner="owner"):
    """comms-package M4 — the conversation-level introspection question:
    for a conversation, WHO is in it (broker-verified senders, never a payload
    field), on which channel/source, how is it composed (message + turn counts,
    time span), where did it branch from, and which correlation(s) thread it.

    Trust rule (same as recall/phonebook): participants come from the
    broker-verified `sender` column the kernel stamped, NEVER from a
    payload-claimed `sender` an agent could forge."""
    # in/agent events belonging to this session.
    agent_rows = _rows(
        conn,
        "SELECT id, type, correlation_id, payload, state, sender, created_at "
        "FROM events WHERE type LIKE 'in/agent/%' ORDER BY id ASC LIMIT 5000",
        (),
    )
    agent_events = [r for r in agent_rows if session_for_event(r) == session]
    corrs = set()
    for r in agent_events:
        if r["correlation_id"] is not None:
            corrs.add(r["correlation_id"])

    # in/human events tied to this session, by payload.session or by correlation.
    human_rows = _rows(
        conn,
        "SELECT id, type, correlation_id, payload, state, sender, created_at "
        "FROM events WHERE type LIKE 'in/human/%' ORDER BY id ASC LIMIT 5000",
        (),
    )
    human_events = []
    for r in human_rows:
        p = parse_payload(r["payload"])
        if p.get("session") == session or (
            r["correlation_id"] is not None and r["correlation_id"] in corrs
        ):
            human_events.append(r)
            if r["correlation_id"] is not None:
                corrs.add(r["correlation_id"])

    all_events = agent_events + human_events
    if not all_events:
        return None

    # Participants: broker-verified senders ONLY. A forged payload.sender is never
    # consulted.
    participants = sorted({r["sender"] for r in all_events if r["sender"] is not None})

    # Source/channel: labeled from the earliest event, exactly as the list does.
    seed = min(all_events, key=lambda r: r["id"])
    seed_payload = parse_payload(seed["payload"])
    source = source_for(session, seed["sender"], seed_payload, owner)

    # Transcript counts + time span.
    conn.row_factory = sqlite3.Row
    msg_rows = conn.execute(
        "SELECT role, created_at FROM messages WHERE session_id = ? ORDER BY id ASC",
        (session,),
    ).fetchall()
    message_count = len(msg_rows)
    turn_count = sum(1 for m in msg_rows if m["role"] in ("user", "assistant"))

    ts_all = [r["created_at"] for r in all_events if r["created_at"]]
    ts_all += [m["created_at"] for m in msg_rows if m["created_at"]]
    first_ts = min(ts_all) if ts_all else None
    last_ts = max(ts_all) if ts_all else None

    return {
        "session": session,
        "participants": participants,
        "source": source,
        "channels": [source],
        "message_count": message_count,
        "turn_count": turn_count,
        "event_count": len(all_events),
        "first_ts": first_ts,
        "last_ts": last_ts,
        "branched_from": conversation_branched_from(conn, session),
        "correlations": sorted(corrs),
    }


# ---- CLI (used by the M1 golden test; the daemon imports the functions) ------

if __name__ == "__main__":
    import sys

    def ro(db):
        c = sqlite3.connect("file:" + db + "?mode=ro", uri=True, timeout=5)
        c.row_factory = sqlite3.Row
        return c

    cmd = sys.argv[1]
    if cmd == "conversations":
        db, agent, owner = sys.argv[2], sys.argv[3], sys.argv[4]
        with ro(db) as c:
            print(json.dumps(conversation_rows(c, agent, owner), ensure_ascii=False))
    elif cmd == "conversation_info":
        db, session = sys.argv[2], sys.argv[3]
        owner = sys.argv[4] if len(sys.argv) > 4 else "owner"
        with ro(db) as c:
            print(json.dumps(conversation_info(c, session, owner), ensure_ascii=False))
    else:
        sys.stderr.write("usage: comms_view.py conversations|conversation_info ...\n")
        sys.exit(2)
