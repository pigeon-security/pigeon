//! The pigeon harness. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("harness builds for wasm32 only: cargo build --target wasm32-unknown-unknown");
