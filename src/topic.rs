//! MQTT 5 topic names and filters (spec §4.7) — the single pattern language
//! for handler subscriptions, throttles, and (later) recorder rules, grants,
//! and hook points. This module is also the seed of the v2 micro-broker's
//! topic matching: it must stay dependency-free and exactly spec-shaped.

/// Does `filter` match `topic` per MQTT §4.7? `+` matches exactly one level;
/// `#` (final level only) matches any number of levels including zero —
/// `sport/#` matches `sport` itself. Filters starting with a wildcard never
/// match topics whose first level starts with `$` [MQTT-4.7.2-1].
/// A malformed filter (see `valid_filter`) matches nothing.
pub fn matches(filter: &str, topic: &str) -> bool {
    if !valid_filter(filter) {
        return false;
    }
    let f: Vec<&str> = filter.split('/').collect();
    let t: Vec<&str> = topic.split('/').collect();
    if (f[0] == "#" || f[0] == "+") && t[0].starts_with('$') {
        return false;
    }
    let mut i = 0;
    loop {
        match (f.get(i), t.get(i)) {
            (Some(&"#"), _) => return true, // validity guaranteed it's last
            (Some(&"+"), Some(_)) => {}
            (Some(fs), Some(ts)) if fs == ts => {}
            (None, None) => return true,
            _ => return false,
        }
        i += 1;
    }
}

/// Filter validity per [MQTT-4.7.1-1..2]: nonempty; `#` only as the entire
/// last level; `+` only as an entire level.
pub fn valid_filter(filter: &str) -> bool {
    if filter.is_empty() {
        return false;
    }
    let segs: Vec<&str> = filter.split('/').collect();
    let last = segs.len() - 1;
    segs.iter().enumerate().all(|(i, s)| match *s {
        "#" => i == last,
        "+" => true,
        s => !s.contains('#') && !s.contains('+'),
    })
}

/// Topic-name validity: nonempty and wildcard-free [MQTT-4.7.3-1, 4.7.1-1].
pub fn valid_name(topic: &str) -> bool {
    !topic.is_empty() && !topic.contains('#') && !topic.contains('+')
}

/// Encode an identifier (session id, tool name, fs path segment) as a single
/// topic level: percent-encode the wildcard characters, `%` itself, and `/`
/// so the value cannot add or match levels. Filters are authored against the
/// encoded form; nothing ever decodes for matching.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            '+' => out.push_str("%2B"),
            '#' => out.push_str("%23"),
            '/' => out.push_str("%2F"),
            _ => out.push(c),
        }
    }
    out
}

/// Does `wide` (as a filter) cover `narrow` (as a filter)? Returns true iff
/// every concrete topic matched by `narrow` is also matched by `wide` —
/// i.e. the topic-set denoted by `narrow` is a subset (⊆) of the topic-set
/// denoted by `wide`. Used at mint to assert `child ⊆ spawner` for the bus
/// publish/subscribe dimensions (docs/handoffs/authority-delegation.md M2,
/// docs/security.md entry 22).
///
/// ## Decision rules (decidable, conservative)
///
/// The algorithm is structurally recursive over filter levels:
///
/// 1. `wide == "#"` → true (wide is the universal set; narrow ⊆ universal).
/// 2. `wide == narrow` → true (equal sets; trivially ⊆).
/// 3. `wide` ends with `/#` or `#` at the last level:
///    Strip the `/#` / `#` suffix from `wide` to get a `prefix`.
///    If `narrow` starts with that same `prefix` (i.e. `narrow` is
///    `prefix`, `prefix/#`, `prefix/…`, or `prefix/+/…`), then every topic
///    `narrow` matches is under `prefix/…`, so ⊆ holds.
///    More precisely: `narrow`'s first `len(prefix_levels)` levels must
///    all be subsumed by the corresponding `wide` levels — handled by
///    recursing on the matched prefix levels.
/// 4. Level-by-level comparison: walk both filters level by level.
///    At each position `i`:
///    - `wide[i] == "#"` → covered (already handled above, but belt-and-suspenders).
///    - `wide[i] == "+"` and `narrow[i]` is any single level (incl. `+`) → covered
///      at this level (+ in wide covers any single level in narrow, including another +).
///    - `wide[i] == narrow[i]` (literal match) → continue.
///    - Otherwise → NOT covered (wide is too narrow; when in doubt, false).
///
///    After all levels: covered iff both are exhausted simultaneously.
///
/// ## Conservative bias
///
/// When the relationship is ambiguous or the inputs are malformed, the function
/// returns `false` (deny). This is the "when in doubt, return false" doctrine
/// from the spec: an overstated guarantee (saying ⊆ when it might not hold) is
/// itself a defect (docs/security.md entry 22).
///
/// ## Examples
///
/// ```text
/// covers("obs/#", "obs/agent/claude-code/code-abc/#")  → true
/// covers("obs/agent/+/#", "obs/agent/claude-code/#")    → true
/// covers("obs/agent/+/#", "obs/agent/+/code-abc/#")     → true
/// covers("obs/agent/+/code-abc/#", "obs/agent/+/#")     → false  (narrow is wider)
/// covers("obs/+/#", "obs/#")                             → false  (obs/# includes obs itself)
/// covers("a/b", "a/b")                                   → true
/// covers("a/+", "a/b")                                   → true
/// covers("a/b", "a/+")                                   → false  (a/+ is wider than a/b)
/// covers("#", "anything/#")                              → true
/// ```
pub fn covers(wide: &str, narrow: &str) -> bool {
    // Malformed filters cover nothing (and cannot be covered).
    if !valid_filter(wide) || !valid_filter(narrow) {
        return false;
    }
    // Equal strings → same filter, trivially ⊆.
    if wide == narrow {
        return true;
    }
    let wf: Vec<&str> = wide.split('/').collect();
    let nf: Vec<&str> = narrow.split('/').collect();

    // MQTT $-topic rule [MQTT-4.7.2-1] (the same rule `matches` honors): a filter
    // whose FIRST level is a wildcard (`#` or `+`) does not match a topic whose
    // first level begins with `$`. So a root-wildcard `wide` does NOT cover a
    // `narrow` rooted at a `$` literal — `narrow` admits `$…` topics that `wide`
    // would skip. Without this guard `covers("#", "$x/#")` would falsely report
    // true (an overstated ⊆ — itself a defect, security.md entry 22). A `narrow`
    // first level of `+`/`#` is itself $-skipping, so only a literal `$…` matters.
    if let (Some(w0), Some(n0)) = (wf.first(), nf.first()) {
        if (*w0 == "#" || *w0 == "+") && n0.starts_with('$') {
            return false;
        }
    }

    // Walk level by level.
    let mut i = 0;
    loop {
        match (wf.get(i), nf.get(i)) {
            // Wide has '#' at position i: it matches everything from here on,
            // so whatever narrow has from i onward is covered.
            (Some(&"#"), _) => return true,

            // Wide has '+' at position i: covers any single concrete level OR
            // another '+' in narrow at position i — but NOT '#' in narrow, because
            // narrow's '#' at position i would match ZERO levels too (e.g. "sport/#"
            // matches "sport" itself). So wide "a/+" cannot cover narrow "a/#"
            // (which matches "a" — wide "a/+" does not).
            (Some(&"+"), Some(&"#")) => return false,
            (Some(&"+"), Some(_)) => {
                // narrow[i] is a literal or '+': covered at this level; continue.
            }
            (Some(&"+"), None) => return false, // lengths differ

            // Literal match at this level: continue.
            (Some(w), Some(n)) if w == n => {}

            // narrow has '#' at position i but wide does NOT: narrow's '#' covers
            // things wide (at this level) would not, e.g. wide="a/b" narrow="a/#"
            // — narrow matches "a" (zero extra levels) but wide doesn't.
            (Some(_), Some(&"#")) => return false,

            // Both exhausted simultaneously: perfect match.
            (None, None) => return true,

            // Any other mismatch (different literals, length difference).
            _ => return false,
        }
        i += 1;
    }
}

/// Encode a canonical absolute path as a topic suffix: per-segment
/// percent-encoding, '/' as the level separator, leading slash dropped
/// (all paths are absolute, so it carries no information). The fs event
/// topic is "obs/fs/" + this.
pub fn encode_path(p: &std::path::Path) -> String {
    use std::path::Component;
    let mut segs: Vec<String> = Vec::new();
    for c in p.components() {
        if let Component::Normal(s) = c {
            segs.push(encode_segment(&s.to_string_lossy()));
        }
    }
    segs.join("/")
}

/// The agent noun's mailbox topic (docs/topics.md): in/agent/<noun>.
pub fn agent_mailbox(noun: &str) -> String {
    format!("in/agent/{}", encode_segment(noun))
}

/// The human noun's mailbox topic: in/human/<noun>. Asks land here.
pub fn human_mailbox(noun: &str) -> String {
    format!("in/human/{}", encode_segment(noun))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact() {
        assert!(matches("in/agent/main", "in/agent/main"));
        assert!(!matches("in/agent/main", "in/agent"));
        assert!(!matches("in/agent", "in/agent/main"));
    }

    #[test]
    fn multi_level() {
        // §4.7.1.2 examples
        assert!(matches("sport/tennis/player1/#", "sport/tennis/player1"));
        assert!(matches(
            "sport/tennis/player1/#",
            "sport/tennis/player1/ranking"
        ));
        assert!(matches(
            "sport/tennis/player1/#",
            "sport/tennis/player1/score/wimbledon"
        ));
        assert!(matches("sport/#", "sport")); // parent level included
        assert!(matches("#", "a/b/c"));
        assert!(!matches("sport/#", "other"));
    }

    #[test]
    fn single_level() {
        // §4.7.1.3 examples
        assert!(matches("sport/tennis/+", "sport/tennis/player1"));
        assert!(matches("sport/tennis/+", "sport/tennis/player2"));
        assert!(!matches("sport/tennis/+", "sport/tennis/player1/ranking"));
        assert!(!matches("sport/+", "sport"));
        assert!(matches("sport/+", "sport/")); // empty level is a level
        assert!(matches("+/+", "a/b"));
        assert!(!matches("+", "a/b"));
    }

    #[test]
    fn dollar_topics() {
        // [MQTT-4.7.2-1]: leading-wildcard filters skip $-topics
        assert!(!matches("#", "$SYS/broker/load"));
        assert!(!matches("+/broker/load", "$SYS/broker/load"));
        assert!(matches("$SYS/#", "$SYS/broker/load"));
    }

    #[test]
    fn validity() {
        assert!(valid_filter("#"));
        assert!(valid_filter("signal/#"));
        assert!(valid_filter("obs/+/llm/#"));
        assert!(!valid_filter(""));
        assert!(!valid_filter("a/#/b")); // # not last
        assert!(!valid_filter("a#")); // # not a whole level
        assert!(!valid_filter("a/b+")); // + not a whole level
        assert!(valid_name("in/agent/main"));
        assert!(!valid_name("in/+/main"));
        assert!(!valid_name(""));
        // malformed filters match nothing
        assert!(!matches("a/#/b", "a/x/b"));
    }

    #[test]
    fn path_encoding() {
        use std::path::Path;
        assert_eq!(encode_path(Path::new("/Users/tim/x.rs")), "Users/tim/x.rs");
        assert_eq!(
            encode_path(Path::new("/notes/#1 draft.md")),
            "notes/%231 draft.md"
        );
        assert!(matches(
            "obs/fs/Users/tim/#",
            &format!("obs/fs/{}", encode_path(Path::new("/Users/tim/a/b.txt")))
        ));
    }

    #[test]
    fn encoding() {
        assert_eq!(encode_segment("notes/#1 draft.md"), "notes%2F%231 draft.md");
        assert_eq!(encode_segment("c++"), "c%2B%2B");
        assert_eq!(encode_segment("100%"), "100%25");
        assert_eq!(encode_segment("shell"), "shell");
        assert!(valid_name(&encode_segment("a+b/c#d")));
    }

    // ── covers(): filter-containment (M2 — authority-delegation) ─────────────

    #[test]
    fn covers_exact_equal() {
        // A filter covers itself (trivially ⊆).
        assert!(covers("a/b/c", "a/b/c"));
        assert!(covers("obs/agent/claude-code/code-abc/#", "obs/agent/claude-code/code-abc/#"));
        assert!(covers("#", "#"));
        assert!(covers("+", "+"));
    }

    #[test]
    fn covers_universal_wide() {
        // "#" covers everything — it is the universal topic set.
        assert!(covers("#", "obs/agent/claude-code/code-abc/#"));
        assert!(covers("#", "in/human/owner"));
        assert!(covers("#", "a/b/c/d/e"));
        assert!(covers("#", "#"));
        assert!(covers("#", "+"));
        assert!(covers("#", "obs/#"));
    }

    #[test]
    fn covers_dollar_topic_rule() {
        // MQTT [MQTT-4.7.2-1]: a root-level wildcard does not match $-topics, so
        // it cannot cover a filter rooted at a `$` literal (which DOES match them).
        // These would be widening holes if covers() claimed true.
        assert!(!covers("#", "$SYS/#"));
        assert!(!covers("#", "$SYS/broker/load"));
        assert!(!covers("+/#", "$SYS/#"));
        assert!(!covers("+/+", "$SYS/load"));
        assert!(!covers("+", "$SYS"));
        // But a $-anchored wide DOES cover its own $-subtree (both skip the rule
        // identically because the first level is the same literal).
        assert!(covers("$SYS/#", "$SYS/broker/load"));
        assert!(covers("$SYS/+", "$SYS/load"));
        assert!(covers("$SYS/#", "$SYS"));
        // A deeper wildcard (not at root) is unaffected by the $ rule.
        assert!(covers("$SYS/#", "$SYS/+/load"));
    }

    #[test]
    fn covers_hash_suffix_prefix_nesting() {
        // obs/# covers obs/agent/…/# because every topic under obs/agent/… starts with obs/.
        assert!(covers("obs/#", "obs/agent/claude-code/code-abc/#"));
        assert!(covers("obs/#", "obs/agent/+/#"));
        assert!(covers("obs/#", "obs/agent/claude-code/code-abc/tool/Bash/call"));
        // obs/agent/+/# covers obs/agent/claude-code/# because + matches any single level.
        assert!(covers("obs/agent/+/#", "obs/agent/claude-code/#"));
        assert!(covers("obs/agent/+/#", "obs/agent/codex/#"));
        // obs/agent/+/# covers obs/agent/+/# (equal).
        assert!(covers("obs/agent/+/#", "obs/agent/+/#"));
        // obs/agent/claude-code/# covers obs/agent/claude-code/code-abc/#.
        assert!(covers("obs/agent/claude-code/#", "obs/agent/claude-code/code-abc/#"));
        // prefix/# covers the prefix itself (sport/# matches sport).
        assert!(covers("obs/#", "obs"));
    }

    #[test]
    fn covers_plus_wide_single_level() {
        // Wide + covers any literal narrow level.
        assert!(covers("a/+", "a/b"));
        assert!(covers("a/+", "a/x"));
        assert!(covers("+", "a"));
        assert!(covers("+/+", "a/b"));
        // Wide + covers narrow + at the same level (+ ⊆ + trivially).
        assert!(covers("a/+", "a/+"));
        assert!(covers("+/+", "+/b"));
    }

    #[test]
    fn covers_not_wider() {
        // Narrow wider than wide: must return false.
        // obs/agent/+/code-abc/# is NOT ⊆ obs/agent/+/#
        // (obs/agent/+/# includes obs/agent/claude-code/anything, obs/agent/+/code-abc/# only
        // includes code-abc sub-subtrees, so actually obs/agent/+/code-abc/# ⊆ obs/agent/+/#;
        // let's do the real direction: obs/agent/+/# is NOT ⊆ obs/agent/+/code-abc/#)
        assert!(!covers("obs/agent/+/code-abc/#", "obs/agent/+/#"));
        // "a/b" does not cover "a/+" because a/+ includes a/c, a/d, etc.
        assert!(!covers("a/b", "a/+"));
        // "a/+" does not cover "a/#" because a/# includes "a" itself.
        assert!(!covers("a/+", "a/#"));
        // obs/+ does not cover obs/# (obs/# matches "obs" itself; obs/+ requires a level).
        assert!(!covers("obs/+", "obs/#"));
        // Completely disjoint: a/b vs c/d.
        assert!(!covers("a/b", "c/d"));
        assert!(!covers("obs/agent/#", "in/human/owner"));
        // Narrower wide: obs/agent/claude-code/# cannot cover obs/agent/+/#.
        assert!(!covers("obs/agent/claude-code/#", "obs/agent/+/#"));
    }

    #[test]
    fn covers_structural_obs_topic_encode_segment() {
        // The actual filters mint() builds: obs/agent/<agent>/<session>/#
        // encoded with encode_segment (no wildcards in agent/session names in
        // practice, but the encoding must be consistent).
        let agent = encode_segment("claude-code");
        let session = encode_segment("code-deadbeef");
        let child_filter = format!("obs/agent/{agent}/{session}/#");
        // The spawner's obs subtree covers the child's own subtree.
        assert!(covers(&format!("obs/agent/{agent}/#"), &child_filter));
        assert!(covers("obs/#", &child_filter));
        assert!(covers("#", &child_filter));
        // The child's own filter does not cover the whole agent subtree.
        assert!(!covers(&child_filter, &format!("obs/agent/{agent}/#")));
    }

    #[test]
    fn covers_malformed_inputs_return_false() {
        // Malformed filters are neither wide nor narrow — conservative false.
        assert!(!covers("", "obs/#"));
        assert!(!covers("obs/#", ""));
        assert!(!covers("a/#/b", "a/x/b")); // # not last
        assert!(!covers("a+b", "a+b"));     // + not a whole level
        assert!(!covers("obs/#", "a/#/b"));
    }

    #[test]
    fn covers_length_mismatches() {
        // More levels in narrow than wide can handle: not ⊆.
        assert!(!covers("a/b", "a/b/c"));
        assert!(!covers("a/b/c", "a/b"));
        // But wide ending with # handles arbitrary extra levels.
        assert!(covers("a/#", "a/b/c/d/e"));
        assert!(covers("a/#", "a/b"));
        assert!(covers("a/#", "a")); // § sport/# matches sport
    }

    // Brute-force soundness oracle. covers(w,n) must imply that every concrete
    // topic matched by n is also matched by w (matches() is ground truth).
    fn gen_filters() -> Vec<String> {
        // Build all filters of 1..=4 levels over the alphabet {a, b, +, #}, then
        // append a handful of `$`-rooted filters so the oracle exercises the MQTT
        // $-topic rule (a root wildcard must not be reported as covering a
        // $-anchored filter) without combinatorially expanding the alphabet.
        // # only valid as the last level; valid_filter rejects others so we just
        // generate all combos and let valid_filter prune.
        let alpha = ["a", "b", "+", "#"];
        let mut out = Vec::new();
        for len in 1..=4 {
            let mut idx = vec![0usize; len];
            loop {
                let f: Vec<&str> = idx.iter().map(|&i| alpha[i]).collect();
                let s = f.join("/");
                if valid_filter(&s) {
                    out.push(s);
                }
                // increment mixed-radix
                let mut p = len - 1;
                loop {
                    idx[p] += 1;
                    if idx[p] < alpha.len() {
                        break;
                    }
                    idx[p] = 0;
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                }
                if idx.iter().all(|&i| i == 0) {
                    break;
                }
            }
        }
        for extra in ["$s/#", "$s/+", "$s/a", "$s", "$s/a/#"] {
            if valid_filter(extra) {
                out.push(extra.to_string());
            }
        }
        out
    }

    fn gen_topics() -> Vec<String> {
        // Concrete topics of 1..=5 levels over {a, b}, plus a few `$`-rooted
        // topics so the oracle can witness the MQTT $-topic rule. valid_name
        // guaranteed.
        let alpha = ["a", "b"];
        let mut out = Vec::new();
        for len in 1..=5 {
            let mut idx = vec![0usize; len];
            loop {
                let t: Vec<&str> = idx.iter().map(|&i| alpha[i]).collect();
                out.push(t.join("/"));
                let mut p = len - 1;
                loop {
                    idx[p] += 1;
                    if idx[p] < alpha.len() {
                        break;
                    }
                    idx[p] = 0;
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                }
                if idx.iter().all(|&i| i == 0) {
                    break;
                }
            }
        }
        for extra in ["$s", "$s/a", "$s/b", "$s/a/b"] {
            if valid_name(extra) {
                out.push(extra.to_string());
            }
        }
        out
    }

    #[test]
    fn covers_soundness_oracle() {
        let filters = gen_filters();
        let topics = gen_topics();
        let mut violations = Vec::new();
        for w in &filters {
            for n in &filters {
                if covers(w, n) {
                    // Property: for every topic matched by n, w must match too.
                    for t in &topics {
                        if matches(n, t) && !matches(w, t) {
                            violations.push(format!(
                                "covers({w:?},{n:?})=true but topic {t:?} matched by narrow NOT by wide"
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "UNSOUND covers() — {} violations:\n{}",
            violations.len(),
            violations.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}
