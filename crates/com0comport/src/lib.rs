//! com0comport — WDM port function driver for the Rust com0com rewrite.
//!
//! Loaded on each virtual port PDO. Handles the `IOCTL_SERIAL_*` surface so
//! `CreateFile("COMx")` + the Win32 Comm API work, and moves data between the
//! two ports of a pair through the shared `PairContext` owned by the bus.

#![no_std]

extern crate wdk_panic; // kernel panic handler

use wdk_alloc::WdkAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

pub mod comname;
pub mod dispatch;
pub mod driver;
pub mod readwrite;
pub mod serial;
