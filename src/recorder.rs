//! The recorder: disk is a set of subscription patterns (docs/bus.md).
//!
//! Every happening is published with a topic (today via trace::write; via the
//! bus's publish() later). The recorder is an in-process consumer that decides
//! persistence by matching the topic against ordered rules — first match wins.
//! It is deliberately independent of any broker: "the black box doesn't depend
//! on the radio." Rules live in root-level recorder.toml; absent that, a
//! built-in default preserves prior behavior (record everything to trace).
//!
//! Sinks over the trace stream are `trace` (append to trace.jsonl) and `none`
//! (live-only, never touches disk). The `ledger` sink is the emit() path —
//! in/# is sqlite-backed by construction, not via this recorder — so it is
//! accepted as an alias for `trace` here and documented as such.

use crate::paths::Root;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Trace,
    None,
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    #[serde(default)]
    record: Vec<RuleDecl>,
}

#[derive(Debug, Deserialize)]
struct RuleDecl {
    #[serde(rename = "match")]
    match_filter: String,
    sink: String,
}

struct Rule {
    filter: String,
    sink: Sink,
}

pub struct Recorder {
    rules: Vec<Rule>,
}

impl Recorder {
    /// Built-in default when recorder.toml is absent or unparsable: per-file
    /// obs/fs/ deltas are live-only (high volume — cargo build touches
    /// thousands; the obs/agent/+/+/fs/summary still records), everything
    /// else hits trace.
    fn default_rules() -> Vec<Rule> {
        vec![
            Rule {
                filter: "obs/fs/#".into(),
                sink: Sink::None,
            },
            Rule {
                filter: "#".into(),
                sink: Sink::Trace,
            },
        ]
    }

    pub fn load(root: &Root) -> Recorder {
        let path = root.recorder_file();
        let Ok(s) = std::fs::read_to_string(&path) else {
            return Recorder {
                rules: Self::default_rules(),
            };
        };
        let parsed: RuleFile = match toml::from_str(&s) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "[recorder] {} parse error, using defaults: {e}",
                    path.display()
                );
                return Recorder {
                    rules: Self::default_rules(),
                };
            }
        };
        let mut rules = Vec::new();
        for r in parsed.record {
            if !crate::topic::valid_filter(&r.match_filter) {
                eprintln!("[recorder] skipping invalid filter {:?}", r.match_filter);
                continue;
            }
            let sink = match r.sink.as_str() {
                "trace" | "ledger" => Sink::Trace,
                "none" => Sink::None,
                other => {
                    eprintln!(
                        "[recorder] unknown sink {other:?} for {:?}, treating as none",
                        r.match_filter
                    );
                    Sink::None
                }
            };
            rules.push(Rule {
                filter: r.match_filter,
                sink,
            });
        }
        if rules.is_empty() {
            rules = Self::default_rules();
        }
        Recorder { rules }
    }

    /// First matching rule wins; an unmatched topic is recorded (fail toward
    /// keeping the black box's data rather than silently dropping it).
    pub fn sink_for(&self, topic: &str) -> Sink {
        for r in &self.rules {
            if crate::topic::matches(&r.filter, topic) {
                return r.sink;
            }
        }
        Sink::Trace
    }
}

/// Process-global recorder, loaded once from the root. trace::write is called
/// from everywhere with only the root in hand; threading a Recorder through
/// every call site would be invasive, and a process serves one root. Rule
/// changes are picked up on daemon/exec restart (documented).
static RECORDER: OnceLock<Recorder> = OnceLock::new();

pub fn get(root: &Root) -> &'static Recorder {
    RECORDER.get_or_init(|| Recorder::load(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(rules: &[(&str, Sink)]) -> Recorder {
        Recorder {
            rules: rules
                .iter()
                .map(|(f, s)| Rule {
                    filter: f.to_string(),
                    sink: *s,
                })
                .collect(),
        }
    }

    #[test]
    fn first_match_wins() {
        let r = rec(&[("obs/fs/#", Sink::None), ("#", Sink::Trace)]);
        assert_eq!(r.sink_for("obs/fs/Users/tim/x.rs"), Sink::None);
        assert_eq!(r.sink_for("obs/agent/main/s1/llm/request"), Sink::Trace);
        assert_eq!(r.sink_for("signal/pain"), Sink::Trace);
    }

    #[test]
    fn specific_before_catchall() {
        let r = rec(&[
            ("obs/ui/#", Sink::None),
            ("obs/#", Sink::Trace),
            ("#", Sink::Trace),
        ]);
        assert_eq!(r.sink_for("obs/ui/laptop/keydown"), Sink::None);
        assert_eq!(r.sink_for("obs/agent/main/s1/tool/shell/call"), Sink::Trace);
    }

    #[test]
    fn unmatched_records() {
        let r = rec(&[("in/#", Sink::None)]);
        assert_eq!(r.sink_for("something/else"), Sink::Trace);
    }

    #[test]
    fn default_silences_per_file_fs_keeps_summary() {
        let r = Recorder {
            rules: Recorder::default_rules(),
        };
        assert_eq!(r.sink_for("obs/fs/Users/tim/code/x.rs"), Sink::None);
        assert_eq!(r.sink_for("obs/agent/main/s1/fs/summary"), Sink::Trace);
        assert_eq!(r.sink_for("signal/pain"), Sink::Trace);
    }
}
