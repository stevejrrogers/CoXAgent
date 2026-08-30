//! CXA-B113 regression: the primary button (`.pri`, e.g. the sign-in CTA)
//! renders a 13px/600 white label, so WCAG AA demands a 4.5:1 fill contrast.
//! The bare accent (#0891B2) reaches only 3.68:1; the dedicated button tokens
//! must stay AA-safe, and `.pri` rules must never fall back to `--accent`.

use super::APP_CSS;

/// Value of a `--name:#rrggbb;` declaration in the token block.
fn token_hex(css: &str, name: &str) -> Option<String> {
    let needle = format!("{name}:#");
    let start = css.find(&needle)? + needle.len();
    let rest = &css[start..];
    let end = rest.find(';')?;
    Some(rest[..end].trim().to_string())
}

fn srgb_to_linear(channel: f64) -> f64 {
    if channel <= 0.03928 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(hex: &str) -> f64 {
    let digits = hex.trim_start_matches('#');
    let channel = |i: usize| -> f64 {
        f64::from(u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).expect("valid hex channel"))
            / 255.0
    };
    0.2126 * srgb_to_linear(channel(0))
        + 0.7152 * srgb_to_linear(channel(1))
        + 0.0722 * srgb_to_linear(channel(2))
}

fn contrast_vs_white(hex: &str) -> f64 {
    1.05 / (luminance(hex) + 0.05)
}

#[test]
fn pri_button_fills_meet_wcag_aa_against_white_labels() {
    let bg = token_hex(APP_CSS, "--pri-bg").expect("--pri-bg token declared");
    let bgh = token_hex(APP_CSS, "--pri-bgh").expect("--pri-bgh token declared");

    for (token, hex) in [("--pri-bg", bg), ("--pri-bgh", bgh)] {
        let ratio = contrast_vs_white(&hex);
        assert!(
            ratio >= 4.5,
            "{token} ({hex}) gives {ratio:.2}:1 against the white .pri label; WCAG AA needs 4.5:1"
        );
    }
}

#[test]
fn pri_rules_draw_from_aa_safe_tokens_not_the_bare_accent() {
    // The canonical `.pri` rule is the only one at column 0; the others are
    // component-scoped overrides that re-assert the fill.
    for rule in ["\n.pri{", ".gc-btn.pri{", ".ctlrow button.pri{"] {
        let start = APP_CSS
            .find(rule)
            .unwrap_or_else(|| panic!("{rule} rule missing from app.css"));
        let body = &APP_CSS[start..];
        let body = &body[..body.find('}').expect("rule body closed")];
        assert!(
            body.contains("background:var(--pri-bg)"),
            "{rule} must fill with the AA-safe --pri-bg, got: {body}"
        );
        assert!(
            !body.contains("var(--accent"),
            "{rule} must not re-assert the bare accent (3.68:1 with white): {body}"
        );
    }

    assert!(APP_CSS.contains(".pri:hover{background:var(--pri-bgh)}"));
    assert!(APP_CSS.contains(".ctlrow button.pri:hover{background:var(--pri-bgh)}"));
}
