use proc_macro::TokenStream;
use proc_macro2::{Delimiter, LineColumn, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Reconstructs a CSS string from a TokenStream by using source span positions to decide whether a space should be inserted between adjacent tokens.
fn tokens_to_css(input: TokenStream2) -> String {
    let tokens: Vec<TokenTree> = input.into_iter().collect();
    let mut out = String::new();
    append_tokens(&tokens, &mut out);
    out
}

fn append_tokens(tokens: &[TokenTree], out: &mut String) {
    let mut prev_end: Option<LineColumn> = None;

    for tt in tokens {
        // The "start" of a group is its opening delimiter, not the whole span.
        let start = match tt {
            TokenTree::Group(g) => g.span_open().start(),
            _ => tt.span().start(),
        };

        // Insert a space whenever tokens are not directly adjacent in source.
        if let Some(end) = prev_end {
            if end != start {
                out.push(' ');
            }
        }

        let end = match tt {
            TokenTree::Group(g) => {
                let (open, close) = match g.delimiter() {
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                append_tokens(&inner, out);
                out.push_str(close);
                g.span_close().end()
            }
            TokenTree::Ident(id) => {
                out.push_str(&id.to_string());
                tt.span().end()
            }
            TokenTree::Punct(p) => {
                out.push(p.as_char());
                tt.span().end()
            }
            TokenTree::Literal(lit) => {
                out.push_str(&lit.to_string());
                tt.span().end()
            }
        };

        prev_end = Some(end);
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

/// Wraps the user's CSS body in a scoped selector, then runs it through
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
