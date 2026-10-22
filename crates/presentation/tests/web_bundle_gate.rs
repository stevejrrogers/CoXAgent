//! Every dashboard script must actually ship: present in the server's
//! embedded asset list (`APP_JS` in server/mod.rs) AND loaded by
//! `index.html`.
//!
//! The trap this kills hit THREE times in one week: a module is added under
//! `web/js/`, its tests `include_str!` it directly and pass, nothing loads it
//! in the real page, and the first runtime caller gets `undefined` — copy.js
//! (silent toasts, #780/#781/#782) and overview_vm.js (unmounted Overview,
//! caught in #786's review). Classic scripts share one scope and fail
//! silently; this gate makes the failure loud at test time instead.

use std::path::PathBuf;

const INDEX_HTML: &str = include_str!("../src/web/index.html");
const SERVER_MOD: &str = include_str!("../src/server/mod.rs");

/// Vendored or specially-served scripts index.html loads through other
/// routes (or that are loaded lazily by code, not by a script tag).
/// Shrink-only: a NEW first-party module never belongs here.
const EXCEPTIONS: [&str; 4] = [
    "mermaid.min.js",
    "xterm.min.js",
    "xterm-addon-fit.min.js",
    // Loaded on demand by the docs view, not part of the shell bundle.
    "mermaid-init.js",
];

#[test]
fn every_web_js_module_is_embedded_and_loaded() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/web/js");
    let mut missing: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read web/js") {
        let name = entry.expect("dir entry").file_name();
        let name = name.to_string_lossy().into_owned();
        if !name.ends_with(".js") || EXCEPTIONS.contains(&name.as_str()) {
            continue;
        }
        let embedded = SERVER_MOD.contains(&format!("web/js/{name}"));
        let loaded = INDEX_HTML.contains(&format!("/assets/js/{name}"));
        if !embedded || !loaded {
            missing.push(format!(
                "{name}: embedded_in_APP_JS={embedded}, loaded_by_index_html={loaded}"
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "web/js module(s) exist but do not ship — register in APP_JS \
         (server/mod.rs) and add a <script> tag in index.html (order matters: \
         helpers before their consumers), or add a vendored file to the \
         shrink-only EXCEPTIONS list:\n{}",
        missing.join("\n")
    );
}
