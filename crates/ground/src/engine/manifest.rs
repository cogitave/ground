//! A manifest: what one test file's process tree read during a passing run.

use serde::{Deserialize, Serialize};

/// One file the test opened for reading, with its blake3 (lowercase hex).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    /// Lowercase hex, or a marker that never equals a hash: `volatile` (the
    /// file changed while the run was reading it) or `missing` (it was gone by
    /// the end of the run). Either always selects the test.
    pub blake3: String,
    /// The entry fingerprints the symlink itself (its target text), not what
    /// it points to. A path both followed and examined as a link has two entries.
    #[serde(default, skip_serializing_if = "is_false")]
    pub link: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl FileEntry {
    pub fn file(path: impl Into<String>, blake3: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            blake3: blake3.into(),
            link: false,
        }
    }
}

/// The tracer writes this JSON; `test` is added by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// Directory the trace ran in; absolute `files` paths under it are
    /// re-based onto the current root at selection time.
    pub root: String,
    pub command: Vec<String>,
    pub exit_code: i32,
    /// Repo-relative path of the test file this manifest belongs to.
    pub test: String,
    pub files: Vec<FileEntry>,
    /// Paths probed and not found; one appearing invalidates the manifest.
    #[serde(default)]
    pub absent: Vec<String>,
    /// Executables the process tree ran.
    #[serde(default)]
    pub exec: Vec<String>,
    /// The executables' content hashes (the toolchain fingerprint). Any
    /// difference selects every test.
    #[serde(default)]
    pub toolchain: Vec<FileEntry>,
    /// Values of the declared environment allow-list when it was traced
    /// (`null` = unset). A different current value selects every test.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, Option<String>>,
    /// `git rev-parse HEAD` of the checkout it was traced in. Global inputs
    /// that changed since then are found by diffing from it; none recorded
    /// selects everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Engine version that wrote it, when recorded; a mismatch selects all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

pub const MANIFEST_VERSION: u32 = 1;

impl Manifest {
    /// Parses and validates; an unknown version is an error (fail closed).
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let m: Manifest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if m.version != MANIFEST_VERSION {
            return Err(format!("unsupported manifest version {}", m.version));
        }
        if m.test.is_empty() {
            return Err("manifest has no test path".into());
        }
        Ok(m)
    }
}

/// `./a//b` -> `a/b`; backslashes become `/`.
pub fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    p.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_tracer_json_and_defaults_optional_lists() {
        let m = Manifest::parse(
            br#"{"version":1,"root":"/r","command":["vitest"],"exit_code":0,"test":"a.test.ts","files":[{"path":"a.ts","blake3":"ab"}]}"#,
        )
        .unwrap();
        assert!(m.absent.is_empty() && m.exec.is_empty() && m.engine.is_none());
        assert!(m.toolchain.is_empty() && m.env.is_empty() && m.commit.is_none());
    }

    #[test]
    fn rejects_unknown_versions_and_garbage() {
        assert!(Manifest::parse(b"{").is_err());
        assert!(
            Manifest::parse(
                br#"{"version":2,"root":"","command":[],"exit_code":0,"test":"a","files":[]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize_path("./a//b\\c"), "a/b/c");
    }
}
