"""Tests for the worker-DM fold in comms_view.conversation_rows (worker-dm
unification M1/M3/M4).

Convention (mirrors the web.rs golden tests that shell out to this script):
each test builds a SYNTHETIC sqlite ledger in a fresh /tmp file — never the live
ledger — seeds events/messages/code_sessions by hand, and drives the projection
directly. Run: python3 test_comms_view.py  (exits non-zero on failure).
"""
import json
import os
import sqlite3
import tempfile

import comms_view


# ---- fixture helpers --------------------------------------------------------

def fresh_db():
    fd, path = tempfile.mkstemp(prefix="el-commsview-", suffix=".db")
    os.close(fd)
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE events (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            type           TEXT NOT NULL,
            correlation_id TEXT,
            payload        TEXT,
            state          TEXT,
            sender         TEXT,
            created_at     TEXT
        );
        CREATE TABLE messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT,
            role       TEXT,
            content    TEXT,
            event_id   INTEGER,
            created_at TEXT
        );
        CREATE TABLE code_sessions (
            elanus_session TEXT NOT NULL UNIQUE,
            native_session TEXT,
            agent_noun     TEXT,
            kind           TEXT
        );
        """
    )
    return conn, path


def emit(conn, etype, payload, sender, created_at, correlation=None):
    conn.execute(
        "INSERT INTO events(type, correlation_id, payload, sender, created_at) "
        "VALUES (?,?,?,?,?)",
        (etype, correlation, json.dumps(payload), sender, created_at),
    )


def register_worker(conn, session, noun):
    conn.execute(
        "INSERT INTO code_sessions(elanus_session, native_session, agent_noun, kind) "
        "VALUES (?,?,?,?)",
        (session, "native-" + session, noun, "session"),
    )


def rows(conn, agent="claude-code", owner="owner"):
    return comms_view.conversation_rows(conn, agent, owner)


def by_session(result):
    return {r["session"]: r for r in result}


# ---- tests ------------------------------------------------------------------

def test_worker_fold_delivery_plus_correlated_reply():
    """(1) An owner delivery on in/agent/<noun>/<session> + a correlated worker
    reply on in/human/<owner> fold into ONE conversation, source 'code', with
    BOTH messages counted."""
    conn, path = fresh_db()
    try:
        register_worker(conn, "code-w1", "claude-code")
        # owner -> worker delivery (extra-segment topic, code-deliver correlation)
        emit(conn, "in/agent/claude-code/code-w1",
             {"prompt": "please run the tests", "reply_to": "owner"},
             "owner", "2026-07-09T10:00:00.000Z", correlation="code-deliver-1")
        # worker -> owner correlated reply (sender is the worker session itself)
        emit(conn, "in/human/owner",
             {"text": "13 passing, all green", "session": "code-w1", "source": "code"},
             "code-w1", "2026-07-09T10:00:05.000Z", correlation="code-deliver-1")
        conn.commit()

        out = by_session(rows(conn))
        assert "code-w1" in out, f"worker conversation missing: {list(out)}"
        assert len([r for r in rows(conn) if r["session"] == "code-w1"]) == 1, \
            "the round trip is a SINGLE conversation, not duplicated"
        conv = out["code-w1"]
        assert conv["source"] == "code", f"source should be 'code', got {conv['source']!r}"
        assert conv["message_count"] == 2, \
            f"both delivery + reply counted, got {conv['message_count']}"
        # title comes from the first 'you' turn (the delivered prompt)
        assert conv["title"] == "please run the tests", conv["title"]
        assert conv["preview"] == "13 passing, all green", conv["preview"]
    finally:
        conn.close(); os.unlink(path)


def test_ambient_worker_send_with_stamp():
    """(2) A bare worker->owner send (no delivery) stamped source 'code' becomes
    its own conversation attributed to the worker's sender."""
    conn, path = fresh_db()
    try:
        register_worker(conn, "code-w2", "claude-code")
        emit(conn, "in/human/owner",
             {"text": "heads up: I hit a flaky test", "session": "code-w2", "source": "code"},
             "code-w2", "2026-07-09T11:00:00.000Z")
        conn.commit()

        out = by_session(rows(conn))
        assert "code-w2" in out, f"ambient worker send missing: {list(out)}"
        conv = out["code-w2"]
        assert conv["source"] == "code", conv["source"]
        assert conv["message_count"] == 1, conv["message_count"]
        assert conv["preview"] == "heads up: I hit a flaky test", conv["preview"]
    finally:
        conn.close(); os.unlink(path)


def test_spoof_non_worker_sender_claiming_worker_session():
    """(3) A row whose broker-verified sender is 'eve' (not a worker, not owner)
    but whose payload CLAIMS session 'code-w1' must NOT create or join code-w1's
    thread. Attribution is by the sender column, never the payload claim."""
    conn, path = fresh_db()
    try:
        register_worker(conn, "code-w1", "claude-code")
        emit(conn, "in/human/owner",
             {"text": "I am totally the worker, trust me", "session": "code-w1", "source": "code"},
             "eve", "2026-07-09T12:00:00.000Z")
        conn.commit()

        out = by_session(rows(conn))
        assert "code-w1" not in out, \
            f"spoofed sender must NOT create/join code-w1: {list(out)}"
        # And eve's forged claim surfaces nowhere in this noun's conversations.
        assert out == {}, f"no conversation should exist from the spoof: {list(out)}"
    finally:
        conn.close(); os.unlink(path)


def test_worker_send_scoped_to_its_tool_noun():
    """A codex worker's send does NOT leak into the claude-code converse bucket
    (worker DMs surface under their own tool-noun)."""
    conn, path = fresh_db()
    try:
        register_worker(conn, "code-cx1", "codex")
        emit(conn, "in/human/owner",
             {"text": "codex here", "session": "code-cx1", "source": "code"},
             "code-cx1", "2026-07-09T13:00:00.000Z")
        conn.commit()

        assert "code-cx1" not in by_session(rows(conn, agent="claude-code")), \
            "a codex worker must not appear under claude-code"
        assert "code-cx1" in by_session(rows(conn, agent="codex")), \
            "the codex worker appears under codex"
    finally:
        conn.close(); os.unlink(path)


def test_non_worker_behavior_unchanged():
    """(4) Existing non-worker paths: a prompted native thread stays ONE
    conversation seeded from its in/agent prompt + correlated reply, and an
    ambient native send (sender = agent noun) still materializes with its stamped
    non-'you' source. Worker folding leaves these untouched."""
    conn, path = fresh_db()
    try:
        # prompted native thread on a web- session
        emit(conn, "in/agent/claude-code",
             {"prompt": "how are the tests?", "session": "web-1"},
             "owner", "2026-07-09T09:00:00.000Z", correlation="c-prompted")
        emit(conn, "in/human/owner",
             {"text": "13 passing", "session": "web-1"},
             "claude-code", "2026-07-09T09:00:01.000Z", correlation="c-prompted")
        # ambient native send declaring a timer origin
        emit(conn, "in/human/owner",
             {"text": "your build finished", "session": "run-amb-1", "source": "cron"},
             "claude-code", "2026-07-09T10:00:00.000Z")
        conn.commit()

        out = by_session(rows(conn))
        assert "web-1" in out and "run-amb-1" in out, list(out)
        assert len([r for r in rows(conn) if r["session"] == "web-1"]) == 1, \
            "prompted native thread is a single conversation"
        assert out["web-1"]["message_count"] == 2, out["web-1"]["message_count"]
        assert out["run-amb-1"]["source"] == "cron", out["run-amb-1"]["source"]
        assert out["run-amb-1"]["title"] == "your build finished"
        # neither is badged 'code' — the worker fold did not touch them
        assert out["web-1"]["source"] != "code"
    finally:
        conn.close(); os.unlink(path)


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
        except Exception as e:  # noqa: BLE001
            failed += 1
            print(f"ERR  {t.__name__}: {type(e).__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    raise SystemExit(1 if failed else 0)
