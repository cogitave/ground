use super::{BumpedPackage, scan_bumps};
use crate::ports::{PrFile, PrFileStatus};

fn file(path: &str, status: PrFileStatus, patch: Option<&[&str]>) -> PrFile {
    PrFile {
        path: path.into(),
        status,
        patch: patch.map(|l| l.join("\n")),
    }
}

// bumps.test.ts: "reads name and versions from a standard hunk"
#[test]
fn reads_name_and_versions_from_a_standard_hunk() {
    let files = [file(
        "packages/sdk/package.json",
        PrFileStatus::Modified,
        Some(&[
            "@@ -1,5 +1,5 @@",
            " {",
            "   \"name\": \"@namzu/sdk\",",
            "-  \"version\": \"1.0.0\",",
            "+  \"version\": \"1.1.0\",",
            "   \"type\": \"module\",",
        ]),
    )];
    let scan = scan_bumps(&files);
    assert_eq!(
        scan.packages,
        vec![BumpedPackage {
            name: "@namzu/sdk".into(),
            from: Some("1.0.0".into()),
            to: "1.1.0".into()
        }]
    );
    assert!(scan.unresolved.is_empty());
}

// bumps.test.ts: "ignores nested versions, private packages, non-package files and unchanged versions"
#[test]
fn ignores_nested_versions_private_packages_non_package_files_and_unchanged_versions() {
    let files = [
        file(
            "CHANGELOG.md",
            PrFileStatus::Modified,
            Some(&["-  \"version\": \"1\"", "+  \"version\": \"2\""]),
        ),
        file(
            "apps/x/package.json",
            PrFileStatus::Modified,
            Some(&[
                " {",
                "   \"name\": \"x\",",
                "   \"private\": true,",
                "-  \"version\": \"1.0.0\",",
                "+  \"version\": \"1.0.1\",",
            ]),
        ),
        file(
            "packages/a/package.json",
            PrFileStatus::Modified,
            Some(&[
                "   \"name\": \"a\",",
                "     \"version\": \"9.9.9\",",
                "+    \"version\": \"9.9.10\",",
            ]),
        ),
        file(
            "packages/b/package.json",
            PrFileStatus::Modified,
            Some(&[
                "   \"name\": \"b\",",
                "-  \"left\": \"1\",",
                "+  \"left\": \"2\",",
            ]),
        ),
    ];
    let scan = scan_bumps(&files);
    assert!(scan.packages.is_empty());
    assert!(scan.unresolved.is_empty());
}

// bumps.test.ts: "reports a package whose name is not in the diff instead of guessing"
#[test]
fn reports_a_package_whose_name_is_not_in_the_diff_instead_of_guessing() {
    let files = [
        file(
            "packages/c/package.json",
            PrFileStatus::Modified,
            Some(&["-  \"version\": \"1.0.0\",", "+  \"version\": \"1.0.1\","]),
        ),
        file("packages/d/package.json", PrFileStatus::Modified, None),
    ];
    let scan = scan_bumps(&files);
    assert!(scan.packages.is_empty());
    assert_eq!(
        scan.unresolved,
        vec!["packages/c/package.json", "packages/d/package.json"]
    );
}

// bumps.test.ts: "treats a new package as a bump from nothing"
#[test]
fn treats_a_new_package_as_a_bump_from_nothing() {
    let files = [file(
        "packages/n/package.json",
        PrFileStatus::Added,
        Some(&["+{", "+  \"name\": \"n\",", "+  \"version\": \"0.1.0\","]),
    )];
    assert_eq!(
        scan_bumps(&files).packages,
        vec![BumpedPackage {
            name: "n".into(),
            from: None,
            to: "0.1.0".into()
        }]
    );
}
