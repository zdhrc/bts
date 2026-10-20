//! Keep platform adapters downstream of the core, as required by the maintainer.
use std::{fs, path::Path};

#[test]
fn core_has_no_browser_dependencies_or_conditional_behavior() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    for dependency in ["wasm-bindgen", "js-sys", "bts-wasm"] {
        assert!(!manifest.contains(dependency), "core must not depend on {dependency}");
    }
    fn inspect(directory: &Path, core_root: &Path) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                inspect(&path, core_root);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                if path.starts_with(core_root.join("dsl")) || path.starts_with(core_root.join("sdg")) {
                    for feature in ["feature = \"cli\"", "feature = \"native\"", "feature = \"wasm\""] {
                        assert!(
                            !source.contains(feature),
                            "{} has adapter-driven feature gating: {feature}",
                            path.display()
                        );
                    }
                }
                for boundary in ["wasm_bindgen", "wasm32", "feature = \"wasm\"", "bts_wasm"] {
                    assert!(
                        !source.contains(boundary),
                        "{} crosses the consumer boundary: {boundary}",
                        path.display()
                    );
                }
            }
        }
    }
    inspect(&root.join("src"), &root.join("src"));
}
