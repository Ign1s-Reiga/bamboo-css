use bamboo_css_macro::css;

#[test]
fn test_css_macro() {
    let style = css! {
        background-color: red;
        width: 50%;
        &:hover {
            background-color: blue;
        }
    };
    println!("{style}");
    assert!(!style.is_empty());
    assert!(style.starts_with("css-"));
}
