//! Shared definitions for the Rust com0com rewrite.
//!
//! This crate is `#![no_std]` and free of any `wdk`/`windows` dependency so that
//! the exact same type/layout definitions can be linked into:
//!   * `com0combus`  — the KMDF bus driver (kernel mode)
//!   * `com0comport` — the WDM port function driver (kernel mode)
//!   * `setupc`      — the user-mode configuration tool
//!
//! Keeping it dependency-free also lets the ring-buffer logic be unit-tested on
//! the host (plain `cargo test`) with zero Windows headers.

#![no_std]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

pub mod constants;
pub mod guid;
pub mod ioctls;
pub mod naming;
pub mod pair;
pub mod protocol;
pub mod serial_structs;
