//! Bus global state and pair management.

use core::ptr;

use wdk_sys::{
    KeAcquireSpinLock, KeInitializeSpinLock, KeReleaseSpinLock, KIRQL, KSPIN_LOCK, NTSTATUS,
    PCUNICODE_STRING, WDFCHILDLIST, WDFDEVICE, WDFDRIVER, WDF_DEVICE_INIT,
};

use com0com_common::{
    constants::{GUID_COM0COM_BUS_BYTES, GUID_DEVINTERFACE_COMPORT},
    pair::PairContext,
    protocol::Com0comPairParams,
};

pub const MAX_PAIRS: usize = 64;
pub const COM0COM_TAG: u32 = u32::from_le_bytes(*b"c0c0"); // pool tag

/// WDF child identification description. Carries (pair_id, port_index) so the
/// PDO create callback knows which port to spawn.
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

pub unsafe fn set_bus(state: *mut BusState) {
    BUS = state;
}

pub unsafe fn bus() -> Option<&'static mut BusState> {
    BUS.as_mut()
}

#[inline]
pub unsafe fn raw_lock(lock: *mut KSPIN_LOCK) -> KIRQL {
    let mut irql: KIRQL = 0;
    KeAcquireSpinLock(lock, &mut irql);
    irql
}

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
pub unsafe fn alloc_slot(state: &mut BusState) -> Option<usize> {
    for i in 0..MAX_PAIRS {
        if state.slots[i].is_null() {
            return Some(i);
        }
    }
    None
}

/// Allocate and zero a `PairSlot` in non-paged pool.
pub unsafe fn alloc_pair_slot() -> *mut PairSlot {
    let size = core::mem::size_of::<PairSlot>() as u64;
    // VERIFY: POOL_FLAG_NON_PAGED value against your installed WDK headers.
    let ptr = wdk_sys::ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::write_bytes(ptr, 0u8, size as usize);
    ptr as *mut PairSlot
}

pub unsafe fn free_pair_slot(slot: *mut PairSlot) {
    if !slot.is_null() {
        wdk_sys::ExFreePool(slot as wdk_sys::PVOID);
    }
}

/// Allocate and initialize a `PairContext` in non-paged pool.
pub unsafe fn alloc_pair_context(pair_id: u32) -> *mut PairContext {
    let size = core::mem::size_of::<PairContext>() as u64;
    let ptr = wdk_sys::ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
    if ptr.is_null() {
        return ptr::null_mut();
    }
    ptr::write_bytes(ptr, 0u8, size as usize);
    let pc = &mut *(ptr as *mut PairContext);
    pc.magic = com0com_common::pair::COM0COM_MAGIC;
    pc.pair_id = pair_id;
    pc.ref_count = 0;
    KeInitializeSpinLock(&mut pc.lock as *mut u64 as *mut KSPIN_LOCK);
    ptr as *mut PairContext
}

pub unsafe fn free_pair_context(pc: *mut PairContext) {
    if !pc.is_null() {
        wdk_sys::ExFreePool(pc as wdk_sys::PVOID);
    }
}

/// Allocate and initialize the singleton bus state in non-paged pool.
pub unsafe fn alloc_bus_state(fdo: WDFDEVICE, child_list: WDFCHILDLIST) -> *mut BusState {
    let size = core::mem::size_of::<BusState>() as u64;
    let ptr = wdk_sys::ExAllocatePool2(wdk_sys::POOL_FLAG_NON_PAGED, size, COM0COM_TAG);
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
pub unsafe fn find_slot(state: &BusState, pair_id: u32) -> Option<*mut PairSlot> {
    for i in 0..MAX_PAIRS {
        let slot = state.slots[i];
        if !slot.is_null() && (*slot).pair_id == pair_id {
            return Some(slot);
        }
    }
    None
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
