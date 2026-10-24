use std::fs;
use std::path::Path;
use std::process::Command;

use super::*;
use crate::engine::manifest::{FileEntry, Manifest};

fn git(root: &Path, args: &[&str]) {
    let st = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn h(root: &Path, rel: &str) -> String {
    hash_file(&root.join(rel), false).unwrap()
}

fn head(root: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Fx {
    _d: tempfile::TempDir,
    root: PathBuf,
    mdir: PathBuf,
}

/// Repo with a.test.ts -> src/a.ts, b.test.ts -> src/b.ts and dist/x.js (ignored).
fn fixture() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    write(&root, ".gitignore", "dist/\n");
    write(&root, "a.test.ts", "a");
    write(&root, "b.test.ts", "b");
    write(&root, "bin/tool", "tool1");
    write(&root, "src/a.ts", "a1");
    write(&root, "src/b.ts", "b1");
    write(&root, "dist/x.js", "x1");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    git(&root, &["branch", "base"]);
    let mdir = d.path().join("manifests");
    let fx = Fx { _d: d, root, mdir };
    manifest(
        &fx,
        "a.test.ts",
        &["a.test.ts", "src/a.ts", "dist/x.js"],
        &["src/gone.ts"],
    );
    manifest(&fx, "b.test.ts", &["b.test.ts", "src/b.ts"], &[]);
    fx
}

fn manifest(fx: &Fx, test: &str, files: &[&str], absent: &[&str]) {
    let m = Manifest {
        version: 1,
        root: fx.root.to_string_lossy().into_owned(),
        command: vec!["vitest".into(), test.into()],
        exit_code: 0,
        test: test.into(),
        files: files
            .iter()
            .map(|f| FileEntry::file(*f, h(&fx.root, f)))
            .collect(),
        deps: vec![],
        deps_format: 0,
        absent: absent.iter().map(|s| (*s).to_owned()).collect(),
        exec: vec!["bin/tool".into()],
        toolchain: vec![FileEntry::file("bin/tool", h(&fx.root, "bin/tool"))],
        env: Default::default(),
        commit: Some(head(&fx.root)),
        engine: Some(ENGINE_VERSION.into()),
    };
    ManifestStore::new(&fx.mdir).put(&m).unwrap();
}

fn run(fx: &Fx) -> Selection {
    select(&SelectOptions::new(
        fx.root.clone(),
        "base".into(),
        fx.mdir.clone(),
    ))
}

fn names(s: &Selection) -> Vec<&str> {
    s.selected.iter().map(|x| x.test.as_str()).collect()
}

#[test]
fn an_unchanged_repo_selects_nothing() {
    let fx = fixture();
    let s = run(&fx);
    assert!(names(&s).is_empty() && !s.all);
    assert_eq!(
        s.counts,
        Counts {
            total: 2,
            selected: 0,
            skipped: 2
        }
    );
}

#[test]
fn editing_a_manifest_input_selects_only_its_dependents() {
    let fx = fixture();
    write(&fx.root, "src/a.ts", "a2");
    let s = run(&fx);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["input changed: src/a.ts"]);
}

#[test]
fn a_committed_edit_since_the_base_selects_dependents_too() {
    let fx = fixture();
    write(&fx.root, "src/b.ts", "b2");
    git(&fx.root, &["commit", "-qam", "edit"]);
    assert_eq!(names(&run(&fx)), ["b.test.ts"]);
}

#[test]
fn a_build_output_change_is_detected_by_hash_though_git_does_not_see_it() {
    let fx = fixture();
    write(&fx.root, "dist/x.js", "x2");
    assert!(changed_paths(&fx.root, "base").unwrap().is_empty());
    let s = run(&fx);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["input changed: dist/x.js"]);
}

#[test]
fn a_missing_input_selects() {
    let fx = fixture();
    fs::remove_file(fx.root.join("dist/x.js")).unwrap();
    assert_eq!(run(&fx).selected[0].reasons, ["input missing: dist/x.js"]);
}

#[test]
fn an_absent_path_appearing_selects() {
    let fx = fixture();
    write(&fx.root, "src/gone.ts", "now here");
    let s = run(&fx);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(
        s.selected[0].reasons,
        ["absent path now exists: src/gone.ts"]
    );
}

#[test]
fn a_changed_test_file_selects_itself() {
    let fx = fixture();
    write(&fx.root, "b.test.ts", "b changed");
    let s = run(&fx);
    assert_eq!(names(&s), ["b.test.ts"]);
    assert!(
        s.selected[0]
            .reasons
            .contains(&"test file changed".to_owned())
    );
}

#[test]
fn a_global_glob_selects_everything() {
    let fx = fixture();
    write(&fx.root, "pnpm-lock.yaml", "lock");
    let s = run(&fx);
    assert!(s.all);
    assert_eq!(names(&s), ["a.test.ts", "b.test.ts"]);
    assert!(s.all_reason.unwrap().contains("pnpm-lock.yaml"));
}

#[test]
fn a_custom_global_glob_applies() {
    let fx = fixture();
    write(&fx.root, "src/b.ts", "b2");
    let mut o = SelectOptions::new(fx.root.clone(), "base".into(), fx.mdir.clone());
    o.global = vec!["src/**".into()];
    let s = select(&o);
    assert!(s.all && s.counts.selected == 2);
}

#[test]
fn workflow_changes_are_global() {
    let fx = fixture();
    write(&fx.root, ".github/workflows/ci.yml", "x");
    assert!(run(&fx).all);
}

#[test]
fn a_corrupt_manifest_selects_everything() {
    let fx = fixture();
    fs::write(fx.mdir.join("zzz.json"), "{not json").unwrap();
    let s = run(&fx);
    assert!(s.all);
    assert!(s.all_reason.unwrap().contains("manifests unusable"));
    assert_eq!(s.counts.selected, 2);
}

#[test]
fn a_missing_manifest_directory_selects_everything() {
    let fx = fixture();
    fs::remove_dir_all(&fx.mdir).unwrap();
    assert!(run(&fx).all);
}

#[test]
fn a_new_test_without_a_manifest_selects() {
    let fx = fixture();
    write(&fx.root, "c.test.ts", "c");
    let s = run(&fx);
    assert_eq!(names(&s), ["c.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["no manifest"]);
}

#[test]
fn always_globs_select_matching_tests() {
    let fx = fixture();
    let mut o = SelectOptions::new(fx.root.clone(), "base".into(), fx.mdir.clone());
    o.always = vec!["b.*".into()];
    let s = select(&o);
    assert_eq!(names(&s), ["b.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["matches --always"]);
}

#[test]
fn a_manifest_from_a_failed_run_selects() {
    let fx = fixture();
    let store = ManifestStore::new(&fx.mdir);
    let mut m = store.load_all().unwrap().remove(0);
    m.exit_code = 1;
    store.put(&m).unwrap();
    assert_eq!(names(&run(&fx)), [m.test.as_str()]);
}

#[test]
fn a_different_engine_version_selects_everything() {
    let fx = fixture();
    let store = ManifestStore::new(&fx.mdir);
    let mut m = store.load_all().unwrap().remove(0);
    m.engine = Some("0.0.0-other".into());
    store.put(&m).unwrap();
    assert!(run(&fx).all);
}

#[test]
fn an_unknown_base_fails_closed() {
    let fx = fixture();
    let s = select(&SelectOptions::new(
        fx.root.clone(),
        "nope".into(),
        fx.mdir.clone(),
    ));
    assert!(s.all && s.counts.selected == 2);
    assert!(s.all_reason.unwrap().contains("cannot compute changes"));
}

#[test]
fn absolute_manifest_paths_are_rebased_onto_the_current_root() {
    let fx = fixture();
    let store = ManifestStore::new(&fx.mdir);
    let mut m = store.load_all().unwrap().remove(0);
    m.root = "/elsewhere/ci".into();
    m.files = vec![FileEntry::file(
        format!("/elsewhere/ci/src/{}.ts", &m.test[..1]),
        h(&fx.root, &format!("src/{}.ts", &m.test[..1])),
    )];
    store.put(&m).unwrap();
    assert!(run(&fx).selected.is_empty());
}

fn edit_all(fx: &Fx, f: impl Fn(&mut Manifest)) {
    let store = ManifestStore::new(&fx.mdir);
    for mut m in store.load_all().unwrap() {
        f(&mut m);
        store.put(&m).unwrap();
    }
}

fn all_reason(s: &Selection) -> String {
    assert!(s.all, "expected everything to be selected: {s:?}");
    assert_eq!(s.counts.selected, s.counts.total);
    s.all_reason.clone().unwrap()
}

#[test]
fn a_manifest_without_an_engine_version_selects_everything() {
    let fx = fixture();
    edit_all(&fx, |m| m.engine = None);
    assert!(all_reason(&run(&fx)).contains("no engine version"));
}

#[test]
fn a_changed_toolchain_binary_selects_everything() {
    let fx = fixture();
    write(&fx.root, "bin/tool", "tool2");
    let r = all_reason(&run(&fx));
    assert!(r.contains("toolchain changed: bin/tool"), "{r}");
}

#[test]
fn a_vanished_toolchain_binary_selects_everything() {
    let fx = fixture();
    fs::remove_file(fx.root.join("bin/tool")).unwrap();
    assert!(all_reason(&run(&fx)).contains("toolchain changed"));
}

#[test]
fn a_manifest_without_a_toolchain_fingerprint_selects_everything() {
    let fx = fixture();
    edit_all(&fx, |m| m.toolchain.clear());
    assert!(all_reason(&run(&fx)).contains("no toolchain fingerprint"));
}

#[test]
fn a_manifest_without_a_commit_selects_everything() {
    let fx = fixture();
    edit_all(&fx, |m| m.commit = None);
    assert!(all_reason(&run(&fx)).contains("records no commit"));
}

#[test]
fn a_declared_environment_variable_that_differs_selects_everything() {
    let fx = fixture();
    // A name nothing sets: recorded as set, it now reads as unset.
    edit_all(&fx, |m| {
        m.env
            .insert("GROUND_TEST_NEVER_SET_VAR".into(), Some("1".into()));
    });
    let r = all_reason(&run(&fx));
    assert!(r.contains("GROUND_TEST_NEVER_SET_VAR"), "{r}");
    // Recorded as unset, it still is: nothing changed.
    edit_all(&fx, |m| {
        m.env.insert("GROUND_TEST_NEVER_SET_VAR".into(), None);
    });
    assert!(!run(&fx).all);
}

#[test]
fn a_global_input_that_landed_on_the_base_after_the_manifests_is_found() {
    let fx = fixture();
    // The lockfile lands on main, and the base moves up to it: base...HEAD is
    // now empty, but the manifests were traced before it.
    write(&fx.root, "pnpm-lock.yaml", "lock");
    git(&fx.root, &["add", "pnpm-lock.yaml"]);
    git(&fx.root, &["commit", "-qm", "lock"]);
    git(&fx.root, &["branch", "-f", "base", "HEAD"]);
    let r = all_reason(&run(&fx));
    assert!(r.contains("global input changed: pnpm-lock.yaml"), "{r}");
}

#[test]
fn a_manifest_commit_git_does_not_know_fails_closed() {
    let fx = fixture();
    edit_all(&fx, |m| m.commit = Some("0".repeat(40)));
    assert!(all_reason(&run(&fx)).contains("cannot compute changes"));
}

#[test]
fn a_manifest_commit_that_looks_like_an_option_is_refused() {
    let fx = fixture();
    edit_all(&fx, |m| m.commit = Some("--output=/tmp/x".into()));
    assert!(all_reason(&run(&fx)).contains("malformed commit"));
}

#[test]
fn a_lockfile_above_a_package_root_is_still_global() {
    let d = tempfile::tempdir().unwrap();
    let top = d.path().join("repo");
    let pkg = top.join("pkg");
    fs::create_dir_all(&pkg).unwrap();
    git(&top, &["init", "-q", "-b", "main"]);
    write(&pkg, "a.test.ts", "a");
    write(&pkg, "tool", "t");
    git(&top, &["add", "."]);
    git(&top, &["commit", "-qm", "init"]);
    git(&top, &["branch", "base"]);
    let fx = Fx {
        _d: d,
        root: pkg.clone(),
        mdir: pkg.parent().unwrap().join("../manifests"),
    };
    let m = Manifest {
        version: 1,
        root: pkg.to_string_lossy().into_owned(),
        command: vec![],
        exit_code: 0,
        test: "a.test.ts".into(),
        files: vec![FileEntry::file("a.test.ts", h(&pkg, "a.test.ts"))],
        deps: vec![],
        deps_format: 0,
        absent: vec![],
        exec: vec!["tool".into()],
        toolchain: vec![FileEntry::file("tool", h(&pkg, "tool"))],
        env: Default::default(),
        commit: Some(head(&top)),
        engine: Some(ENGINE_VERSION.into()),
    };
    ManifestStore::new(&fx.mdir).put(&m).unwrap();
    assert!(!run(&fx).all, "nothing changed yet");
    write(&top, "pnpm-lock.yaml", "lock");
    let r = all_reason(&run(&fx));
    assert!(r.contains("pnpm-lock.yaml"), "{r}");
}

fn link_entry(fx: &Fx, path: &str) -> FileEntry {
    FileEntry {
        path: path.into(),
        blake3: hash_file(&fx.root.join(path), true).unwrap(),
        link: true,
    }
}

#[test]
fn retargeting_a_probed_symlink_to_identical_content_selects() {
    let fx = fixture();
    write(&fx.root, "src/t1", "same");
    write(&fx.root, "src/t2", "same");
    std::os::unix::fs::symlink("t1", fx.root.join("src/l")).unwrap();
    edit_all(&fx, |_| {});
    let store = ManifestStore::new(&fx.mdir);
    let mut m = store.load_all().unwrap().remove(0);
    m.files.push(link_entry(&fx, "src/l"));
    store.put(&m).unwrap();
    assert!(run(&fx).selected.is_empty());
    fs::remove_file(fx.root.join("src/l")).unwrap();
    std::os::unix::fs::symlink("t2", fx.root.join("src/l")).unwrap();
    let s = run(&fx);
    assert_eq!(names(&s), [m.test.as_str()]);
    assert_eq!(s.selected[0].reasons, ["input changed: src/l"]);
}

#[test]
fn a_dangling_symlink_counts_as_an_existing_path() {
    let fx = fixture();
    // Probed as absent when nothing was there; now a dangling link is.
    std::os::unix::fs::symlink("nowhere", fx.root.join("src/gone.ts")).unwrap();
    let s = run(&fx);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(
        s.selected[0].reasons,
        ["absent path now exists: src/gone.ts"]
    );
}

#[test]
fn a_new_unlisted_test_like_file_selects_everything_unless_tests_are_declared() {
    let fx = fixture();
    write(&fx.root, "__tests__/new.ts", "it()");
    let r = all_reason(&run(&fx));
    assert!(r.contains("__tests__/new.ts"), "{r}");
    let mut o = SelectOptions::new(fx.root.clone(), "base".into(), fx.mdir.clone());
    o.tests_declared = true;
    assert!(!select(&o).all);
}

#[test]
fn a_test_like_file_some_test_already_reads_is_not_a_surprise() {
    let fx = fixture();
    write(&fx.root, "test/helper.ts", "h1");
    git(&fx.root, &["add", "test/helper.ts"]);
    git(&fx.root, &["commit", "-qm", "helper"]);
    git(&fx.root, &["branch", "-f", "base", "HEAD~1"]);
    manifest(
        &fx,
        "a.test.ts",
        &["a.test.ts", "src/a.ts", "test/helper.ts"],
        &[],
    );
    write(&fx.root, "test/helper.ts", "h2");
    let s = run(&fx);
    assert!(!s.all);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["input changed: test/helper.ts"]);
}

#[test]
fn the_default_globs_list_type_tests() {
    let fx = fixture();
    write(&fx.root, "types/x.test-d.ts", "t");
    let s = run(&fx);
    assert_eq!(names(&s), ["types/x.test-d.ts"]);
    assert_eq!(s.selected[0].reasons, ["no manifest"]);
}

#[test]
fn test_like_paths_are_code_under_a_test_directory() {
    assert!(looks_like_test("a/__tests__/x.ts"));
    assert!(looks_like_test("test/x.mjs"));
    assert!(!looks_like_test("test/data.json"));
    assert!(!looks_like_test("src/x.ts"));
    assert!(!looks_like_test("testing/x.ts"));
}

#[test]
fn not_a_git_checkout_selects_everything_with_nothing_listed() {
    let d = tempfile::tempdir().unwrap();
    let s = select(&SelectOptions::new(
        d.path().to_owned(),
        "base".into(),
        d.path().join("m"),
    ));
    assert!(s.all && s.selected.is_empty());
    assert!(s.all_reason.unwrap().contains("enumerating tests failed"));
}

// ---- package-level dependency records ----

const ZOD: &str = "zod@3.25.76";
const CHAI: &str = "chai@5.2.0";

/// pnpm's layout under the fixture: two installed packages, each with a
/// package.json and a module file.
fn install(fx: &Fx) {
    for (dir, name) in [(ZOD, "zod"), (CHAI, "chai")] {
        let base = format!("node_modules/.pnpm/{dir}/node_modules/{name}");
        write(&fx.root, &format!("{base}/package.json"), "{}");
        write(&fx.root, &format!("{base}/index.js"), "1");
    }
}

fn dep(dir: &str, name: &str) -> DepEntry {
    DepEntry {
        dep: format!("{dir}/{name}"),
        store: "node_modules/.pnpm".into(),
    }
}

/// a.test.ts depends on zod, b.test.ts on chai, both recorded per package.
fn dep_fixture() -> Fx {
    let fx = fixture();
    install(&fx);
    edit_all(&fx, |m| {
        m.deps_format = crate::engine::manifest::DEPS_FORMAT;
        m.deps = vec![if m.test == "a.test.ts" {
            dep(ZOD, "zod")
        } else {
            dep(CHAI, "chai")
        }];
    });
    fx
}

#[test]
fn recorded_packages_that_are_still_installed_select_nothing() {
    let fx = dep_fixture();
    let s = run(&fx);
    assert!(!s.all && names(&s).is_empty(), "{s:?}");
}

#[test]
fn the_lockfile_is_not_global_once_every_manifest_records_packages() {
    let fx = dep_fixture();
    write(&fx.root, "pnpm-lock.yaml", "lock v2");
    let s = run(&fx);
    assert!(!s.all && names(&s).is_empty(), "{s:?}");
    // Other globals still are.
    write(&fx.root, "pnpm-workspace.yaml", "packages: []");
    assert!(all_reason(&run(&fx)).contains("pnpm-workspace.yaml"));
}

#[test]
fn the_lockfile_stays_global_for_a_manifest_without_package_records() {
    let fx = dep_fixture();
    edit_all(&fx, |m| {
        if m.test == "b.test.ts" {
            m.deps_format = 0;
            m.deps.clear();
        }
    });
    write(&fx.root, "pnpm-lock.yaml", "lock v2");
    assert!(all_reason(&run(&fx)).contains("pnpm-lock.yaml"));
}

#[test]
fn a_test_that_read_the_lockfile_itself_is_selected_by_its_hash() {
    let fx = dep_fixture();
    write(&fx.root, "pnpm-lock.yaml", "lock v1");
    git(&fx.root, &["add", "pnpm-lock.yaml"]);
    git(&fx.root, &["commit", "-qm", "lock"]);
    edit_all(&fx, |m| {
        m.commit = Some(head(&fx.root));
        if m.test == "b.test.ts" {
            m.files.push(FileEntry::file(
                "pnpm-lock.yaml",
                h(&fx.root, "pnpm-lock.yaml"),
            ));
        }
    });
    assert!(names(&run(&fx)).is_empty());
    write(&fx.root, "pnpm-lock.yaml", "lock v2");
    let s = run(&fx);
    assert!(!s.all, "{s:?}");
    assert_eq!(names(&s), ["b.test.ts"]);
    assert_eq!(s.selected[0].reasons, ["input changed: pnpm-lock.yaml"]);
}

#[test]
fn a_package_directory_that_is_gone_selects_only_its_dependents() {
    let fx = dep_fixture();
    // zod moved to another version: the recorded directory no longer exists.
    fs::remove_dir_all(fx.root.join(format!("node_modules/.pnpm/{ZOD}"))).unwrap();
    write(
        &fx.root,
        "node_modules/.pnpm/zod@3.25.77/node_modules/zod/package.json",
        "{}",
    );
    write(&fx.root, "pnpm-lock.yaml", "lock v2");
    let s = run(&fx);
    assert!(!s.all);
    assert_eq!(names(&s), ["a.test.ts"]);
    assert_eq!(
        s.selected[0].reasons,
        [format!("dependency not installed: {ZOD}/zod")]
    );
}

#[test]
fn a_package_directory_replaced_by_a_file_or_a_link_or_emptied_selects() {
    let fx = dep_fixture();
    let zod = fx
        .root
        .join(format!("node_modules/.pnpm/{ZOD}/node_modules/zod"));
    fs::remove_dir_all(&zod).unwrap();
    fs::write(&zod, "not a directory").unwrap();
    assert_eq!(names(&run(&fx)), ["a.test.ts"]);
    fs::remove_file(&zod).unwrap();
    std::os::unix::fs::symlink("../../chai@5.2.0/node_modules/chai", &zod).unwrap();
    assert_eq!(names(&run(&fx)), ["a.test.ts"]);
    fs::remove_file(&zod).unwrap();
    fs::create_dir(&zod).unwrap(); // present but holding no package.json
    assert_eq!(names(&run(&fx)), ["a.test.ts"]);
}

#[test]
fn no_node_modules_at_all_selects_everything() {
    let fx = dep_fixture();
    fs::remove_dir_all(fx.root.join("node_modules")).unwrap();
    let r = all_reason(&run(&fx));
    assert!(r.contains("installed packages unreadable"), "{r}");
}

#[test]
fn package_changes_inside_a_recorded_directory_are_not_looked_for() {
    // The directory name pins name, version and peers; pnpm checks integrity.
    let fx = dep_fixture();
    write(
        &fx.root,
        &format!("node_modules/.pnpm/{ZOD}/node_modules/zod/index.js"),
        "edited by hand",
    );
    assert!(names(&run(&fx)).is_empty());
}

#[test]
fn a_manifest_of_the_old_format_still_checks_every_node_modules_file() {
    let fx = fixture();
    install(&fx);
    let f = format!("node_modules/.pnpm/{ZOD}/node_modules/zod/index.js");
    edit_all(&fx, |m| {
        if m.test == "a.test.ts" {
            m.files.push(FileEntry::file(&f, h(&fx.root, &f)));
        }
    });
    assert!(names(&run(&fx)).is_empty());
    write(&fx.root, &f, "2");
    assert_eq!(names(&run(&fx)), ["a.test.ts"]);
}
