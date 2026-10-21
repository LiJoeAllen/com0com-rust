//! Bus global state and pair management.

use core::ptr;

use wdk_sys::{
    ntddk::{
        ExAllocatePool2, ExFreePool, KeAcquireSpinLockRaiseToDpc, KeInitializeSpinLock,
        KeReleaseSpinLock,
    },
    KIRQL, KSPIN_LOCK, WDFCHILDLIST, WDFDEVICE,
};

use com0com_common::{
    pair::{PairContext, DEFAULT_RNG_SEED, RING_BUFFER_SIZE, RING_CAPACITY},
    protocol::Com0comPairParams,
};

pub const MAX_PAIRS: usize = 64;
pub const COM0COM_TAG: u32 = u32::from_le_bytes(*b"c0c0"); // pool tag

/// WDF child identification description. Carries (pair_id, port_index) so the
/// PDO create callback knows which port to spawn.
///
/// KMDF compares the whole `IdentificationDescriptionSize` block with
/// `RtlCompareMemory` (no compare callback is registered), so every byte —
/// including the 3 bytes of tail padding — must be deterministic. Always build
/// this with `core::mem::zeroed()` instead of a struct literal, otherwise the
/// uninitialized padding makes removal lookups fail with STATUS_NO_SUCH_DEVICE.
#[repr(C)]
pub struct Com0comChildId {
    pub header: wdk_sys::WDF_CHILD_IDENTIFICATION_DESCRIPTION_HEADER,
    pub pair_id: u32,
    pub port_index: u8,
}

/// Per-pair slot kept in the bus. The `PairContext` itself lives in the slot's
/// `pair_va`; the slot tracks enumeration + link state.
#[repr(C)]
pub struct PairSlot {
    pub pair_id: u32,
    pub pair_va: u64, // *mut PairContext
    pub params: Com0comPairParams,
    pub child_present: [bool; 2], // A, B currently present in the child list
    pub deleted: bool,            // DELETE_PAIR seen: hidden from ENUM, rejects new links
    pub ref_count: i32,           // ports currently linked via PORT_LINK
}

/// Singleton bus state. The bus is a single device, so a runtime-allocated
/// pointer (set in `evt_driver_device_add`) is sufficient and avoids WDF
/// context-type plumbing.
pub struct BusState {
    pub fdo: WDFDEVICE,
    pub child_list: WDFCHILDLIST,
    pub lock: KSPIN_LOCK,
    pub slots: [*mut PairSlot; MAX_PAIRS],
    pub next_pair_id: u32,
}

static mut BUS: *mut BusState = ptr::null_mut();

/// Publish the singleton bus state.
///
/// # Safety
/// `state` must come from [`alloc_bus_state`], and this must run once during
/// device add before any other bus entry point is reachable.
pub unsafe fn set_bus(state: *mut BusState) {
    BUS = state;
}

/// Borrow the singleton bus state.
///
/// # Safety
/// The returned reference aliases the singleton; callers must serialize access
/// to the fields it guards through the bus spinlock.
pub unsafe fn bus() -> Option<&'static mut BusState> {
    BUS.as_mut()
}

/// # Safety
/// `lock` must point to an initialized `KSPIN_LOCK`.
#[inline]
pub unsafe fn raw_lock(lock: *mut KSPIN_LOCK) -> KIRQL {
    KeAcquireSpinLockRaiseToDpc(lock)
}

/// # Safety
/// `lock` must be the lock acquired by [`raw_lock`], released at its `irql`.
#[inline]
pub unsafe fn raw_unlock(lock: *mut KSPIN_LOCK, irql: KIRQL) {
    KeReleaseSpinLock(lock, irql);
}

/// Build a `wdk_sys::GUID` from little-endian GUID bytes.
pub fn guid_from_bytes(b: &[u8; 16]) -> wdk_sys::GUID {
    wdk_sys::GUID {
        Data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        Data2: u16::from_le_bytes([b[4], b[5]]),
        Data3: u16::from_le_bytes([b[6], b[7]]),
        Data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    }
}

pub const GUID_COM0COM_BUS_WDF: () = (); // placeholder; use guid_from_bytes at call sites

/// Find a free slot index, or `None` if at capacity. Caller must hold the lock.
///
/// # Safety
/// `state` must be the live singleton and the bus spinlock must be held.
pub unsafe fn alloc_slot(state: &mut BusState) -> Option<usize> {
    state.slots.iter().position(|slot| slot.is_null())
}

/// Allocate and zero a `PairSlot` in non-paged pool.
///
/// # Safety
/// Must be called at IRQL <= DISPATCH_LEVEL; the returned pointer is freed only
/// by [`free_pair_slot`].
pub unsafe fn alloc_pair_slot() -> *mut PairSlot {
    let size = core::mem::size_of::<PairSlot>() as u64;
    // WDK 10.0.28000 bindings expose POOL_FLAG_NON_PAGED at the crate root;
    // ExAllocatePool2 itself is generated in the ntddk module.
    let ptr = ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::write_bytes(ptr, 0u8, size as usize);
    ptr as *mut PairSlot
}

/// # Safety
/// `slot` must come from [`alloc_pair_slot`] and must not be used afterwards.
pub unsafe fn free_pair_slot(slot: *mut PairSlot) {
    if !slot.is_null() {
        ExFreePool(slot as wdk_sys::PVOID);
    }
}

/// Allocate and initialize a `PairContext` in non-paged pool.
///
/// # Safety
/// Must be called at IRQL <= DISPATCH_LEVEL; the resulting VA is shared with
/// the port drivers and freed only by [`free_pair_context`].
pub unsafe fn alloc_pair_context(pair_id: u32) -> *mut PairContext {
    let size = core::mem::size_of::<PairContext>() as u64;
    let ptr = ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::write_bytes(ptr, 0u8, size as usize);
    let pc = &mut *(ptr as *mut PairContext);
    pc.magic = com0com_common::pair::COM0COM_MAGIC;
    pc.pair_id = pair_id;
    pc.ref_count = 0;
    pc.deleting = 0;
    pc.params = Com0comPairParams::new();
    // 就地补上非零默认值；绝不能再写 `pc.port[0] = PortState::new()`。
    // 每个 PortState 约 16 KiB（内含一个 RingBuffer），赋值语句会先在栈上
    // 构造 16 KiB 的临时值。本函数在 DISPATCH_LEVEL 的内核栈上执行
    // （KMDF 设备锁把 IOCTL 回调抬到 IRQL 2），v8 的 0x0A 崩溃正是
    // `_chkstk` 在 `alloc_pair_context` 里探测新栈页。
    // 清零已由上面的 write_bytes 完成，这里只写非零字段：
    // 零值会让写路径一个字节都收不下（queue_limit == 0）且 xorshift32 退化。
    for port in pc.port.iter_mut() {
        port.rx_buffer_size = RING_BUFFER_SIZE as u32;
        port.tx_buffer_size = RING_BUFFER_SIZE as u32;
        port.queue_limit = RING_CAPACITY as u32;
        port.rng_state = DEFAULT_RNG_SEED;
    }
    KeInitializeSpinLock(&mut pc.lock as *mut u64 as *mut KSPIN_LOCK);
    ptr as *mut PairContext
}

/// # Safety
/// `pc` must come from [`alloc_pair_context`], have a zero ref count, and no
/// port may hold its VA any more.
pub unsafe fn free_pair_context(pc: *mut PairContext) {
    if !pc.is_null() {
        ExFreePool(pc as wdk_sys::PVOID);
    }
}

/// Allocate and initialize the singleton bus state in non-paged pool.
///
/// # Safety
/// `fdo` and `child_list` must be live WDF handles owned by this driver.
pub unsafe fn alloc_bus_state(fdo: WDFDEVICE, child_list: WDFCHILDLIST) -> *mut BusState {
    let size = core::mem::size_of::<BusState>() as u64;
    let ptr = ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::write_bytes(ptr, 0u8, size as usize);
    let state = &mut *(ptr as *mut BusState);
    state.fdo = fdo;
    state.child_list = child_list;
    state.slots = [ptr::null_mut(); MAX_PAIRS];
    state.next_pair_id = 1;
    KeInitializeSpinLock(&mut state.lock);
    ptr as *mut BusState
}

/// Locate the slot for a pair id under the lock. Returns the slot pointer.
///
/// # Safety
/// The bus spinlock must be held so the returned slot cannot be freed.
pub unsafe fn find_slot(state: &BusState, pair_id: u32) -> Option<*mut PairSlot> {
    state
        .slots
        .iter()
        .copied()
        .find(|&slot| !slot.is_null() && (*slot).pair_id == pair_id)
}

/// Convenience: convert a kernel VA back to a `&mut PairContext`.
///
/// # Safety
/// The pointer must be a valid, non-freed `PairContext` VA obtained from
/// `PORT_LINK`.
pub unsafe fn pair_context_from_va(va: u64) -> Option<&'static mut PairContext> {
    if va == 0 {
        return None;
    }
    (va as *mut PairContext).as_mut()
}
