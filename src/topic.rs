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

/// Encode a canonical absolute path as a topic suffix: per-segment
/// percent-encoding, '/' as the level separator, leading slash dropped
/// (all paths are absolute, so it carries no information). The fs event
/// topic is "fs/" + this.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact() {
        assert!(matches("work/agent/exec", "work/agent/exec"));
        assert!(!matches("work/agent/exec", "work/agent"));
        assert!(!matches("work/agent", "work/agent/exec"));
    }

    #[test]
    fn multi_level() {
        // §4.7.1.2 examples
        assert!(matches("sport/tennis/player1/#", "sport/tennis/player1"));
        assert!(matches("sport/tennis/player1/#", "sport/tennis/player1/ranking"));
        assert!(matches("sport/tennis/player1/#", "sport/tennis/player1/score/wimbledon"));
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
        assert!(!valid_filter("a#"));    // # not a whole level
        assert!(!valid_filter("a/b+"));  // + not a whole level
        assert!(valid_name("work/agent/exec"));
        assert!(!valid_name("work/+/exec"));
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
        assert!(matches("fs/Users/tim/#", &format!("fs/{}", encode_path(Path::new("/Users/tim/a/b.txt")))));
    }

    #[test]
    fn encoding() {
        assert_eq!(encode_segment("notes/#1 draft.md"), "notes%2F%231 draft.md");
        assert_eq!(encode_segment("c++"), "c%2B%2B");
        assert_eq!(encode_segment("100%"), "100%25");
        assert_eq!(encode_segment("shell"), "shell");
        assert!(valid_name(&encode_segment("a+b/c#d")));
    }
}
