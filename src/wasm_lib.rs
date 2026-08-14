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
// logic here.
//
// `cargo build --example quackiso` builds it for the host, which is all CI
// needs to keep it compiling. The Wasm artifact is `make wasm_mvp`: cargo
// --release --target wasm32-unknown-emscripten --example quackiso for
// libquackiso.a, then emcc -sSIDE_MODULE=2
// -sEXPORTED_FUNCTIONS=_quackiso_init_c_api against it, with emsdk 3.1.71,
// the version extension-ci-tools pins.
//
// 3.1.71 gets as far as -O1. emcc reads the target features off the archive
// and hands them to wasm-opt, and rustc 1.97 emits two that binaryen 120
// does not know, so the -O2 and -O3 pass dies on
// --enable-bulk-memory-opt. wasm-ld is not the problem. 3.1.74 is the first
// emsdk that takes -O3.
