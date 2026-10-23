use super::types::{BumpScan, BumpedPackage};
use crate::ports::PrFile;
use crate::ports::PrFileStatus;

/// Strips the diff marker (one of `allowed`) and the two-space or tab indent that
/// marks a top-level key, so a nested "name" or "version" inside dependencies, bin
/// or an author block is never mistaken for the package's.
fn top_level<'a>(line: &'a str, allowed: &[char]) -> Option<&'a str> {
    let mut chars = line.chars();
    let first = chars.next()?;
    if !allowed.contains(&first) {
        return None;
    }
    let rest = chars.as_str();
    rest.strip_prefix("  ").or_else(|| rest.strip_prefix('\t'))
}

/// `"key"\s*:\s*` at the start of `s`; returns what follows.
fn after_key<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let s = s.strip_prefix('"')?.strip_prefix(key)?.strip_prefix('"')?;
    let s = s
        .trim_start_matches(char::is_whitespace)
        .strip_prefix(':')?;
    Some(s.trim_start_matches(char::is_whitespace))
}

/// `"key"\s*:\s*"([^"]+)"` after the indent and a diff marker from `allowed`.
fn string_field<'a>(line: &'a str, allowed: &[char], key: &str) -> Option<&'a str> {
    let rest = after_key(top_level(line, allowed)?, key)?;
    let body = rest.strip_prefix('"')?;
    let end = body.find('"')?;
    (end > 0).then(|| &body[..end])
}

fn is_private(line: &str) -> bool {
    top_level(line, &[' ', '+', '-'])
        .and_then(|r| after_key(r, "private"))
        .is_some_and(|r| r.starts_with("true"))
}

fn is_package_json(path: &str) -> bool {
    path == "package.json" || path.ends_with("/package.json")
}

/// Which packages a version PR bumps, read from the package.json patches only.
///
/// `GitHubPort` has no file-content read, so the package name comes from the
/// patch's context lines. A version bump sits within three lines of "name" in
/// every npm package.json, so the default context shows both. When it does not
/// (or GitHub omitted the patch as too large) the file is reported as
/// unresolved instead of guessing a name from its directory: a wrong name would
/// make the registry check poll a package that is not ours.
pub fn scan_bumps(files: &[PrFile]) -> BumpScan {
    let mut scan = BumpScan::default();
    for file in files {
        if !is_package_json(&file.path) {
            continue;
        }
        let patch = match file.patch.as_deref() {
            Some(p) if !p.is_empty() && file.status != PrFileStatus::Removed => p,
            _ => {
                if file.status != PrFileStatus::Removed {
                    scan.unresolved.push(file.path.clone());
                }
                continue;
            }
        };
        if patch.split('\n').any(is_private) {
            continue;
        }
        let (mut from, mut to, mut name): (Option<&str>, Option<&str>, Option<&str>) =
            (None, None, None);
        for line in patch.split('\n') {
            name = name.or_else(|| string_field(line, &[' ', '+', '-'], "name"));
            from = from.or_else(|| string_field(line, &['-'], "version"));
            to = to.or_else(|| string_field(line, &['+'], "version"));
        }
        let Some(to) = to else { continue };
        if from == Some(to) {
            continue;
        }
        match name {
            None => scan.unresolved.push(file.path.clone()),
            Some(name) => scan.packages.push(BumpedPackage {
                name: name.to_owned(),
                from: from.map(str::to_owned),
                to: to.to_owned(),
            }),
        }
    }
    scan
}
