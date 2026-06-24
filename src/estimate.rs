//! Work estimation (docs/handoffs/work-estimation.md, E1–E3).
//!
//! Estimation is a PACKAGE-shaped capability with NO kernel data model: there is
//! no estimation table and no estimation type in the ledger. All state lives in
//! the existing substrates —
//!
//!   * the running estimate is an `estimate` memory BLOCK (session scope, JSON
//!     content) plus an `obs/estimate/<session>` event that marks the count-from
//!     boundary (E1);
//!   * actuals are READ from the obs projection (`src/code_projection.rs`,
//!     `code_session_stats`/`code_session_events`) and from per-harness token
//!     usage × a package-local `pricing.toml` (E2);
//!   * the learned heuristic is a durable `estimation` block (agent scope) that
//!     each retro appends a dated miss to, and each future E1 reads (E3).
//!
//! This module is the pure core: the `Estimate` shape, the pricing map, the
//! actuals/variance computation over a projection `Connection`, and the retro
//! text. The CLI glue (`elanus estimate set/actual/retro`) lives in
//! `src/estimatecli.rs`; the hook wiring lives in `src/codeagent.rs`. Nothing
//! here touches a kernel table that estimation owns — it only reads the obs
//! projection and writes blocks through `context_store`.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// The well-known name of the per-session running-estimate block (E1). Session
/// scope: it binds to the session it estimates, so it shows in that session's
/// context and is read back by the retro.
pub const ESTIMATE_BLOCK: &str = "estimate";

/// The well-known name of the per-session computed estimate-vs-actual block (E2).
pub const ESTIMATE_VS_ACTUAL_BLOCK: &str = "estimate-vs-actual";

/// The well-known name of the durable learned-heuristic block (E3). Agent scope:
/// it accumulates dated misses across sessions and is what a future E1 reads.
pub const ESTIMATION_BLOCK: &str = "estimation";

/// A multi-dimensional estimate. Dollars is the headline normalizer (the
/// cross-model axis); turns/tokens/wall-clock are the dimensions that produce it.
/// Every dimension is optional so a partial estimate (e.g. "8 turns" with no
/// dollar guess) still records — the agent declares whatever it can.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Estimate {
    /// Headline dollars (the great normalizer across models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dollars: Option<f64>,
    /// Agent turns (user/assistant exchanges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<i64>,
    /// Total tokens (in + out).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    /// Wall-clock milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_ms: Option<i64>,
    /// RFC3339 timestamp the estimate was declared — the count-from boundary.
    /// Actuals are computed from this moment onward.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Estimate {
    /// Serialize to the JSON the `estimate` block stores (pretty for legibility
    /// in `context render`).
    pub fn to_block_content(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Parse an `estimate` block's JSON content back into an `Estimate`.
    pub fn from_block_content(s: &str) -> Result<Self> {
        serde_json::from_str(s).context("parsing estimate block JSON")
    }
}

/// The package-local pricing map: model id -> dollars-per-token. The cost-
/// visibility journey (docs/journeys/03-cost-visibility.md) is explicit that
/// dollars are unknown until a pricing source exists; this is that source,
/// owned by the estimation package (`pricing.toml`), NOT a kernel table. A model
/// absent from the map yields no dollar figure (we report what we can — never a
/// fabricated cost).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pricing {
    /// model id -> { input, output } dollars-per-token. Both optional; a missing
    /// side simply contributes nothing.
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

/// Per-model token pricing in dollars per SINGLE token (so `1e-6` is $1/M tokens).
/// Kept in $/token — not $/Mtok — so the multiply is unit-clean and the toml is
/// explicit; the package README documents the unit.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModelPrice {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
}

impl Pricing {
    /// Load the pricing map from a `pricing.toml`. A missing file is NOT an error
    /// — it yields an empty map (dollars then unavailable, which the report states
    /// honestly), so the rest of estimation never blocks on pricing.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Pricing::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading pricing {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing pricing {}", path.display()))
    }

    /// Dollars for `input_tokens`/`output_tokens` at `model`'s price, or `None`
    /// when the model is not in the map (cost genuinely unknown).
    pub fn dollars_for(&self, model: &str, input_tokens: i64, output_tokens: i64) -> Option<f64> {
        let p = self.models.get(model)?;
        let mut total = 0.0;
        let mut any = false;
        if let Some(rate) = p.input {
            total += rate * input_tokens as f64;
            any = true;
        }
        if let Some(rate) = p.output {
            total += rate * output_tokens as f64;
            any = true;
        }
        if any {
            Some(total)
        } else {
            None
        }
    }
}

/// The actuals computed for a session FROM the estimate boundary onward (E2).
/// Every dimension is what the obs projection could supply; tokens/dollars are
/// `None` when the harness did not surface usage or the model is unpriced.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Actuals {
    pub turns: i64,
    pub tool_calls: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dollars: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One dimension's estimate-vs-actual line: the estimate, the actual, and the
/// signed variance (actual - estimate). All `f64` so dollars and counts share a
/// shape; `None` where the dimension is unavailable on either side.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Variance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    /// actual - estimate (positive = over, negative = under). `None` unless both
    /// sides are present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
}

impl Variance {
    fn of(estimate: Option<f64>, actual: Option<f64>) -> Self {
        let delta = match (estimate, actual) {
            (Some(e), Some(a)) => Some(a - e),
            _ => None,
        };
        Variance {
            estimate,
            actual,
            delta,
        }
    }
}

/// The full estimate-vs-actual report for a session (E2). Headline is dollars;
/// the other dimensions follow. Serializes straight into the `estimate-vs-actual`
/// computed block.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub session: String,
    /// dollars is the headline axis (listed first).
    pub dollars: Variance,
    pub turns: Variance,
    pub tool_calls: Variance,
    pub tokens: Variance,
    pub wall_clock_ms: Variance,
    /// True when the report's dollars are unavailable on EITHER side (no priced
    /// token usage) — the cost-visibility caveat, surfaced so a reader never
    /// mistakes a missing dollar figure for "$0".
    pub dollars_unavailable: bool,
}

impl Report {
    /// Build the report from an estimate and the computed actuals.
    pub fn build(session: &str, est: &Estimate, act: &Actuals) -> Self {
        let dollars = Variance::of(est.dollars, act.dollars);
        Report {
            session: session.to_string(),
            dollars_unavailable: dollars.actual.is_none(),
            dollars,
            turns: Variance::of(est.turns.map(|v| v as f64), Some(act.turns as f64)),
            tool_calls: Variance::of(None, Some(act.tool_calls as f64)),
            tokens: Variance::of(
                est.tokens.map(|v| v as f64),
                act.tokens.map(|v| v as f64),
            ),
            wall_clock_ms: Variance::of(
                est.wall_clock_ms.map(|v| v as f64),
                act.wall_clock_ms.map(|v| v as f64),
            ),
        }
    }

    /// The computed-block content (pretty JSON) the `estimate-vs-actual` block holds.
    pub fn to_block_content(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// A terse one-line retro note for the durable `estimation` block (E3). Records
    /// the miss in dollars + turns (the two most legible axes), tagged with the
    /// boundary date. The LLM "why it missed" reflection is a documented follow-on;
    /// this is the MVP heuristic note.
    pub fn retro_note(&self, date: &str) -> String {
        let dollars = miss_phrase("$", self.dollars.estimate, self.dollars.actual, 2);
        let turns = miss_phrase("", self.turns.estimate, self.turns.actual, 0);
        let lean = self.lean_hint();
        format!("{date} — {dollars}, {turns}{lean}")
    }

    /// A terse directional hint ("underestimated tool-heavy work" / "overestimated")
    /// from the dollar (else turn) delta. Empty when neither side is comparable.
    fn lean_hint(&self) -> String {
        let delta = self.dollars.delta.or(self.turns.delta);
        match delta {
            Some(d) if d > 0.0 => "; underestimated".to_string(),
            Some(d) if d < 0.0 => "; overestimated".to_string(),
            Some(_) => "; on the mark".to_string(),
            None => String::new(),
        }
    }
}

/// "$0.40 → $0.62 (+0.22)" style phrase for one dimension, with a missing side
/// rendered as "?".
fn miss_phrase(unit: &str, estimate: Option<f64>, actual: Option<f64>, prec: usize) -> String {
    let fmt = |v: Option<f64>| match v {
        Some(x) => format!("{unit}{x:.prec$}"),
        None => "?".to_string(),
    };
    let delta = match (estimate, actual) {
        (Some(e), Some(a)) => format!(" ({:+.prec$})", a - e),
        _ => String::new(),
    };
    let label = if unit == "$" { "" } else { " turns" };
    format!("estimated {} actual {}{delta}{label}", fmt(estimate), fmt(actual))
}

/// Compute the actuals for `session` from the obs projection, counting only
/// events at-or-after the estimate boundary `since` (E2). Turns = user-message
/// events; tool_calls = `tool/*/call` events; wall-clock = boundary→last-event
/// span; tokens come from `code_session_stats` (the per-harness usage the
/// projection already folds, when present); dollars = tokens × pricing for the
/// session's model.
///
/// `conn` is a projection database (the same `code_session_stats` /
/// `code_session_events` schema `code_projection.rs` writes). A session with no
/// rows yields empty actuals (counts of 0, tokens/dollars `None`) — never an error.
pub fn compute_actuals(
    conn: &Connection,
    session: &str,
    since: Option<&str>,
    pricing: &Pricing,
) -> Result<Actuals> {
    // The boundary: count events with ts >= `since` (or all, when no boundary).
    // ts is RFC3339 with a fixed millis format, so lexicographic compare is
    // chronological.
    let since = since.unwrap_or("");

    // Turns and tool calls from the event timeline. `kind` is the obs leaf
    // (e.g. "user/message", "tool/edit/call"). We count post-boundary events.
    let mut turns = 0i64;
    let mut tool_calls = 0i64;
    let mut last_ts: Option<String> = None;
    if let Ok(mut stmt) = conn.prepare(
        "SELECT kind, ts FROM code_session_events
          WHERE elanus_session = ?1 AND (ts IS NULL OR ts >= ?2)
          ORDER BY ts, id",
    ) {
        let rows = stmt.query_map(rusqlite::params![session, since], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (kind, ts) = row?;
            if let Some(k) = &kind {
                if k == "user/message" {
                    turns += 1;
                } else if k.starts_with("tool/") && k.ends_with("/call") {
                    tool_calls += 1;
                }
            }
            if ts.is_some() {
                last_ts = ts;
            }
        }
    }

    // Wall-clock: boundary → the last post-boundary event. When there is no
    // boundary, fall back to the session's started_at→ended_at span.
    let stat = load_stat(conn, session)?;
    let wall_clock_ms = match (since.is_empty(), &last_ts, &stat) {
        // No declared boundary: use the session's own start→end.
        (true, _, Some(s)) => span_ms(s.started_at.as_deref(), s.ended_at.as_deref()),
        // Boundary declared: from the boundary to the last event seen after it.
        (false, Some(end), _) => span_ms(Some(since), Some(end)),
        _ => None,
    };

    // Tokens: the projection folds per-harness usage into the stat row. Absent
    // (a harness that never surfaced usage) → None, handled gracefully.
    let (input_tokens, output_tokens, model) = match &stat {
        Some(s) => (s.input_tokens, s.output_tokens, s.model.clone()),
        None => (0, 0, None),
    };
    let has_tokens = input_tokens > 0 || output_tokens > 0;
    let tokens = has_tokens.then_some(input_tokens + output_tokens);

    // Dollars: only when we have BOTH token usage and a price for the model.
    let dollars = match (&model, has_tokens) {
        (Some(m), true) => pricing.dollars_for(m, input_tokens, output_tokens),
        _ => None,
    };

    Ok(Actuals {
        turns,
        tool_calls,
        wall_clock_ms,
        tokens,
        input_tokens: has_tokens.then_some(input_tokens),
        output_tokens: has_tokens.then_some(output_tokens),
        dollars,
        model,
    })
}

/// The few `code_session_stats` fields the actuals need. Defined locally (not
/// imported from `code_projection`) so estimation reads the projection as a plain
/// derived index, owning nothing.
struct Stat {
    started_at: Option<String>,
    ended_at: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    model: Option<String>,
}

fn load_stat(conn: &Connection, session: &str) -> Result<Option<Stat>> {
    let row = conn
        .query_row(
            "SELECT started_at, ended_at, input_tokens, output_tokens, model
               FROM code_session_stats WHERE elanus_session = ?1",
            rusqlite::params![session],
            |r| {
                Ok(Stat {
                    started_at: r.get(0)?,
                    ended_at: r.get(1)?,
                    input_tokens: r.get(2)?,
                    output_tokens: r.get(3)?,
                    model: r.get(4)?,
                })
            },
        )
        .optional()
        // The projection table may not exist yet (no coding session ran): that is
        // "no stats", not an error.
        .unwrap_or(None);
    Ok(row)
}

/// Milliseconds between two RFC3339 timestamps, or None if either is absent or
/// unparseable.
fn span_ms(start: Option<&str>, end: Option<&str>) -> Option<i64> {
    let (s, e) = (start?, end?);
    let s = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let e = chrono::DateTime::parse_from_rfc3339(e).ok()?;
    Some((e - s).num_milliseconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_block_json_roundtrips() {
        let e = Estimate {
            dollars: Some(0.40),
            turns: Some(8),
            tokens: Some(120_000),
            wall_clock_ms: Some(600_000),
            at: Some("2026-06-23T00:00:00.000Z".into()),
        };
        let s = e.to_block_content();
        let back = Estimate::from_block_content(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn pricing_dollars_and_missing_model() {
        let toml = r#"
[models."gpt-5"]
input = 0.000001
output = 0.000002
"#;
        let p: Pricing = toml::from_str(toml).unwrap();
        // 100 in @ $1/M + 50 out @ $2/M = 0.0001 + 0.0001 = 0.0002.
        let d = p.dollars_for("gpt-5", 100, 50).unwrap();
        assert!((d - 0.0002).abs() < 1e-12, "got {d}");
        // An unpriced model yields None (cost genuinely unknown, never $0).
        assert!(p.dollars_for("unknown-model", 100, 50).is_none());
    }

    #[test]
    fn variance_delta_is_actual_minus_estimate() {
        let v = Variance::of(Some(8.0), Some(13.0));
        assert_eq!(v.delta, Some(5.0));
        // A one-sided dimension has no delta.
        assert_eq!(Variance::of(None, Some(13.0)).delta, None);
    }

    #[test]
    fn retro_note_records_the_miss_with_direction() {
        let est = Estimate {
            dollars: Some(0.40),
            turns: Some(8),
            ..Default::default()
        };
        let act = Actuals {
            turns: 13,
            tool_calls: 20,
            dollars: Some(0.62),
            ..Default::default()
        };
        let r = Report::build("code-x", &est, &act);
        let note = r.retro_note("2026-06-23");
        assert!(note.contains("$0.40"), "{note}");
        assert!(note.contains("$0.62"), "{note}");
        assert!(note.contains("underestimated"), "{note}");
    }
}
