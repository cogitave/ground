use std::path::{Path, PathBuf};

/// `$GROUND_HOME`, default `~/.ground`. `env` looks a variable up.
pub fn ground_home(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match env("GROUND_HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => PathBuf::from(env("HOME").unwrap_or_default()).join(".ground"),
    }
}

/// `ground_home` over the process environment.
pub fn ground_home_from_env() -> PathBuf {
    ground_home(&|k| std::env::var(k).ok())
}

pub fn repos_dir(home: &Path) -> PathBuf {
    home.join("repos")
}
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("groundd.sock")
}
pub fn db_path(home: &Path) -> PathBuf {
    home.join("state.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honours_ground_home_and_falls_back_to_dot_ground() {
        let with = |v: Option<&'static str>| {
            ground_home(&move |k| match k {
                "GROUND_HOME" => v.map(str::to_owned),
                "HOME" => Some("/home/u".into()),
                _ => None,
            })
        };
        assert_eq!(with(Some("/data/g")), PathBuf::from("/data/g"));
        assert_eq!(with(Some("")), PathBuf::from("/home/u/.ground"));
        assert_eq!(with(None), PathBuf::from("/home/u/.ground"));
        assert_eq!(
            socket_path(Path::new("/h")),
            PathBuf::from("/h/groundd.sock")
        );
        assert_eq!(db_path(Path::new("/h")), PathBuf::from("/h/state.db"));
        assert_eq!(repos_dir(Path::new("/h")), PathBuf::from("/h/repos"));
    }
}
