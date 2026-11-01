//! `docs/code-map.md` must mention every source file of the workspace, in
//! the section of its crate (`## <crate>`): by its path relative to `src/`,
//! or, for a shader, by its name in backticks.

use std::fs;
use std::path::{Path, PathBuf};

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            source_files(&path, out);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "wgsl")) {
            out.push(path);
        }
    }
}

/// Text of the `## <crate>` section, up to the next `## ` heading.
fn section<'a>(map: &'a str, krate: &str) -> Option<&'a str> {
    let start = map.find(&format!("\n## {krate}\n"))? + 1;
    let rest = &map[start + 3..];
    let end = rest.find("\n## ").map_or(rest.len(), |i| i + 1);
    Some(&rest[..end])
}

#[test]
fn code_map_lists_every_source_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let map = fs::read_to_string(root.join("docs/code-map.md"))
        .expect("docs/code-map.md exists")
        .replace("\r\n", "\n");
    let mut missing = Vec::new();
    for krate in fs::read_dir(root.join("crates")).expect("crates directory") {
        let krate = krate.expect("readable directory entry").path();
        let name = krate.file_name().unwrap().to_string_lossy().into_owned();
        let Some(text) = section(&map, &name) else {
            missing.push(format!("section `## {name}`"));
            continue;
        };
        let src = krate.join("src");
        let mut files = Vec::new();
        source_files(&src, &mut files);
        for file in files {
            let rel = file.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            let stem = file.file_stem().unwrap().to_string_lossy();
            let shader_name = rel.ends_with(".wgsl") && text.contains(&format!("`{stem}`"));
            if !text.contains(&rel) && !shader_name {
                missing.push(format!("{name}/src/{rel}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "missing from docs/code-map.md (see the code-map skill): {missing:?}"
    );
}
