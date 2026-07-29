#![allow(special_module_name)]

mod lib;

// The Wasm target needs a `staticlib` crate-type, which cannot be selected
// per-target in Cargo.toml, so this example remaps lib.rs. Do not add logic
// here. Build the Wasm target with: cargo build --example quackiso
