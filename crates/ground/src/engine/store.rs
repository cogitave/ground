//! The manifest store: one JSON file per test file under a directory.

use std::fs;
use std::path::{Path, PathBuf};

use super::manifest::{Manifest, normalize_path};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("manifest directory {0} is missing")]
    Missing(PathBuf),
    #[error("{0}: {1}")]
    Corrupt(PathBuf, String),
    #[error("{0}: {1}")]
    Io(PathBuf, String),
}

pub struct ManifestStore {
    dir: PathBuf,
}

/// `$GROUND_HOME/manifests/<repo-key>/`, the repo key being the root's name
/// plus a hash of its canonical path.
pub fn default_dir(ground_home: &Path, root: &Path) -> PathBuf {
    let canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let hash = blake3::hash(canon.to_string_lossy().as_bytes()).to_hex();
    let name: String = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    ground_home
        .join("manifests")
        .join(format!("{name}-{}", &hash[..16]))
}

/// A reversible, filesystem-safe file name for a test path.
fn file_name(test: &str) -> String {
    let mut s = String::new();
    for c in normalize_path(test).chars() {
        match c {
            '%' => s.push_str("%25"),
            '/' => s.push_str("%2F"),
            c => s.push(c),
        }
    }
    s.push_str(".json");
    s
}

impl ManifestStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn put(&self, m: &Manifest) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| StoreError::Io(self.dir.clone(), e.to_string()))?;
        let mut m = m.clone();
        m.test = normalize_path(&m.test);
        let path = self.dir.join(file_name(&m.test));
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&m)
            .map_err(|e| StoreError::Io(path.clone(), e.to_string()))?;
        fs::write(&tmp, bytes)
            .and_then(|_| fs::rename(&tmp, &path))
            .map_err(|e| StoreError::Io(path, e.to_string()))
    }

    /// Every manifest, keyed by the test path inside it. A missing directory
    /// or any unreadable, unparseable or misfiled entry is an error: the
    /// caller selects everything.
    pub fn load_all(&self) -> Result<Vec<Manifest>, StoreError> {
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::Missing(self.dir.clone()));
            }
            Err(e) => return Err(StoreError::Io(self.dir.clone(), e.to_string())),
        };
        let mut paths: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            let bytes = fs::read(&p).map_err(|e| StoreError::Io(p.clone(), e.to_string()))?;
            let mut m = Manifest::parse(&bytes).map_err(|e| StoreError::Corrupt(p.clone(), e))?;
            m.test = normalize_path(&m.test);
            if p.file_name().is_some_and(|n| *n != *file_name(&m.test)) {
                return Err(StoreError::Corrupt(
                    p,
                    "file name does not match its test path".into(),
                ));
            }
            out.push(m);
        }
        Ok(out)
    }

    /// Imports every `*.json` in `src` (validated) and returns how many.
    /// Nothing is written unless all of them parse.
    pub fn import(&self, src: &Path) -> Result<usize, StoreError> {
        let rd = fs::read_dir(src).map_err(|e| StoreError::Io(src.to_owned(), e.to_string()))?;
        let mut all = Vec::new();
        for entry in rd.filter_map(Result::ok) {
            let p = entry.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let bytes = fs::read(&p).map_err(|e| StoreError::Io(p.clone(), e.to_string()))?;
            all.push(Manifest::parse(&bytes).map_err(|e| StoreError::Corrupt(p, e))?);
        }
        for m in &all {
            self.put(m)?;
        }
        Ok(all.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample(test: &str) -> Manifest {
        Manifest {
            version: 1,
            root: "/r".into(),
            command: vec!["vitest".into()],
            exit_code: 0,
            test: test.into(),
            files: vec![],
            absent: vec![],
            exec: vec![],
            toolchain: vec![],
            env: Default::default(),
            commit: None,
            engine: None,
        }
    }

    #[test]
    fn put_then_load_round_trips_nested_test_paths() {
        let d = tempfile::tempdir().unwrap();
        let s = ManifestStore::new(d.path().join("m"));
        s.put(&sample("packages/a/x.test.ts")).unwrap();
        s.put(&sample("./b%.test.ts")).unwrap();
        let got = s.load_all().unwrap();
        let tests: Vec<_> = got.iter().map(|m| m.test.as_str()).collect();
        assert_eq!(tests, ["b%.test.ts", "packages/a/x.test.ts"]);
    }

    #[test]
    fn missing_dir_and_corrupt_entries_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let s = ManifestStore::new(d.path().join("m"));
        assert!(matches!(s.load_all(), Err(StoreError::Missing(_))));
        s.put(&sample("a.test.ts")).unwrap();
        fs::write(d.path().join("m/bad.json"), "{nope").unwrap();
        assert!(matches!(s.load_all(), Err(StoreError::Corrupt(..))));
    }

    #[test]
    fn import_is_all_or_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(
            src.join("a.json"),
            serde_json::to_vec(&sample("a.test.ts")).unwrap(),
        )
        .unwrap();
        fs::write(src.join("b.json"), "garbage").unwrap();
        let s = ManifestStore::new(d.path().join("m"));
        assert!(s.import(&src).is_err());
        assert!(matches!(s.load_all(), Err(StoreError::Missing(_))));
        fs::remove_file(src.join("b.json")).unwrap();
        assert_eq!(s.import(&src).unwrap(), 1);
        assert_eq!(s.load_all().unwrap().len(), 1);
    }

    #[test]
    fn default_dir_is_stable_and_distinguishes_roots() {
        let h = Path::new("/h");
        let a = default_dir(h, Path::new("/nonexistent/a"));
        assert_eq!(a, default_dir(h, Path::new("/nonexistent/a")));
        assert_ne!(a, default_dir(h, Path::new("/other/a")));
        assert!(a.starts_with("/h/manifests"));
    }
}
