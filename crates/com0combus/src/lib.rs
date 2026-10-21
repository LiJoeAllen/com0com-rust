//! com0combus — KMDF bus driver for the Rust com0com rewrite.
//!
//! Responsibilities:
//!   * Create the bus FDO and a WDF static child list.
//!   * Own the per-pair `PairContext` (allocated in non-paged pool) and
//!     reference-count its lifetime across the two port drivers.
//!   * Expose a control device interface (`GUID_COM0COM_BUS`) consumed by the
//!     user-mode `setupc` tool and by the port driver (for PORT_LINK/UNLINK).
//!   * Handle the com0com private IOCTLs.
//!
//! The port *function* driver (`com0comport`) is a separate WDM binary.

#![no_std]

extern crate wdk_panic; // provides the kernel panic handler

/// Kernel debug output (captured by DebugView / a kernel debugger).
/// 只能在 unsafe 上下文中调用（DbgPrint 是外部 C 函数）。
macro_rules! klog {
    ($s:literal $(, $a:expr)* $(,)?) => {
        wdk_sys::ntddk::DbgPrint(concat!($s, "\n\0").as_ptr().cast() $(, $a)*)
    };
}

use wdk_alloc::WdkAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

pub mod bus;
pub mod driver;
pub mod ioctl;
pub mod pdo;
