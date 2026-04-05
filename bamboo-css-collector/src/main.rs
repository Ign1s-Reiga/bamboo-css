use clap::Parser;
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Source directory to scan for css! invocations (relative to --project-root)
    #[arg(short, long, default_value = "src")]
    src: PathBuf,

    /// Directory containing CSS fragment files (relative to --project-root)
    #[arg(short, long, default_value = "target/styled-fragments")]
    fragments: PathBuf,

    /// Output bundle path [env: BAMBOO_CSS_DIST] (relative to --project-root)
    #[arg(short, long, env = "BAMBOO_CSS_DIST", default_value = "assets/bundle.css")]
    out: PathBuf,

    /// Project root; all other relative paths are resolved against this
    #[arg(short = 'r', long, default_value = ".")]
    project_root: PathBuf,
}

impl Args {
    fn abs(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.project_root.join(p)
        }
    }
}

fn generate_hash(normalized: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    normalized.hash(&mut h);
    format!("css-{:08x}", h.finish() as u32)
}

/// Returns the byte offset of the matching closing `}` in `source[start..]`,
/// where `start` is the byte offset just *after* an already-matched opening `{`.
fn find_brace_end(source: &str, start: usize) -> Option<usize> {
    let rest = &source[start..];
    let mut depth: usize = 1;
    for (byte_pos, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + byte_pos);
                }
            }
            _ => {}
        }
    }
    None
}

/// Returns the raw text content of every `css! { … }` and `styled!(…, { … })`
/// block found in `source` (the part between the outer braces, not including
/// the braces themselves).
fn extract_css_blocks(source: &str) -> Vec<&str> {
    let css_re = Regex::new(r"css!\s*\{").unwrap();
    let styled_re = Regex::new(r"styled!\s*\(").unwrap();
    let mut blocks = Vec::new();

    // --- css! { … } ---
    for mat in css_re.find_iter(source) {
        let inner_start = mat.end();
        if let Some(end) = find_brace_end(source, inner_start) {
            blocks.push(&source[inner_start..end]);
        }
    }

    // --- styled!(tag, { … }) ---
    for mat in styled_re.find_iter(source) {
        // `mat.end()` is just after the `(`.  We need to find the CSS brace
        // block, which comes after the tag token and a comma.  Walk through
        // the paren contents character-by-character looking for the first `{`
        // that is at paren-depth 0 (i.e. not nested inside sub-parens).
        let after_paren = mat.end();
        let rest = &source[after_paren..];
        let mut paren_depth: usize = 0;
        let mut css_brace_start: Option<usize> = None;

        for (byte_pos, ch) in rest.char_indices() {
            match ch {
                '(' => paren_depth += 1,
                ')' => {
                    if paren_depth == 0 {
                        // Closed the outer paren — no CSS brace found.
                        break;
                    }
                    paren_depth -= 1;
                }
                '{' if paren_depth == 0 => {
                    css_brace_start = Some(after_paren + byte_pos + 1); // just after `{`
                    break;
                }
                _ => {}
            }
        }

        if let Some(inner_start) = css_brace_start {
            if let Some(end) = find_brace_end(source, inner_start) {
                blocks.push(&source[inner_start..end]);
            }
        }
    }

    blocks
}

/// Concatenates every token's text without separators, recursing into groups.
fn tokens_to_hash_input(tokens: proc_macro2::TokenStream) -> String {
    use proc_macro2::{Delimiter, TokenTree};
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

/// Parses `css_text` and returns the spacing-independent token concatenation used as input to `generate_hash`
fn normalize_for_hash(css_text: &str) -> String {
    match css_text.parse::<proc_macro2::TokenStream>() {
        Ok(ts) => tokens_to_hash_input(ts),
        Err(_) => css_text.chars().filter(|c| !c.is_whitespace()).collect(),
    }
}

/// Scans `src_dir` for `.rs` files and returns the set of hashes for all css! invocations currently found in the source tree (live hashes).
fn collect_live_hashes(src_dir: &Path) -> HashSet<String> {
    let mut live = HashSet::new();

    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().map(|x| x == "rs").unwrap_or(false)
        })
    {
        let source = match fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "bamboo-css-collector: warning: could not read {}: {e}",
                    entry.path().display()
                );
                continue;
            }
        };

        for block in extract_css_blocks(&source) {
            let normalized = normalize_for_hash(block);
            let hash = generate_hash(normalized.as_str());
            live.insert(hash);
        }
    }

    live
}

/// Reads all `*.css` fragment files from `fragments_dir`, keeps only those
/// whose stem (= hash) is in `live_hashes`, and concatenates them in
/// deterministic (alphabetical) order.
fn bundle_fragments(fragments_dir: &Path, live_hashes: &HashSet<String>) -> String {
    if !fragments_dir.exists() {
        return String::new();
    }

    let mut entries: Vec<_> = match fs::read_dir(fragments_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().map(|x| x == "css").unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            eprintln!(
                "bamboo-css-collector: warning: could not read fragments dir {}: {e}",
                fragments_dir.display()
            );
            return String::new();
        }
    };

    entries.sort_by_key(|e| e.path());

    let mut bundle = String::new();
    let mut included = 0usize;

    for entry in &entries {
        let path = entry.path();
        let hash = match path.file_stem().and_then(|s| s.to_str()) {
            Some(h) => h.to_owned(),
            None => continue,
        };

        if !live_hashes.contains(&hash) {
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(css) => {
                bundle.push_str(&css);
                included += 1;
            }
            Err(e) => {
                eprintln!(
                    "bamboo-css-collector: warning: could not read {}: {e}",
                    path.display()
                );
            }
        }
    }

    eprintln!(
        "bamboo-css-collector: {included}/{} fragment(s) included (DCE: {} eliminated)",
        entries.len(),
        entries.len().saturating_sub(included),
    );

    bundle
}

fn main() {
    let args = Args::parse();

    let src_dir = args.abs(&args.src);
    let fragments_dir = args.abs(&args.fragments);
    let out_path = args.abs(&args.out);

    let live = collect_live_hashes(&src_dir);

    let bundle = bundle_fragments(&fragments_dir, &live);

    if let Some(parent) = out_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("bamboo-css-collector: error: could not create output directory: {e}");
            std::process::exit(1);
        }
    }

    if let Err(e) = fs::write(&out_path, &bundle) {
        eprintln!("bamboo-css-collector: error: could not write bundle: {e}");
        std::process::exit(1);
    }

    eprintln!(
        "bamboo-css-collector: wrote {} byte(s) → {}",
        bundle.len(),
        out_path.display()
    );
}
