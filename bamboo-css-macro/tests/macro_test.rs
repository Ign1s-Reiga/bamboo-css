use bamboo_css_macro::css;
use std::path::PathBuf;

/// Reads back the CSS fragment the macro wrote for `hash`.
fn fragment(hash: &str) -> String {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the macro crate sits inside the workspace")
        .to_path_buf();
    let path = workspace_root
        .join("target")
        .join("styled-fragments")
        .join(format!("{hash}.css"));

    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

#[test]
fn test_css_macro() {
    let style = css! {
        background-color: red;
        width: 50%;
        padding: 50px 50px;
        transition: background-color 0.15s ease;
        &:hover {
            background-color: blue;
        }
    };
    println!("{style}");
    assert!(!style.is_empty());
    assert!(style.starts_with("css-"));
}

/// The unit tests in `src/lib.rs` reach `tokens_to_css` through streams parsed
/// from a string, which carry proc-macro2's own fallback span locations.  This
/// one goes through a real expansion, so it is the only check that the
/// *compiler's* spans still report locations: were a toolchain to stop exposing
/// them, the macro would quietly drop back to the token-kind heuristic and
/// start emitting `&img` and `calc(100%+4px)` again — with nothing failing
/// until someone looked at the rendered page.
#[test]
fn source_spacing_survives_a_real_expansion() {
    let style = css! {
        & img { width: 28px }
        &:hover { top: calc(100% + 4px) }
    };

    let css = fragment(style);
    assert!(css.contains("& img{"), "descendant selector collapsed: {css}");
    assert!(css.contains("&:hover{"), "pseudo-class gained a space: {css}");
    assert!(css.contains("calc(100% + 4px)"), "calc lost its spaces: {css}");
}
