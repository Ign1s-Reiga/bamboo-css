use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Reconstructs a CSS string from a TokenStream using a CSS-aware heuristic:
/// a space is inserted between two adjacent tokens only when neither is a
/// punctuation character and the incoming token is not a group.  This keeps
/// compound property names (`background-color`), percentage values (`50%`),
/// function calls (`rgba(…)`), and pseudo-selectors (`&:hover`) tight, while
/// correctly separating adjacent values (`50px 50px`, `0.15s ease`).
fn tokens_to_css(input: TokenStream2) -> String {
    let tokens: Vec<TokenTree> = input.into_iter().collect();
    let mut out = String::new();
    // `prev_is_punct = true` at the start so the first token never gets a
    // leading space.
    append_tokens(&tokens, &mut out, true);
    out
}

/// Appends the text of `tokens` to `out`, inserting spaces according to the
/// CSS-aware rule described on `tokens_to_css`.
///
/// `prev_is_punct` should be `true` when the character immediately preceding
/// the first token in `tokens` is a punctuation character (or the buffer is
/// empty / we just opened a delimiter), so that no spurious leading space is
/// emitted.
fn append_tokens(tokens: &[TokenTree], out: &mut String, mut prev_is_punct: bool) {
    for tt in tokens {
        let is_punct = matches!(tt, TokenTree::Punct(_));
        let is_group = matches!(tt, TokenTree::Group(_));

        // Insert a space only when neither the previous nor the current token
        // is a punctuation character, and the current token is not a group.
        if !prev_is_punct && !is_punct && !is_group {
            out.push(' ');
        }

        match tt {
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                // After the opening delimiter, treat it like a punct so the
                // first inner token does not get a spurious leading space.
                append_tokens(&inner, out, true);
                out.push_str(close);
            }
            TokenTree::Ident(id) => out.push_str(&id.to_string()),
            TokenTree::Punct(p) => out.push(p.as_char()),
            TokenTree::Literal(lit) => out.push_str(&lit.to_string()),
        }

        prev_is_punct = is_punct;
    }
}

/// Concatenates every token's text without any separators, recursing into groups.
fn tokens_to_hash_input(tokens: TokenStream2) -> String {
    let mut out = String::new();
    for tt in tokens {
        match tt {
            TokenTree::Ident(id) => out.push_str(&id.to_string()),
            TokenTree::Punct(p) => out.push(p.as_char()),
            TokenTree::Literal(lit) => out.push_str(&lit.to_string()),
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                out.push_str(&tokens_to_hash_input(g.stream()));
                out.push_str(close);
            }
        }
    }
    out
}

/// Returns a CSS class name like `css-a1b2c3d4` derived from the CSS body.
fn generate_hash(css_body: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    css_body.hash(&mut h);
    format!("css-{:08x}", h.finish() as u32)
}

/// Wraps the CSS body in `.{hash} { … }`, runs it through lightningcss
/// (nesting resolution, vendor prefixes, minification), and returns the
/// resulting CSS string.
fn process_css(hash: &str, body: &str) -> Result<String, String> {
    use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};

    // Wrap in the scoped class so that `&` refers to `.{hash}` via CSS nesting.
    let scoped = format!(".{hash} {{{body}}}");

    let sheet = StyleSheet::parse(&scoped, ParserOptions::default())
        .map_err(|e| format!("CSS parse error: {e}"))?;

    let result = sheet
        .to_css(PrinterOptions {
            minify: true,
            ..Default::default()
        })
        .map_err(|e| format!("CSS print error: {e:?}"))?;

    Ok(result.code)
}

fn find_workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(PathBuf::from(&manifest_dir).join("Cargo.toml"))
        .no_deps()
        .exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;

    Ok(metadata.workspace_root.into())
}

/// Writes `target/styled-fragments/{hash}.css` under the workspace root.
fn write_fragment(workspace_root: &PathBuf, hash: &str, css: &str) -> Result<(), String> {
    let dir = workspace_root.join("target").join("styled-fragments");
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create fragments dir: {e}"))?;

    let path = dir.join(format!("{hash}.css"));

    // Uses `create_new` so parallel macro invocations that produce the same
    // hash (identical CSS) never race: only one writer succeeds; the rest hit
    // `AlreadyExists` and skip silently.
    match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => f
            .write_all(css.as_bytes())
            .map_err(|e| format!("failed to write fragment: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Same hash ⟹ same content; nothing to do.
        }
        Err(e) => return Err(format!("failed to open fragment for writing: {e}")),
    }

    Ok(())
}

/// Validates, scopes, and extracts CSS at compile time, returning the
/// auto-generated class name as a `&'static str`.
///
/// The CSS body is processed through [lightningcss](https://lightningcss.dev/):
/// nesting is resolved, vendor prefixes are added, and the output is minified.
/// Styles are scoped to an auto-generated hash class (e.g. `.css-a1b2c3d4`),
/// so they never leak to other elements.  A CSS fragment is written to
/// `target/styled-fragments/{hash}.css`; `bamboo-css-collector` picks these up
/// before each Trunk build and assembles the final bundle.
///
/// A `compile_error!` is emitted if the CSS is invalid, giving you IDE
/// diagnostics without a runtime panic.
///
/// # Syntax
///
/// ```text
/// css! { /* CSS properties and nested rules */ }
/// ```
///
/// The `&` selector refers to the scoped class, just like in CSS nesting.
///
/// # Example (Leptos)
///
/// ```rust
/// use bamboo_css_macro::css;
///
/// #[component]
/// fn MyButton() -> impl IntoView {
///     let class = css! {
///         padding: 0.5rem 1rem;
///         border-radius: 4px;
///         background-color: royalblue;
///         color: white;
///
///         &:hover {
///             background-color: steelblue;
///         }
///     };
///
///     view! { <button class=class>"Click me"</button> }
/// }
/// ```
#[proc_macro]
pub fn css(input: TokenStream) -> TokenStream {
    let input2: TokenStream2 = input.into();

    let hash = generate_hash(&tokens_to_hash_input(input2.clone()));
    let css_body = tokens_to_css(input2);

    // Validate + process CSS; emit compile_error! on failure.
    let processed = match process_css(&hash, &css_body) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("bamboo-css: {e}");
            return quote! { compile_error!(#msg) }.into();
        }
    };

    let write_result = find_workspace_root()
        .and_then(|root| write_fragment(&root, &hash, &processed));

    if let Err(e) = write_result {
        let msg = format!("bamboo-css: {e}");
        return quote! { compile_error!(#msg) }.into();
    }

    let lit = proc_macro2::Literal::string(&hash);
    quote! { #lit }.into()
}

/// Parses `ComponentName , tag , { css_body }` from the input token stream.
/// - `ComponentName`: bare ident — the name of the generated Leptos component function
/// - `tag`: bare ident or string literal — the HTML element to render
/// - `{ css_body }`: brace-delimited CSS
fn parse_styled_args(input: TokenStream2) -> Option<(String, String, TokenStream2)> {
    let mut iter = input.into_iter();

    // First token: component function name (must be a bare ident)
    let component = match iter.next()? {
        TokenTree::Ident(id) => id.to_string(),
        _ => return None,
    };

    // Separator
    match iter.next()? {
        TokenTree::Punct(p) if p.as_char() == ',' => {}
        _ => return None,
    }

    // Second token: HTML tag name
    let tag = match iter.next()? {
        TokenTree::Ident(id) => id.to_string(),
        TokenTree::Literal(lit) => lit.to_string().trim_matches('"').to_string(),
        _ => return None,
    };

    // Separator
    match iter.next()? {
        TokenTree::Punct(p) if p.as_char() == ',' => {}
        _ => return None,
    }

    // CSS block — must be brace-delimited
    let css = match iter.next()? {
        TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => g.stream(),
        tt => {
            let mut ts = TokenStream2::new();
            ts.extend(std::iter::once(tt));
            ts.extend(iter);
            ts
        }
    };

    Some((component, tag, css))
}

/// Defines a scoped Leptos component backed by a plain HTML element.
///
/// Processes the CSS through the same pipeline as `css!` (hash, validate,
/// minify, write fragment) and emits a `#[component]` function with the given
/// name.
///
/// **Void elements** (`input`, `img`, `br`, …) generate a component with no
/// `children` prop.  Arbitrary HTML attributes (e.g. `attr:type`,
/// `attr:value`) are forwarded to the inner element via `AttributeInterceptor`.
///
/// **All other elements** generate a component that accepts `children` and
/// renders them inside the scoped element.
///
/// The scoped class is always applied; it cannot be overridden by callers.
///
/// # Syntax
///
/// ```text
/// styled!(ComponentName, tag, { /* CSS */ });
/// ```
///
/// - `ComponentName` — the identifier of the generated Leptos component
/// - `tag` — a bare HTML element name (`div`, `button`, `span`, …) or a
///   double-quoted string literal (`"div"`)
///
/// # Example (Leptos)
///
/// ```rust
/// use bamboo_css_macro::styled;
///
/// // Normal element — accepts children
/// styled!(Card, div, {
///     padding: 1rem;
///     border-radius: 8px;
///     box-shadow: 0 2px 8px rgba(0,0,0,0.1);
/// });
///
/// // Void element — no children, props forwarded as HTML attributes
/// styled!(StyledInput, input, {
///     border: none;
///     padding: 0.5rem;
/// });
///
/// #[component]
/// fn App() -> impl IntoView {
///     view! {
///         <Card><p>"Hello"</p></Card>
///         <StyledInput attr:type="text" attr:placeholder="Enter text…" />
///     }
/// }
/// ```
#[proc_macro]
pub fn styled(input: TokenStream) -> TokenStream {
    let (component, tag, css_tokens) = match parse_styled_args(input.into()) {
        Some(v) => v,
        None => {
            return quote! {
                compile_error!("bamboo-css: styled! expects `styled!(ComponentName, tag, { /* CSS */ })`")
            }
            .into();
        }
    };

    let hash = generate_hash(&tokens_to_hash_input(css_tokens.clone()));
    let css_body = tokens_to_css(css_tokens);

    let processed = match process_css(&hash, &css_body) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("bamboo-css: {e}");
            return quote! { compile_error!(#msg) }.into();
        }
    };

    if let Err(e) =
        find_workspace_root().and_then(|root| write_fragment(&root, &hash, &processed))
    {
        let msg = format!("bamboo-css: {e}");
        return quote! { compile_error!(#msg) }.into();
    }

    let component_ident =
        proc_macro2::Ident::new(&component, proc_macro2::Span::call_site());
    let tag_ident = proc_macro2::Ident::new(&tag, proc_macro2::Span::call_site());
    let hash_lit = proc_macro2::Literal::string(&hash);

    // Cannot have children or a closing tag.
    const VOID_ELEMENTS: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
        "param", "source", "track", "wbr",
    ];

    if VOID_ELEMENTS.contains(&tag.as_str()) {
        // Void/self-closing element: no children prop.  Arbitrary HTML
        // attributes (e.g. `attr:type`, `attr:value`, `attr:placeholder`) are
        // forwarded to the inner element via `AttributeInterceptor`.
        quote! {
            #[::leptos::component]
            fn #component_ident() -> impl ::leptos::IntoView {
                use ::leptos::prelude::AddAnyAttr;
                use ::leptos::attribute_interceptor::AttributeInterceptor;
                ::leptos::view! {
                    <AttributeInterceptor let:attr>
                        <#tag_ident class=#hash_lit  {..attr}/>
                    </AttributeInterceptor>
                }
            }
        }
        .into()
    } else {
        // Normal element: accepts children rendered inside the scoped element.
        // `Children` (`Box<dyn FnOnce() -> AnyView>`) is called once directly
        // in the component body — no outer `Fn` closure is needed here.
        quote! {
            #[::leptos::component]
            fn #component_ident(
                children: ::leptos::children::Children,
            ) -> impl ::leptos::IntoView {
                ::leptos::view! {
                    <#tag_ident class=#hash_lit>
                        {children()}
                    </#tag_ident>
                }
            }
        }
        .into()
    }
}

/// Splits a `TokenStream` on top-level commas.
fn split_by_comma(input: TokenStream2) -> Vec<TokenStream2> {
    let mut args: Vec<TokenStream2> = Vec::new();
    let mut current = TokenStream2::new();

    for tt in input {
        match &tt {
            TokenTree::Punct(p) if p.as_char() == ',' => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current = TokenStream2::new();
                }
            }
            _ => current.extend(std::iter::once(tt)),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Joins one or more class-name expressions into a single space-separated
/// `String` at runtime, skipping any value that is empty after conversion.
///
/// Each argument may be any expression that implements `Into<String>` —
/// typically a `&str` literal, the result of `css!`, or a conditional
/// expression such as `if condition { active_class } else { "" }`.
///
/// # Example (Leptos)
///
/// ```rust
/// use bamboo_css_macro::{css, cx};
///
/// #[component]
/// fn Button(active: ReadSignal<bool>) -> impl IntoView {
///     let base = css! { padding: 0.5rem 1rem; border-radius: 4px; };
///     let highlighted = css! { background-color: royalblue; color: white; };
///
///     view! {
///         <button class=cx!(base, if active.get() { highlighted } else { "" })>
///             "Click"
///         </button>
///     }
/// }
/// ```
#[proc_macro]
pub fn cx(input: TokenStream) -> TokenStream {
    let args = split_by_comma(input.into());

    let stmts = args.iter().map(|arg| {
        quote! {
            {
                let __s = ::std::string::String::from(#arg);
                if !__s.is_empty() {
                    __parts.push(__s);
                }
            }
        }
    });

    quote! {
        {
            let mut __parts: ::std::vec::Vec<::std::string::String> =
                ::std::vec::Vec::new();
            #(#stmts)*
            __parts.join(" ")
        }
    }
    .into()
}
