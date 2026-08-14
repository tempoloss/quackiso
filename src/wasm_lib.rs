#![allow(special_module_name)]

#[path = "lib.rs"]
mod lib;

// Every module under lib.rs refers to its siblings as `crate::wire`, and here
// the crate root is this file, so the glob is what makes those paths resolve.
use lib::*;

// The Wasm target needs a `staticlib` crate-type, which cannot be selected
// per-target in Cargo.toml, so this example remaps lib.rs. The `#[path]` is
// load-bearing: without it lib.rs is a non-mod.rs module and its twenty
// sibling modules resolve under src/lib/, which does not exist. Do not add
// logic here. Build the Wasm target with: cargo build --example quackiso
