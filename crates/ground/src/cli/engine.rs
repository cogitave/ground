//! `ground select` and `ground manifests import`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};

use crate::engine::select::{SelectOptions, Selection, select};
use crate::engine::store::{ManifestStore, default_dir};

/// Printed by `--format vitest` when nothing is selected.
pub const NOTHING_SELECTED: &str = "GROUND_NO_TESTS_SELECTED";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Json,
    Lines,
    Vitest,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct SelectArgs {
    /// Ref to diff against (`git diff <base>...HEAD`)
    #[arg(long)]
    pub base: String,
    /// Manifest directory (default $GROUND_HOME/manifests/<repo-key>)
    #[arg(long)]
    pub manifests: Option<PathBuf>,
    /// Repository root (default: current directory)
    #[arg(long)]
    pub root: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "json")]
    pub format: Format,
    /// Extra globs whose change selects every test
    #[arg(long, num_args = 1..)]
    pub global: Vec<String>,
    /// Globs of tests that always run
    #[arg(long, num_args = 1..)]
    pub always: Vec<String>,
    /// Globs that identify test files (replaces the defaults)
    #[arg(long, num_args = 1..)]
    pub tests: Vec<String>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ManifestsCommand {
    /// Copy a directory of manifest JSON files into the store
    Import {
        dir: PathBuf,
        #[arg(long)]
        manifests: Option<PathBuf>,
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

pub fn render(sel: &Selection, format: Format) -> Result<String, String> {
    // "Run everything" with nothing listed (test enumeration failed) is not
    // "run nothing": every format would print an empty list and the build
    // would go green having run no test. Refuse, so the exit code says so.
    if sel.all && sel.selected.is_empty() {
        return Err(format!(
            "cannot list the tests to run, and everything must run: {}",
            sel.all_reason.as_deref().unwrap_or("unknown reason")
        ));
    }
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(sel).map_err(|e| e.to_string())?,
        Format::Lines => sel
            .selected
            .iter()
            .map(|s| s.test.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Vitest if sel.selected.is_empty() => NOTHING_SELECTED.to_owned(),
        Format::Vitest => sel
            .selected
            .iter()
            .map(|s| s.test.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    })
}

fn root_of(root: Option<PathBuf>) -> Result<PathBuf, String> {
    match root {
        Some(r) => Ok(r),
        None => std::env::current_dir().map_err(|e| e.to_string()),
    }
}

pub fn cmd_select(home: &Path, args: SelectArgs) -> Result<i32, String> {
    let root = root_of(args.root)?;
    let manifests = args.manifests.unwrap_or_else(|| default_dir(home, &root));
    let mut opts = SelectOptions::new(root, args.base, manifests);
    opts.global.extend(args.global);
    opts.always = args.always;
    if !args.tests.is_empty() {
        opts.tests = args.tests;
        opts.tests_declared = true;
    }
    let sel = select(&opts);
    if let Some(reason) = &sel.all_reason {
        eprintln!("ground: selecting everything: {reason}");
    }
    let text = render(&sel, args.format)?;
    if !text.is_empty() {
        println!("{text}");
    }
    Ok(0)
}

pub fn cmd_manifests(home: &Path, cmd: ManifestsCommand) -> Result<i32, String> {
    match cmd {
        ManifestsCommand::Import {
            dir,
            manifests,
            root,
        } => {
            let root = root_of(root)?;
            let store = ManifestStore::new(manifests.unwrap_or_else(|| default_dir(home, &root)));
            let n = store.import(&dir).map_err(|e| e.to_string())?;
            println!("imported {n} manifests into {}", store.dir().display());
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::select::{Counts, Selected};

    fn sel(tests: &[&str]) -> Selection {
        Selection {
            base: "b".into(),
            all: false,
            all_reason: None,
            selected: tests
                .iter()
                .map(|t| Selected {
                    test: (*t).into(),
                    reasons: vec![],
                })
                .collect(),
            counts: Counts {
                total: 3,
                selected: tests.len(),
                skipped: 3 - tests.len(),
            },
        }
    }

    #[test]
    fn vitest_format_is_space_separated_with_an_explicit_empty_marker() {
        assert_eq!(
            render(&sel(&["a.test.ts", "b.test.ts"]), Format::Vitest).unwrap(),
            "a.test.ts b.test.ts"
        );
        assert_eq!(render(&sel(&[]), Format::Vitest).unwrap(), NOTHING_SELECTED);
        assert_eq!(render(&sel(&[]), Format::Lines).unwrap(), "");
    }

    #[test]
    fn run_everything_with_no_list_is_an_error_in_every_format_not_an_empty_list() {
        let mut s = sel(&[]);
        s.all = true;
        s.all_reason = Some("enumerating tests failed: boom".into());
        for f in [Format::Json, Format::Lines, Format::Vitest] {
            let e = render(&s, f).unwrap_err();
            assert!(e.contains("boom"), "{e}");
        }
    }

    #[test]
    fn a_broken_checkout_makes_the_command_fail_instead_of_printing_nothing() {
        let d = tempfile::tempdir().unwrap();
        let r = cmd_select(
            d.path(),
            SelectArgs {
                base: "main".into(),
                manifests: Some(d.path().join("m")),
                root: Some(d.path().to_owned()),
                format: Format::Vitest,
                global: vec![],
                always: vec![],
                tests: vec![],
            },
        );
        assert!(r.unwrap_err().contains("enumerating tests failed"));
    }

    #[test]
    fn json_carries_reasons_and_counts() {
        let v: serde_json::Value =
            serde_json::from_str(&render(&sel(&["a"]), Format::Json).unwrap()).unwrap();
        assert_eq!(v["counts"]["selected"], 1);
        assert!(v["selected"][0]["reasons"].is_array());
    }
}
