# bamboo-css

A CSS-in-Rust library with **zero runtime overhead**. Write scoped styles directly alongside your Rust components — CSS is extracted at compile time, bundled by a CLI hook, and served as a static file.

Inspired by CSS-in-JS solutions like [emotion](https://emotion.sh/).

## Overview

```rust
let class = css! {
    background-color: red;
    width: 50%;
    margin-left: 4rem;
    display: flex;

    &:hover {
        background-color: blue;
    }
};
// class == "css-a1b2c3d4"
```

The `css!` macro:
- Validates CSS at compile time using [lightningcss](https://lightningcss.dev/)
- Resolves nesting, applies vendor prefixes, and minifies
- Scopes styles under an auto-generated hash class (e.g. `.css-a1b2c3d4`)
- Writes a CSS fragment to `target/styled-fragments/` — no runtime injection

`bamboo-css-collector` is a CLI tool that runs as a Trunk `pre_build` hook. It scans your source tree, eliminates dead CSS (fragments from deleted `css!` calls), and writes a single `bundle.css` for Trunk to pick up.

## Installation

### 1. Add the macro crate to your app

```toml
# Cargo.toml
[dependencies]
bamboo-css-macro = { git = "https://github.com/Ign1s-Reiga/bamboo-css" }
```

### 2. Install the collector

```sh
cargo install bamboo-css-collector --git https://github.com/Ign1s-Reiga/bamboo-css
```

Or clone the repository:

```bash
git clone https://github.com/Ign1s-Reiga/bamboo-css.git
```

### 3. Configure Trunk

Add the collector as a `pre_build` hook and reference the bundle in your `index.html`.

```toml
# Trunk.toml
[watch]
ignore = ["assets/bundle.css"]

[[hooks]]
stage = "pre_build"
command = "bamboo-css-collector"
command_arguments = []

# or

[[hooks]]
stage = "pre_build"
command = "cargo"
command_arguments = [
    "run",
    "--manifest-path", "../../bamboo-css/bamboo-css-collector/Cargo.toml",
    "--quiet",
]
```

```html
<!-- index.html -->
<link data-trunk rel="css" href="assets/bundle.css" />
```

The output path defaults to `assets/bundle.css` and can be overridden with the `BAMBOO_CSS_DIST` environment variable or the `--out` flag.

### 4. Use the macro

```rust
use bamboo_css_macro::css;

#[component]
fn MyButton() -> impl IntoView {
    let class = css! {
        padding: 0.5rem 1rem;
        border-radius: 4px;
        background-color: royalblue;
        color: white;

        &:hover {
            background-color: steelblue;
        }
    };

    view! { <button class=class>"Click me"</button> }
}
```

## Collector CLI options

```
bamboo-css-collector [OPTIONS]

Options:
  -s, --src <DIR>            Source directory to scan          [default: src]
  -f, --fragments <DIR>      CSS fragments directory           [default: target/styled-fragments]
  -o, --out <FILE>           Output bundle path                [default: assets/bundle.css]
                             [env: BAMBOO_CSS_DIST]
  -r, --project-root <DIR>   Base for all relative paths       [default: .]
```

## How it works

1. **Macro** — `css!` tokenizes the input, generates a content-based hash, validates and processes the CSS through lightningcss, then writes `target/styled-fragments/{hash}.css`. The macro expands to the hash string `"css-{hash}"`.
2. **Collector** — Before each Trunk build, the collector scans `src/` for all `css!` invocations, re-derives each hash, and bundles only the corresponding fragment files. Fragments from deleted `css!` calls are excluded (dead-code elimination) without removing any files from `target/`.
3. **Trunk** — Detects `<link data-trunk rel="css">` in `index.html`, fingerprints and copies the bundle to `dist/`.

## License

This project is licensed under the **MIT License**.  
See the [LICENSE](LICENSE) file for details.

*Note: This project depends on [lightningcss](https://github.com/parcel-bundler/lightningcss), which is licensed under the MPL-2.0.*
