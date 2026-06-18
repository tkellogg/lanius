use std::path::Path;

/// Minimal .env loader: KEY=VALUE lines, '#' comments, optional `export `
/// prefix and surrounding quotes. Real environment always wins — a var that
/// is already set is never overridden, so `.env` is a fallback, not policy.
pub fn load(path: &Path) {
    let Ok(s) = std::fs::read_to_string(path) else {
        return;
    };
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let mut v = v.trim();
        if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            v = &v[1..v.len() - 1];
        }
        if !k.is_empty() && std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}
