use std::path::Path;

use super::paths::repos_dir;
use crate::config::{RepoConfig, load_repo_config};

#[derive(Debug, Default)]
pub struct LoadedRepos {
    pub configs: Vec<RepoConfig>,
    /// One message per file that did not load; other repos still run.
    pub errors: Vec<String>,
}

pub fn load_repos(home: &Path) -> LoadedRepos {
    let dir = repos_dir(home);
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json"))
            .collect(),
        Err(err) => {
            return LoadedRepos {
                configs: vec![],
                errors: vec![format!("cannot read {} ({err})", dir.display())],
            };
        }
    };
    names.sort();
    let mut out = LoadedRepos::default();
    for name in names {
        match load_repo_config(&dir.join(&name)) {
            Ok(c) => out.configs.push(c),
            Err(e) => out.errors.push(e.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good(repo: &str) -> String {
        crate::daemon::test_support::config_json(repo).to_string()
    }

    #[test]
    fn loads_good_files_sorted_and_reports_bad_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repos_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.json"), good("x/b")).unwrap();
        std::fs::write(dir.join("a.json"), good("a/b")).unwrap();
        std::fs::write(dir.join("bad.json"), "{").unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        let r = load_repos(tmp.path());
        let repos: Vec<_> = r.configs.iter().map(|c| c.repo.as_str()).collect();
        assert_eq!(repos, ["a/b", "x/b"]);
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("bad.json"));
    }

    #[test]
    fn a_missing_directory_is_one_error() {
        let tmp = tempfile::tempdir().unwrap();
        let r = load_repos(tmp.path());
        assert!(r.configs.is_empty());
        assert!(r.errors[0].starts_with("cannot read"));
    }
}
