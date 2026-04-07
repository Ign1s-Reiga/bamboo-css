use bamboo_css_macro::css;

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
