# CLAUDE.md

## Project Overview

A general-purpose CSS-in-Rust library. When integrated into a user's project, it provides compile-time CSS extraction and automatic bundling via Trunk.

### Core Concepts

* **Zero Runtime Overhead:** CSS parsing and injection are omitted at runtime; everything is finalized during the build process.
* **Scoping:** Styles within `css!` are encapsulated in automatically generated hash classes (e.g., `.css-a1b2c3`).
* **Compile-time Validation:** CSS validity is validated during macro deployment using `lightningcss`.
* **Trunk Integration:** Distributed CSS fragments are aggregated and optimized using the `pre_build` hook.

## Tech Stack

A suite of key tools that support project reliability and performance.

* Core Macro: `proc-macro2`, `syn`, `quote` (Rust's standard macro development set)
* CSS Engine: `lightningcss`
  * Used for nesting resolution, vendor prefixing, minification, and syntax checking.
* Hash Function: `std::collections::hash_map::DefaultHasher`
  * Generates a deterministic hash (Content-based Hash) for incremental builds.
* Collector CLI: `clap` (argument parsing), `walkdir` (source scanning), `regex` (for hash extraction)
* Build Tool: `Trunk` (v0.17.0+ recommended, Hook functionality required)

## Project Structure

```
# --- Library Side (Your Crate) ---
bamboo-css/
├── bamboo-css-macro/      # CSS macros that users use in their code.
│   └── src/lib.rs         # Generate Hash + Fragment output to target/
└── bamboo-css-collector/  # Binary that user calls in Trunk hook
    └── src/main.rs        # Scan source code + Concatenate logic

# --- User Side (App Project) ---
user-app/
├── Cargo.toml             # Depends on bamboo-css-macro
├── Trunk.toml             # Register bamboo-css-collector in the pre_build hook.
├── src/                   # User's Rust code
└── assets/                # Output path of bundled CSS
```

## What is `css!` macro

`css!` is a key component of this CSS-in-Rust library, which was inspired by CSS-in-JS (e.g., emotion).
The basic structure of this macro is as follows:

```rust
fn example() {
   let style = css! {
      background-color: red;
      width: 50%;
      margin-left: 4rem;
      display: flex;
      &:hover {
         background-color: blue;
      }
   };
}
```

## Build Pipeline & Logic

### 1. Macro Execution (`bamboo-css-macro`)

* The `css!` macro receives a TokenStream.
* Normalize the contents and generate a hash (e.g., `css-5f3e2`).
* Replace the `&` selector with a hash class.
* Create `target/styled-fragments/{hash}.css` if it doesn't exist.
* The macro returns "{hash}" (string) as the expanded result.

### 2. Trunk Hook (`bamboo-css-collector`)

* Launched in the `pre_build` stage.
* Scans `src/` and identifies the hashes currently referenced in the code (DCE).
* Combines only the relevant fragments and outputs `assets/bundle.css`.

### 3. Final Asset Handling

* Trunk detects `<link data-trunk rel="css" href="assets/static/bundle.css">` in index.html.
* It then adds a hash, performs optimization, and deploys the final file to `dist/`.

## Important Specifications

### The Artifact Path

The library needs to identify the `target/` directory in the user's project.
* **Method:** Find out the root path of workspace with `cargo_metadata` crate when macro runs.

### Distribution Model of CLI tool

It needs to make it easy for users to call the collector from `Trunk.toml`.
* **Recommended:** You can either have the user run `cargo install bamboo-css-collector`, or add it to the project's `[dev-dependencies]` and run it with `cargo run --bin bamboo-css-collector`.

## Precautions

* **Namespace Collision:** It would be kind for users to be able to change prefix of auto generated-classname in settings.
* **Crate Versioning:** If happens of version mismatch between bamboo-css-macro and bamboo-css-collector, DCE (Dead Code Elimination) may fails by difference of hash deriving algorithm. Therefore, These are recommend to manage in same repository.
* **Parallel Compilation Race:** Cargo executes macros in parallel. Writing to `target/styled-fragments/` is done using the format "filename = hash of content", ensuring that overwriting is not a problem if the content is the same (using `OpenOptions::create_new` makes it even safer).
* **Incremental Build Ghosting:** Rust does not re-execute `proc-macro` for files that have not been changed. Therefore, `bamboo-css-collector` needs to filter and combine only the hashes that "currently exist in the source code" without deleting any files in `target/`.
* **Trunk Watch Loop:** To prevent an infinite loop (file update → Trunk detection → build → file update...) from occurring when `bamboo-css-collector` outputs files to Trunk's monitored directories (`assets/`, etc.), configure the output destination appropriately.

## Do & Don't

### ⭕ Do

* **Provide a `Plugin` helper:** Provide a helper or documents to be able to configure Trunk's `pre_build` command.
* **Support Environment Variable:** Make output path customizable with environment variable (e.g. BAMBOO_CSS_DIST)
* **Graceful Failure:** When `lightningcss` return error, shouldn't be panicked but show error on user's IDE using `compile_error!` macro.
* **Atomic CSS Fragments:** One file (hash name) is generated for each macro call, localizing management.

### ❌ Don't

* **Don't hardcode paths:** Do not use absolute paths like `/target/...` or OS-dependent path separators.
* **Don't touch user source code:** `bamboo-css-collector` reads the source code, but it should never perform any destructive operations.
* **Don't Use Runtime Injection:** Do not dynamically add `<style>` tags via the JS bridge. This can cause performance degradation and hydration mismatch.

## Docs

Only access the site and read it when necessary.

* lightningcss: https://docs.rs/lightningcss/latest/lightningcss/
* proc-macro2: https://docs.rs/proc-macro2/latest/proc_macro2/
* quote: https://docs.rs/quote/latest/quote/
* syn: https://docs.rs/syn/latest/syn/
* walkdir: https://docs.rs/walkdir/latest/walkdir/
* clap: https://docs.rs/walkdir/latest/walkdir/
