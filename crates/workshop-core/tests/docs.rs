//! The generated reference pages in `docs/book/reference/` must match the code.
//! `UPDATE_DOCS=1 cargo test-host` rewrites them.

use std::path::PathBuf;

#[test]
fn generated_docs_are_up_to_date() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/book/reference");
    let update = std::env::var_os("UPDATE_DOCS").is_some();
    let mut stale = Vec::new();
    for (name, generate) in workshop_core::docgen::PAGES {
        let mut text = String::new();
        generate(&mut text).unwrap();
        let path = dir.join(name);
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &text).unwrap();
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(name);
        }
    }
    assert!(stale.is_empty(), "out of date: {stale:?}. Run `UPDATE_DOCS=1 cargo test-host` and commit the result.");
}
