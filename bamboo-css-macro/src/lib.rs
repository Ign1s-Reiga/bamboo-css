use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, LineColumn, Span, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

/// Reconstructs a CSS string from a `TokenStream`, restoring the whitespace
/// the tokens carried in the user's source file.
///
/// Whitespace is significant in CSS in places the token kinds cannot reveal:
/// `& img` (descendant) versus `&img` (compound), `calc(100% + 4px)` versus the
/// unparseable `calc(100%+4px)`, `translateX(-4px)` versus the invalid
/// `translateX(- 4px)`.  Which of the two was written is a property of the
/// source text, not of the tokens, so the spacing is recovered from span
/// locations: two tokens were adjacent in the source exactly when the first
/// one's end position is the second one's start position.
///
/// `Punct::spacing()` cannot stand in for this — it only reports whether a
/// punct is glued to the *next punct*, so the `&` of both `& img` and `&:hover`
/// is `Alone`.
///
/// Tokens with no usable location — synthetic streams built by `quote!`, or a
/// toolchain that does not expose span locations — fall back to the earlier
/// heuristic: a space between two non-punctuation tokens.
fn tokens_to_css(input: TokenStream2) -> String {
    let tokens: Vec<TokenTree> = input.into_iter().collect();
    let mut out = String::new();
    Spacer::new().append_tokens(&tokens, &mut out);
    out
}

/// Returns where `span` starts and ends in the source, or `None` when it
/// carries no usable location.
///
/// Every real token covers at least one column, so a zero-width span means
/// either a synthetic token or a toolchain without span locations — proc-macro2
/// reports a single fixed position for every such span, which would make every
/// pair of tokens look adjacent.
fn span_bounds(span: Span) -> Option<(LineColumn, LineColumn)> {
    let (start, end) = (span.start(), span.end());
    if start == end { None } else { Some((start, end)) }
}

/// Tracks what was written last so the right amount of whitespace goes in front
/// of the next token.
struct Spacer {
    /// Source position just past the previously written token, when it had one.
    prev_end: Option<LineColumn>,
    /// Whether the previously written token was a punctuation character.  Only
    /// consulted when a location is missing on either side of the gap.
    prev_is_punct: bool,
}

impl Spacer {
    /// Starts as if a punctuation character preceded the stream, so the first
    /// token never picks up a leading space.
    fn new() -> Self {
        Self { prev_end: None, prev_is_punct: true }
    }

    /// Writes the separator that belongs in front of a token which starts at
    /// `start` in the source.
    ///
    /// Known limitation: rustc drops comments before the macro sees the tokens
    /// but leaves the gap they occupied, so a comment written *inside* a
    /// selector reads here as the whitespace that replaced it — `&/**/:hover`
    /// becomes `& :hover`, a descendant selector.  Nothing in the token stream
    /// separates an elided comment from real whitespace; the APIs that could,
    /// `Span::join` and `Span::byte_range`, are both nightly-only.  Comments
    /// between declarations or on their own line are unaffected, since the
    /// newline around them is whitespace anyway.
    fn separate(
        &self,
        out: &mut String,
        start: Option<LineColumn>,
        is_punct: bool,
        is_group: bool,
    ) {
        let space = match (self.prev_end, start) {
            // Both sides located, and in source order: the source had
            // whitespace here exactly when the two positions do not meet.
            (Some(prev_end), Some(start)) if start >= prev_end => start != prev_end,
            // Either side is unlocated, or the positions run backwards because
            // the tokens were spliced in from elsewhere.  Neither can be
            // compared, so fall back to the token-kind heuristic.
            _ => !self.prev_is_punct && !is_punct && !is_group,
        };
        if space {
            out.push(' ');
        }
    }

    /// Records the token just written as the new left-hand side of the next gap.
    fn advance(&mut self, end: Option<LineColumn>, is_punct: bool) {
        self.prev_end = end;
        self.prev_is_punct = is_punct;
    }

    /// Appends the text of `tokens` to `out`, separated as they were in source.
    fn append_tokens(&mut self, tokens: &[TokenTree], out: &mut String) {
        for tt in tokens {
            let (text, is_punct) = match tt {
                TokenTree::Group(g) => {
                    self.append_group(g, out);
                    continue;
                }
                TokenTree::Ident(id) => (id.to_string(), false),
                TokenTree::Punct(p) => (p.as_char().to_string(), true),
                TokenTree::Literal(lit) => (lit.to_string(), false),
            };

            let bounds = span_bounds(tt.span());
            self.separate(out, bounds.map(|(start, _)| start), is_punct, false);
            out.push_str(&text);
            self.advance(bounds.map(|(_, end)| end), is_punct);
        }
    }

    /// Appends a delimited group, spacing the delimiters themselves from their
    /// surroundings the same way as any other token.
    fn append_group(&mut self, group: &Group, out: &mut String) {
        let (open, close) = match group.delimiter() {
            Delimiter::Brace => ("{", "}"),
            Delimiter::Bracket => ("[", "]"),
            Delimiter::Parenthesis => ("(", ")"),
            // An invisible delimiter writes nothing, so its contents simply
            // continue the surrounding run rather than interrupting it.
            Delimiter::None => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                self.append_tokens(&inner, out);
                return;
            }
        };

        let open_bounds = span_bounds(group.span_open());
        let close_bounds = span_bounds(group.span_close());

        self.separate(out, open_bounds.map(|(start, _)| start), false, true);
        out.push_str(open);
        // Within the group the opening character stands in for the previous
        // token; calling it punctuation keeps the fallback from opening with a
        // spurious space.
        self.advance(open_bounds.map(|(_, end)| end), true);

        let inner: Vec<TokenTree> = group.stream().into_iter().collect();
        self.append_tokens(&inner, out);

        self.separate(out, close_bounds.map(|(start, _)| start), true, false);
        out.push_str(close);
        // A group as a whole is not punctuation, which is how the fallback
        // heuristic has always spaced one from whatever follows it.
        self.advance(close_bounds.map(|(_, end)| end), false);
    }
}

/// Concatenates every token's text without any separators, recursing into
/// groups.
///
/// The result is the hash input, and it is deliberately whitespace-independent:
/// `bamboo-css-collector` re-derives the same string from the source text with
/// its own `normalize_for_hash`, and only ever sees whatever spacing the user
/// typed.  Spacing therefore belongs in `tokens_to_css` alone — routing it
/// through here would desynchronise the two and break dead-code elimination.
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

/// Fragment hashes this process has already written.
///
/// It is what tells a fragment left behind by an earlier build apart from a
/// second body claiming the same hash during *this* one.
static CLAIMED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Distinguishes the temporary files of concurrent writers within one process.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `target/styled-fragments/{hash}.css` under the workspace root.
///
/// This used to open with `create_new` and skip on `AlreadyExists`, reasoning
/// that the same hash implied the same content.  That no longer holds:
/// `tokens_to_hash_input` ignores whitespace while `tokens_to_css` now
/// preserves it, so two bodies differing only in spacing share a hash yet can
/// describe different CSS.  Skipping would then serve whichever body was
/// compiled first — and, across builds, would keep serving CSS from before an
/// edit that changed only spacing, since the file name never changes.
///
/// So the content decides.  Matching content is the ordinary case and costs
/// nothing; differing content is either a stale fragment, which is replaced, or
/// two live bodies under one class name, which cannot both be honoured and is
/// reported instead.
///
/// `CLAIMED` separates those two only within one proc-macro process, which is
/// one crate.  A pair of colliding bodies in *different* crates still reads as
/// staleness, and the later one wins.  Closing that gap would mean making the
/// hash itself spacing-aware, which would rename every class every consumer
/// already ships.
fn write_fragment(workspace_root: &Path, hash: &str, css: &str) -> Result<(), String> {
    let dir = workspace_root.join("target").join("styled-fragments");
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create fragments dir: {e}"))?;

    let path = dir.join(format!("{hash}.css"));
    // Claim before reading, so the check below sees this invocation too.  A
    // poisoned lock is treated as a first claim: replacing a fragment is the
    // recoverable outcome, refusing to compile is not.
    let first_claim = CLAIMED
        .lock()
        .map(|mut claimed| claimed.insert(hash.to_string()))
        .unwrap_or(true);

    match fs::read_to_string(&path) {
        // Already current.  Two identical bodies deduplicating onto one class
        // land here, which is the whole point of hashing the content.
        Ok(existing) if existing == css => return Ok(()),
        Ok(_) if !first_claim => {
            return Err(format!(
                "two css! bodies map to `{hash}` but produce different CSS, so \
                 they differ only in whitespace — which the class hash ignores \
                 by design. One class cannot carry both rules. Make the two \
                 bodies agree, or change one enough that they hash apart."
            ));
        }
        // Left over from an earlier build, from before an edit that changed
        // only spacing.  Replace it.
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("failed to read fragment: {e}")),
    }

    // Publish by rename so that macro invocations in parallel processes never
    // observe a half-written fragment — the property `create_new` used to
    // provide.
    let tmp = dir.join(format!(
        "{hash}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, css).map_err(|e| format!("failed to write fragment: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("failed to publish fragment: {e}")
    })?;

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
/// `attr:value`) can be passed via Leptos' standard `attr:*` syntax.
///
/// **All other elements** generate a component that accepts a `children:
/// Children` prop rendered inside the scoped element.  Arbitrary HTML
/// attributes (e.g. `attr:style="…"`) can likewise be passed via Leptos'
/// standard `attr:*` syntax.
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
/// // Void element — no children
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
        // attributes are passed via Leptos' standard `attr:*` syntax.
        quote! {
            #[::leptos::component]
            pub fn #component_ident() -> impl ::leptos::IntoView {
                use ::leptos::prelude::ClassAttribute;

                ::leptos::view! {
                    <#tag_ident class=#hash_lit/>
                }
            }
        }
        .into()
    } else {
        // Normal element: accepts children rendered inside the scoped element.
        // Arbitrary HTML attributes can be passed as `attr:*` props via
        // Leptos' standard attribute syntax.
        quote! {
            #[::leptos::component]
            pub fn #component_ident(
                children: ::leptos::children::Children,
            ) -> impl ::leptos::IntoView {
                use ::leptos::prelude::ClassAttribute;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenizes `body` and reconstructs the CSS text the macro hands to
    /// lightningcss.  Parsing from a string gives the tokens real span
    /// locations, so this exercises the same path as a macro invocation.
    fn emit(body: &str) -> String {
        let tokens: TokenStream2 = body.parse().expect("body should tokenize");
        tokens_to_css(tokens)
    }

    /// Runs `body` through the full pipeline, ending in minified CSS scoped to
    /// `.css-test`.
    fn minify(body: &str) -> String {
        process_css("css-test", &emit(body)).expect("CSS should be valid")
    }

    #[test]
    fn descendant_selector_keeps_its_space() {
        // `&img` is a compound selector — "is both the scope class and an img" —
        // and matches nothing.
        assert_eq!(emit("& img { color: red }"), "& img { color: red }");
        assert_eq!(minify("& img { color: red }"), ".css-test{& img{color:red}}");
        assert_eq!(minify("& svg { color: red }"), ".css-test{& svg{color:red}}");
    }

    #[test]
    fn child_combinator_stays_tight() {
        assert_eq!(minify("& > img { color: red }"), ".css-test{&>img{color:red}}");
    }

    #[test]
    fn pseudo_classes_do_not_gain_a_space() {
        // `& :hover` would select hovered descendants instead of the element.
        assert_eq!(minify("&:hover { color: red }"), ".css-test{&:hover{color:red}}");
        assert_eq!(
            minify("&:disabled { color: red }"),
            ".css-test{&:disabled{color:red}}"
        );
        assert_eq!(
            minify("&:last-child { color: red }"),
            ".css-test{&:last-child{color:red}}"
        );
    }

    #[test]
    fn calc_keeps_the_spaces_its_operators_need() {
        // `calc(100%+4px)` and `calc(100vh-340px)` are both invalid, and
        // lightningcss hands them straight through rather than complaining —
        // the browser is what drops the declaration, which is why losing these
        // spaces was silent all the way to the rendered page.
        assert_eq!(
            minify("top: calc(100% + 4px);"),
            ".css-test{top:calc(100% + 4px)}"
        );
        assert_eq!(
            minify("max-height: calc(100vh - 340px);"),
            ".css-test{max-height:calc(100vh - 340px)}"
        );
    }

    #[test]
    fn unary_minus_stays_glued_to_its_number() {
        // A space here would make it a subtraction with a missing left operand.
        assert_eq!(emit("transform: translateX(-4px);"), "transform: translateX(-4px);");
        // lightningcss rewrites `translateX` to the shorter `translate`.
        assert_eq!(
            minify("transform: translateX(-4px);"),
            ".css-test{transform:translate(-4px)}"
        );
    }

    #[test]
    fn multi_value_declarations_keep_their_separators() {
        assert_eq!(minify("margin: 0 auto;"), ".css-test{margin:0 auto}");
        assert_eq!(
            minify("grid-template-columns: 28px 1fr auto auto;"),
            ".css-test{grid-template-columns:28px 1fr auto auto}"
        );
        assert_eq!(
            minify("transition: background-color .15s, opacity .15s;"),
            ".css-test{transition:background-color .15s,opacity .15s}"
        );
    }

    #[test]
    fn spacing_never_reaches_the_hash() {
        // The collector re-derives hashes from source text whose spacing it
        // never inspects, so differently spaced bodies must hash alike.
        let spaced: TokenStream2 = "& img { color : red }".parse().unwrap();
        let tight: TokenStream2 = "&img{color:red}".parse().unwrap();
        assert_eq!(
            generate_hash(&tokens_to_hash_input(spaced)),
            generate_hash(&tokens_to_hash_input(tight)),
        );
    }

    /// A throwaway stand-in for a workspace root, placed under the real
    /// `target/` so the scratch files land in the build directory rather than
    /// in the system temp directory.
    ///
    /// Each test passes its own `name` *and* its own hash: `CLAIMED` is
    /// process-global and the harness runs tests in parallel, so sharing either
    /// would make them read each other's state.
    fn scratch_root(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the macro crate sits inside the workspace")
            .join("target")
            .join("fragment-tests")
            .join(name);
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn fragment_path(root: &Path, hash: &str) -> PathBuf {
        root.join("target")
            .join("styled-fragments")
            .join(format!("{hash}.css"))
    }

    #[test]
    fn an_unchanged_fragment_is_left_alone() {
        // Two identical bodies deduplicating onto one class.
        let root = scratch_root("unchanged");
        let hash = "css-testunchanged";
        write_fragment(&root, hash, ".a{color:red}").unwrap();
        write_fragment(&root, hash, ".a{color:red}").unwrap();
        assert_eq!(
            fs::read_to_string(fragment_path(&root, hash)).unwrap(),
            ".a{color:red}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fragment_left_by_an_earlier_build_is_replaced() {
        // The hash survives an edit that changes only spacing, so the file name
        // does not move and `create_new` would have served this forever.
        let root = scratch_root("stale");
        let hash = "css-teststale";
        let path = fragment_path(&root, hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, ".a{&img{width:28px}}").unwrap();

        write_fragment(&root, hash, ".a{& img{width:28px}}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), ".a{& img{width:28px}}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_live_bodies_under_one_hash_are_reported() {
        // `& img` and `&img` hash alike but no longer emit alike, and one class
        // cannot carry both rules.
        let root = scratch_root("collision");
        let hash = "css-testcollision";
        write_fragment(&root, hash, ".a{& img{width:28px}}").unwrap();
        let err = write_fragment(&root, hash, ".a{&img{width:28px}}")
            .expect_err("a second body under one hash must not pass silently");
        assert!(err.contains("differ only in whitespace"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unlocated_tokens_fall_back_to_the_token_kind_heuristic() {
        // `quote!` builds tokens with no source position, so spacing falls back
        // to the older rule: a space between two non-punctuation tokens.
        let synthetic = quote! { color: red; padding: 4px 8px; };
        assert_eq!(tokens_to_css(synthetic), "color:red;padding:4px 8px;");
    }
}
