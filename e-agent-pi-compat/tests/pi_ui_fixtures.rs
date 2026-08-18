use std::{fs, path::Path};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct Manifest {
    pi_version: String,
    package: String,
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    path: String,
    sha256: String,
    source: String,
    provenance: String,
}

#[test]
fn pinned_pi_ui_fixtures_are_complete_and_unmodified() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-0.84.2");
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(root.join("manifest.json")).expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");

    assert_eq!(manifest.pi_version, "0.84.2");
    assert_eq!(manifest.package, "@earendil-works/pi-coding-agent");
    assert!(manifest.fixtures.len() >= 12, "fixture coverage regressed");

    for fixture in &manifest.fixtures {
        let bytes = fs::read(root.join(&fixture.path))
            .unwrap_or_else(|error| panic!("read {}: {error}", fixture.path));
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            fixture.sha256,
            "fixture was modified: {}",
            fixture.path
        );
        assert!(
            fixture.source.starts_with("examples/")
                || fixture.provenance == "documentation-derived",
            "fixture source must be explicit: {}",
            fixture.path
        );
    }

    let names = manifest
        .fixtures
        .iter()
        .map(|fixture| fixture.path.as_str())
        .collect::<Vec<_>>();
    for required in [
        "extensions/overlay-qa-tests.ts",
        "extensions/modal-editor.ts",
        "extensions/custom-header.ts",
        "extensions/custom-footer.ts",
        "extensions/widget-placement.ts",
        "extensions/working-indicator.ts",
        "extensions/github-issue-autocomplete.ts",
        "extensions/todo.ts",
        "extensions/message-renderer.ts",
        "extensions/entry-renderer.ts",
        "extensions/mac-system-theme.ts",
        "markdown-transformer.ts",
        "terminal-input-subscription.ts",
    ] {
        assert!(
            names.contains(&required),
            "missing required fixture: {required}"
        );
    }
}
