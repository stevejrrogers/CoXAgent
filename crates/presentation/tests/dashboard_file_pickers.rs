//! COX-B056 / COX-B057 regression guard for the dashboard's file pickers.
//!
//! A file input that is out of the layout tree — `style="display:none"`, or the
//! bare `hidden` attribute, which the UA stylesheet turns into the same thing —
//! cannot be activated by clicking a `<label>` wrapped around it, so the native
//! picker never opens. That is the whole of COX-B056: "Change photo" in Edit
//! Profile did nothing. The shape shipped twice — once as the original avatar
//! markup, and again when a later commit swapped `hidden` for `display:none`
//! on an input still inside its label — and both times a live instance served
//! it, because nothing in this repo could fail on it. The browser-level
//! coverage in `tests/e2e.spec.js` needs Playwright, which CI does not run;
//! `cargo test` is what CI runs, so the invariant lives here.
//!
//! It is asserted over the exact bytes the server hands the browser: every
//! hidden file input carries an `id`, something opens it with an explicit
//! `.click()`, and none of them relies on a label wrapper.

/// The dashboard as served: the router embeds this file with `include_str!`
/// and returns it unmodified.
const DASHBOARD: &str = include_str!("../src/web/index.html");

/// The router that embeds it — read so these tests cannot drift onto a file
/// nobody serves any more.
const SERVER: &str = include_str!("../src/server/mod.rs");

/// An `<input type="file" …>` tag and the byte offset it starts at.
struct FileInput<'a> {
    tag: &'a str,
    at: usize,
}

impl FileInput<'_> {
    fn id(&self) -> Option<&str> {
        attr(self.tag, "id")
    }

    /// Out of the layout tree, so a click on anything around it never reaches
    /// it — the precondition for the whole bug class.
    fn is_hidden(&self) -> bool {
        attr(self.tag, "style").is_some_and(|s| s.replace(' ', "").contains("display:none"))
            || has_bare_attr(self.tag, "hidden")
    }
}

/// Every `<input type="file">` in the document, in source order.
fn file_inputs(html: &str) -> Vec<FileInput<'_>> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find("<input") {
        let at = cursor + offset;
        let Some(close) = html[at..].find('>') else {
            break;
        };
        let tag = &html[at..=at + close];
        if attr(tag, "type") == Some("file") {
            found.push(FileInput { tag, at });
        }
        cursor = at + close + 1;
    }
    found
}

/// Value of `name="…"` on a tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

/// A valueless attribute, as in `<input type="file" hidden>`.
fn has_bare_attr(tag: &str, name: &str) -> bool {
    tag.split_whitespace()
        .any(|token| token.trim_end_matches('>') == name)
}

/// Does the tag at `at` sit inside a `<label>`? That is the dead-picker shape:
/// the label is the only clickable thing, and its activation never arrives.
fn inside_label(html: &str, at: usize) -> bool {
    let before = &html[..at];
    match (before.rfind("<label"), before.rfind("</label>")) {
        (Some(open), Some(close)) => open > close,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Does anything open this input directly? Only a real `.click()` on the
/// element counts — label activation is exactly what does not work.
fn opened_by_click(html: &str, id: &str) -> bool {
    [
        format!("getElementById('{id}').click()"),
        format!("getElementById(\"{id}\").click()"),
        format!("querySelector('#{id}').click()"),
    ]
    .iter()
    .any(|call| html.contains(call))
}

/// 1-based line number of a byte offset, for failure messages that point at
/// the markup the way `grep -n` would.
fn line_of(html: &str, at: usize) -> usize {
    html[..at].matches('\n').count() + 1
}

#[test]
fn the_server_serves_the_markup_these_tests_guard() {
    assert!(
        SERVER.contains("include_str!(\"../web/index.html\")"),
        "the router no longer embeds src/web/index.html — this guard is now \
         asserting on markup nobody serves; point it at the file that is"
    );
}

#[test]
fn every_hidden_file_input_is_opened_by_an_explicit_click() {
    let inputs = file_inputs(DASHBOARD);
    assert!(
        !inputs.is_empty(),
        "no <input type=\"file\"> found in the dashboard — the scanner is \
         broken, which would make every assertion below vacuous"
    );

    for input in inputs.iter().filter(|i| i.is_hidden()) {
        let line = line_of(DASHBOARD, input.at);
        let Some(id) = input.id() else {
            panic!(
                "index.html:{line}: hidden file input has no id, so nothing can \
                 click it open: {}",
                input.tag
            );
        };
        assert!(
            opened_by_click(DASHBOARD, id),
            "index.html:{line}: nothing calls .click() on #{id}. A hidden input \
             opens its picker only when clicked directly (COX-B056)"
        );
    }
}

#[test]
fn no_hidden_file_input_relies_on_a_label_wrapper() {
    for input in file_inputs(DASHBOARD).iter().filter(|i| i.is_hidden()) {
        let line = line_of(DASHBOARD, input.at);
        assert!(
            !inside_label(DASHBOARD, input.at),
            "index.html:{line}: hidden file input sits inside a <label>. Being \
             out of the layout tree, it never receives the label's activation, \
             so the picker never opens — COX-B056 verbatim"
        );
    }
}

#[test]
fn the_change_photo_control_is_a_button_that_clicks_the_avatar_input() {
    // The ticket's own repro was `curl :4000/ | grep -n "Change photo"`, which
    // returned the label line. Assert on the same line it greps.
    let Some(control) = DASHBOARD.lines().find(|l| l.contains("Change photo")) else {
        panic!("the Edit Profile avatar control is gone from the dashboard");
    };
    assert!(
        control.contains("<button"),
        "the \"Change photo\" control must be a button that clicks the input, \
         not a label wrapping it: {control}"
    );
    assert!(
        control.contains("getElementById('pp-avatar-file').click()"),
        "the \"Change photo\" button must open #pp-avatar-file explicitly: {control}"
    );
}

/// The avatar markup that shipped the bug, verbatim from the pre-fix
/// dashboard. The guards above are only worth having if they fail on it.
const SHIPPED_BROKEN: &str = r#"<div id="pp-profile" hidden>
  <label class="btn-ghost" style="cursor:pointer;display:inline-flex"><i class="ti ti-camera"></i> Change photo
    <input type="file" accept="image/*" style="display:none" onchange="uploadAvatar(this)"></label>
</div>"#;

/// The same failure wearing the other hat: a bare `hidden` attribute instead of
/// `display:none`. Same layout tree, same dead picker.
const SHIPPED_BROKEN_HIDDEN_ATTR: &str = r#"<label class="btn-ghost"> Change photo<input type="file" accept="image/*" hidden onchange="uploadAvatar(this)"></label>"#;

#[test]
fn the_guards_reject_the_markup_that_shipped_the_bug() {
    for (name, markup) in [
        ("display:none", SHIPPED_BROKEN),
        ("hidden attribute", SHIPPED_BROKEN_HIDDEN_ATTR),
    ] {
        let inputs = file_inputs(markup);
        let Some(input) = inputs.first() else {
            panic!("{name}: the scanner did not even find the file input");
        };
        assert_eq!(inputs.len(), 1, "{name}: expected exactly one file input");
        assert!(
            input.is_hidden(),
            "{name}: the hidden-input detector missed it, so the guards would pass"
        );
        assert!(
            input.id().is_none(),
            "{name}: the shipped markup had no id — the fixture drifted"
        );
        assert!(
            inside_label(markup, input.at),
            "{name}: the label detector missed the exact wrapper COX-B056 diagnosed"
        );
    }
}
