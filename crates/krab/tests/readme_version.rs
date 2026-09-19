//! The install snippet in README.md names a release version, and it has to be
//! the current one or the `curl` in it 404s. That is the only version string
//! in the documentation that has to track the manifest; the rest either says
//! nothing or derives the number at the point of use.
//!
//! Bumping the workspace version therefore fails this test until README.md is
//! bumped with it, which is the point.

use std::path::PathBuf;

#[test]
fn the_readme_install_snippet_names_the_current_version() {
    let readme_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../README.md")
        .canonicalize()
        .expect("README.md exists");
    let readme = std::fs::read_to_string(&readme_path).expect("reading README.md");

    // `version=2.0.0-alpha.4 target=...` in the install snippet. Prose that
    // names an older release on purpose ("releases before X were called Y")
    // does not match and must not be rewritten.
    let documented = readme
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("version="))
        .map(|rest| rest.split_whitespace().next().unwrap_or_default())
        .expect("README.md has a `version=...` line in the install snippet");

    assert_eq!(
        documented,
        env!("CARGO_PKG_VERSION"),
        "the install snippet in {} names {documented}, the workspace is at {}. \
         Bump the snippet, or the download URL in it will 404.",
        readme_path.display(),
        env!("CARGO_PKG_VERSION"),
    );
}
