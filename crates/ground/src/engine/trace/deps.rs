//! Package-level records for installed dependencies.
//!
//! pnpm lays packages out as `node_modules/.pnpm/<name>@<version>[_<peers>]/
//! node_modules/<name>/...` and verifies each against the lockfile's
//! integrity, so the directory name pins the package's name, version and peer
//! set. A read of any file inside it is recorded as the package instead
//! ([`DepEntry`]); the selector then asks only whether the package directory is
//! still installed. Anything the name does not pin stays a per-file record:
//!
//! - a sibling link inside a store entry (`.pnpm/X/node_modules/chai`, the
//!   dependency link, not X itself): the lockfile may re-resolve it to another
//!   version under the same directory name;
//! - a version that is not a registry semver (`file+`, `https+`, `github.com+`
//!   store names pin a location, not content).
//!
//! The name alone is not trusted for what is inside: each record carries a
//! content hash of the package directory, taken when the run ends. A registry
//! that republishes a version (a private mirror) is covered by the same hash.

use std::path::{Component, Path, PathBuf};

use super::normalise;
use crate::engine::manifest::DepEntry;

fn parts(rel: &Path) -> Vec<&str> {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect()
}

/// `1.2.3`, `1.2.3-rc.1`, `1.2.3+build`: what a registry publishes (and never
/// republishes). A version starting with a letter (`file+..`, `https+..`,
/// `github.com+..`) names a location.
fn is_registry_version(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let mut n = 0;
    for seg in core.split('.') {
        n += 1;
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    n == 3
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-+.".contains(&b))
}

/// The package a virtual-store directory name is for, when its version is a
/// registry version: `@vitest+mocker@4.1.11_vite@8` -> `@vitest/mocker`.
fn store_dir_package(dir: &str) -> Option<String> {
    let at = dir.char_indices().skip(1).find(|(_, c)| *c == '@')?.0;
    let (name, rest) = (&dir[..at], &dir[at + 1..]);
    let version = rest.split('_').next()?;
    if name.is_empty() || !is_registry_version(version) {
        return None;
    }
    Some(match name.strip_prefix('@') {
        Some(scoped) => format!("@{}", scoped.replacen('+', "/", 1)),
        None => name.to_owned(),
    })
}

/// The package record for a path inside a pnpm store entry's own package
/// directory, or `None` when the path is anywhere else or the layout does not
/// pin its content.
pub fn pnpm_dep(rel: &Path) -> Option<DepEntry> {
    let p = parts(rel);
    let i = (0..p.len().saturating_sub(4)).find(|&i| {
        p[i] == "node_modules" && p[i + 1] == ".pnpm" && p.get(i + 3) == Some(&"node_modules")
    })?;
    let dir = p[i + 2];
    let expect = store_dir_package(dir)?;
    let named = if p[i + 4].starts_with('@') {
        format!("{}/{}", p[i + 4], p.get(i + 5)?)
    } else {
        p[i + 4].to_owned()
    };
    if named != expect {
        return None;
    }
    Some(DepEntry {
        dep: format!("{dir}/{named}"),
        store: p[..=i + 1].join("/"),
        blake3: String::new(),
    })
}

/// A path read through a symlink to an installed package, which is how most
/// of a pnpm install is reached (`packages/x/node_modules/vitest -> ../../..
/// /node_modules/.pnpm/vitest@4.1.11_../node_modules/vitest`, and the
/// dependency links inside each store entry). Returns the link (relative to
/// `root`, whose text the caller keeps as a `link` record, so a retarget is
/// seen) and the package it points to, whose directory name pins the content.
/// `None` unless the first symlinked `node_modules/<name>` on the path
/// resolves, lexically and inside `root`, to exactly a package directory that
/// [`pnpm_dep`] collapses.
pub fn pnpm_link_dep(root: &Path, rel: &Path) -> Option<(PathBuf, DepEntry)> {
    let p = parts(rel);
    for i in 0..p.len() {
        if p[i] != "node_modules" {
            continue;
        }
        let Some(first) = p.get(i + 1) else { break };
        if *first == ".pnpm" || first.starts_with('.') {
            continue;
        }
        let end = if first.starts_with('@') { i + 3 } else { i + 2 };
        if p.len() < end {
            break;
        }
        let link: PathBuf = p[..end].iter().collect();
        let abs = root.join(&link);
        if !std::fs::symlink_metadata(&abs).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        let target = std::fs::read_link(&abs).ok()?;
        if target.is_absolute() {
            return None;
        }
        let resolved = normalise(&root.join(link.parent()?).join(target));
        let resolved = resolved.strip_prefix(root).ok()?;
        let dep = pnpm_dep(resolved)?;
        // Only the package directory itself: anything else it names is not
        // pinned by the store entry's name.
        if PathBuf::from(dep.path()) != resolved {
            return None;
        }
        // The lexical result must be what the filesystem says too: a
        // symlinked ancestor of the link can send the real path elsewhere.
        let real = std::fs::canonicalize(&abs).ok()?;
        let expect = std::fs::canonicalize(root.join(resolved)).ok()?;
        return (real == expect).then_some((link, dep));
    }
    None
}

/// The `package.json` of the installed package a path under `node_modules`
/// belongs to (the nearest `node_modules/<name>` or `node_modules/@s/<name>`).
pub fn package_json_of(rel: &Path) -> Option<String> {
    let p = parts(rel);
    let i = p.iter().rposition(|c| *c == "node_modules")?;
    let first = *p.get(i + 1)?;
    if first.starts_with('.') {
        return None;
    }
    let end = if first.starts_with('@') { i + 3 } else { i + 2 };
    if p.len() <= end {
        return None; // the package directory itself, or a scope
    }
    Some(format!("{}/package.json", p[..end].join("/")))
}

/// Files the installer keeps for itself. They change on every install (a
/// prune timestamp, the store location) while describing nothing a package
/// contains; the packages a test read are checked on their own.
pub fn is_installer_state(rel: &Path) -> bool {
    let p = parts(rel);
    p.iter().enumerate().any(|(i, c)| {
        *c == "node_modules"
            && p.len() == i + 2
            && matches!(p[i + 1], ".modules.yaml" | ".pnpm-workspace-state-v1.json")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(p: &str) -> Option<String> {
        pnpm_dep(Path::new(p)).map(|d| format!("{} @ {}", d.dep, d.store))
    }

    #[test]
    fn a_file_in_the_stores_own_package_collapses_to_the_package() {
        assert_eq!(
            dep("node_modules/.pnpm/zod@3.25.76/node_modules/zod/dist/index.js").unwrap(),
            "zod@3.25.76/zod @ node_modules/.pnpm"
        );
        assert_eq!(
            dep("node_modules/.pnpm/@vitest+mocker@4.1.11_vite@8.3.1_@types+node@22/node_modules/@vitest/mocker/package.json")
                .unwrap(),
            "@vitest+mocker@4.1.11_vite@8.3.1_@types+node@22/@vitest/mocker @ node_modules/.pnpm"
        );
        assert_eq!(
            dep("packages/a/node_modules/.pnpm/x@1.0.0-rc.1/node_modules/x").unwrap(),
            "x@1.0.0-rc.1/x @ packages/a/node_modules/.pnpm"
        );
    }

    #[test]
    fn what_the_directory_name_does_not_pin_stays_per_file() {
        // A dependency link inside the entry, not the entry's own package.
        assert_eq!(
            dep("node_modules/.pnpm/@vitest+expect@4.1.11/node_modules/chai"),
            None
        );
        assert_eq!(
            dep("node_modules/.pnpm/a@1.0.0/node_modules/@s/b/x.js"),
            None
        );
        // A location, not a version.
        assert_eq!(
            dep("node_modules/.pnpm/x@file+..+x/node_modules/x/i.js"),
            None
        );
        assert_eq!(
            dep("node_modules/.pnpm/x@https+host+x.tgz/node_modules/x/i.js"),
            None
        );
        assert_eq!(dep("node_modules/.pnpm/x@1.0/node_modules/x/i.js"), None);
        // Not under a store entry at all.
        assert_eq!(dep("node_modules/zod/index.js"), None);
        assert_eq!(dep("node_modules/.pnpm/lock.yaml"), None);
        assert_eq!(dep("node_modules/.pnpm/zod@3.25.76"), None);
    }

    #[test]
    fn a_path_read_through_a_link_to_a_package_collapses_and_keeps_the_link() {
        let d = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(d.path()).unwrap();
        let store = root.join("node_modules/.pnpm");
        std::fs::create_dir_all(store.join("chai@5.2.0/node_modules/chai")).unwrap();
        std::fs::create_dir_all(store.join("vitest@4.1.11/node_modules")).unwrap();
        std::os::unix::fs::symlink(
            "../../chai@5.2.0/node_modules/chai",
            store.join("vitest@4.1.11/node_modules/chai"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("packages/a/node_modules")).unwrap();
        std::os::unix::fs::symlink(
            "../../../node_modules/.pnpm/chai@5.2.0/node_modules/chai",
            root.join("packages/a/node_modules/chai"),
        )
        .unwrap();
        // A link that points somewhere that is not a package directory.
        std::os::unix::fs::symlink("../../../src", root.join("packages/a/node_modules/loose"))
            .unwrap();
        // A link whose target leaves the root.
        std::os::unix::fs::symlink("../../../../x", root.join("packages/a/node_modules/out"))
            .unwrap();

        let got = |p: &str| {
            pnpm_link_dep(&root, Path::new(p))
                .map(|(l, d)| (l.to_string_lossy().into_owned(), d.dep))
        };
        assert_eq!(
            got("node_modules/.pnpm/vitest@4.1.11/node_modules/chai/index.js").unwrap(),
            (
                "node_modules/.pnpm/vitest@4.1.11/node_modules/chai".to_owned(),
                "chai@5.2.0/chai".to_owned()
            )
        );
        assert_eq!(
            got("packages/a/node_modules/chai/package.json").unwrap().1,
            "chai@5.2.0/chai"
        );
        assert_eq!(got("packages/a/node_modules/loose/x.js"), None);
        assert_eq!(got("packages/a/node_modules/out/x.js"), None);
        assert_eq!(got("packages/a/node_modules/plain/x.js"), None);
        assert_eq!(got("src/x.js"), None);
    }

    #[test]
    fn a_link_reached_through_a_symlinked_ancestor_is_not_collapsed() {
        let d = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(d.path()).unwrap();
        let store = root.join("node_modules/.pnpm");
        std::fs::create_dir_all(store.join("chai@5.2.0/node_modules/chai")).unwrap();
        // `real/a/node_modules/chai -> ../../../node_modules/.pnpm/...` is
        // right lexically from `alias/a` (a link to `real`) only by accident:
        // the real directory sits one level deeper than the link claims.
        std::fs::create_dir_all(root.join("real/deeper/a/node_modules")).unwrap();
        std::os::unix::fs::symlink("real/deeper", root.join("alias")).unwrap();
        std::os::unix::fs::symlink(
            "../../../../node_modules/.pnpm/chai@5.2.0/node_modules/chai",
            root.join("real/deeper/a/node_modules/chai"),
        )
        .unwrap();
        // Through the real path the link resolves to the store entry.
        assert!(pnpm_link_dep(&root, Path::new("real/deeper/a/node_modules/chai/x.js")).is_some());
        // Through the alias, lexically `alias/a/node_modules/chai` -> the same
        // text resolves to a path that is not the store entry for the
        // filesystem, so it stays a per-file record.
        assert_eq!(
            pnpm_link_dep(&root, Path::new("alias/a/node_modules/chai/x.js")).map(|r| r.1.dep),
            None
        );
    }

    #[test]
    fn the_package_json_of_the_nearest_installed_package() {
        let pj = |p: &str| package_json_of(Path::new(p));
        assert_eq!(
            pj("node_modules/a/lib/x.js").unwrap(),
            "node_modules/a/package.json"
        );
        assert_eq!(
            pj("p/node_modules/@s/a/x.js").unwrap(),
            "p/node_modules/@s/a/package.json"
        );
        assert_eq!(
            pj("node_modules/a/node_modules/b/i.js").unwrap(),
            "node_modules/a/node_modules/b/package.json"
        );
        assert_eq!(pj("node_modules/a"), None);
        assert_eq!(pj("node_modules/@s/a"), None);
        assert_eq!(pj("node_modules/.bin/x"), None);
        assert_eq!(pj("src/x.js"), None);
    }

    #[test]
    fn installer_state_is_only_the_two_files_at_the_top_of_node_modules() {
        let s = |p: &str| is_installer_state(Path::new(p));
        assert!(s("node_modules/.modules.yaml"));
        assert!(s("a/node_modules/.pnpm-workspace-state-v1.json"));
        assert!(!s("node_modules/x/.modules.yaml"));
        assert!(!s("node_modules/.pnpm/lock.yaml"));
    }
}
