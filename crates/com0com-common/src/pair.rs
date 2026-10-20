//! Shared per-pair state that lives in the bus driver's non-paged pool and is
//! referenced directly (as a kernel VA) by both port function drivers.
//!
//! Because all drivers run in the same kernel address space, passing the
//! `PairContext` pointer across the bus<->port boundary is valid. Lifetime is
//! governed by `ref_count` (incremented on PORT_LINK, decremented on
//! PORT_UNLINK); the bus frees the structure only when `ref_count == 0`.
//!
//! Synchronization: `lock` is a `KSPIN_LOCK` (8 bytes on x64). The kernel
//! consumers cast `&pair.lock as *mut u64 as *mut KSPIN_LOCK` and call
//! `KeInitializeSpinLock` / `KeAcquireSpinLock` / `KeReleaseSpinLock`. The
//! ring-buffer helpers here are pure and assume the caller already holds the
//! lock.

use crate::serial_structs::SERIALPERF_STATS;

pub const COM0COM_MAGIC: u32 = 0x434f_4d43; // "COMC"
pub const RING_BUFFER_SIZE: usize = 16 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RingBuffer {
    pub data: [u8; RING_BUFFER_SIZE],
    pub head: u32, // next byte to write
    pub tail: u32, // next byte to read
}

impl RingBuffer {
    pub const fn new() -> Self {
        RingBuffer {
            data: [0u8; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    fn mask(idx: u32) -> usize {
        (idx as usize) % RING_BUFFER_SIZE
    }

    /// Bytes currently available to read.
    pub fn available_to_read(&self) -> usize {
        let head = self.head as usize % RING_BUFFER_SIZE;
        let tail = self.tail as usize % RING_BUFFER_SIZE;
        if head >= tail {
            head - tail
        } else {
            RING_BUFFER_SIZE - tail + head
        }
    }

    /// Free space available to write.
    pub fn available_to_write(&self) -> usize {
        RING_BUFFER_SIZE - 1 - self.available_to_read()
    }

    /// Copy as many bytes as fit from `src` into the buffer. Returns the count
    /// actually written.
    pub fn write(&mut self, src: &[u8]) -> usize {
        let free = self.available_to_write();
        let n = free.min(src.len());
        for i in 0..n {
            let pos = Self::mask(self.head);
            self.data[pos] = src[i];
            self.head = (self.head + 1) % RING_BUFFER_SIZE as u32;
        }
        n
    }

    /// Copy up to `dst.len()` bytes out of the buffer. Returns the count read.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let avail = self.available_to_read();
        let n = avail.min(dst.len());
        for i in 0..n {
            let pos = Self::mask(self.tail);
            dst[i] = self.data[pos];
            self.tail = (self.tail + 1) % RING_BUFFER_SIZE as u32;
        }
        n
    }

    pub fn is_empty(&self) -> bool {
        self.available_to_read() == 0
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PortState {
    pub rx: RingBuffer,
    /// Kernel VA of a pended `IRP_MJ_READ` (0 = none). The peer writer
    /// completes it directly when data arrives.
    pub pending_read_irp: u64,
    /// Kernel VA of a pended `IOCTL_SERIAL_WAIT_ON_MASK` IRP (0 = none).
    pub pending_event_irp: u64,
    pub wait_mask: u32,
    /// Events accumulated since the last WAIT_ON_MASK was satisfied.
    pub pending_events: u32,
    pub baud: u32,
    pub line_control: [u8; 3], // StopBits, Parity, WordLength
    pub chars: [u8; 6],        // Eof, Error, Break, Event, Xon, Xoff
    pub handflow: [u32; 4],    // ControlHandShake, FlowReplace, XonLimit, XoffLimit
    pub timeouts: [u32; 5], // ReadInterval, ReadTotalMult, ReadTotalConst, WriteTotalMult, WriteTotalConst
    /// Modem Control Register bits asserted by THIS port (DTR_STATE | RTS_STATE).
    pub modem_control: u32,
    pub break_state: u8,
    pub rx_buffer_size: u32,
    pub tx_buffer_size: u32,
    pub stats: SERIALPERF_STATS,
}

impl PortState {
    pub const fn new() -> Self {
        PortState {
            rx: RingBuffer::new(),
            pending_read_irp: 0,
            pending_event_irp: 0,
            wait_mask: 0,
            pending_events: 0,
            baud: 0,
            line_control: [0; 3],
            chars: [0; 6],
            handflow: [0; 4],
            timeouts: [0; 5],
            modem_control: 0,
            break_state: 0,
            rx_buffer_size: RING_BUFFER_SIZE as u32,
            tx_buffer_size: RING_BUFFER_SIZE as u32,
            stats: SERIALPERF_STATS {
                ReceivedCount: 0,
                TransmittedCount: 0,
                FrameErrorCount: 0,
                SerialOverrunErrorCount: 0,
                BufferOverrunErrorCount: 0,
                ParityErrorCount: 0,
            },
        }
    }
}

#[repr(C)]
pub struct PairContext {
    pub magic: u32,
    pub lock: u64, // KSPIN_LOCK (ULONG_PTR)
    pub ref_count: i32,
    pub deleting: u32, // bool: teardown requested
    pub pair_id: u32,
    pub port: [PortState; 2], // 0 = A, 1 = B
}

impl PairContext {
    pub const fn new(pair_id: u32) -> Self {
        PairContext {
            magic: COM0COM_MAGIC,
            lock: 0,
            ref_count: 0,
            deleting: 0,
            pair_id,
            port: [PortState::new(), PortState::new()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_roundtrip() {
        let mut rb = RingBuffer::new();
        assert_eq!(rb.available_to_read(), 0);
        let written = rb.write(&[1, 2, 3, 4, 5]);
        assert_eq!(written, 5);
        assert_eq!(rb.available_to_read(), 5);
        let mut out = [0u8; 8];
        let read = rb.read(&mut out);
        assert_eq!(read, 5);
        assert_eq!(&out[..5], &[1, 2, 3, 4, 5]);
        assert_eq!(rb.available_to_read(), 0);
    }

    #[test]
    fn ring_buffer_wraps() {
        let mut rb = RingBuffer::new();
        let block = [0xABu8; 4096];
        // Fill almost entirely, drain, fill again to force head/tail wrap.
        assert_eq!(rb.write(&block), 4096);
        let mut sink = [0u8; 4096];
        assert_eq!(rb.read(&mut sink), 4096);
        assert_eq!(rb.write(&block), 4096);
        assert_eq!(rb.available_to_read(), 4096);
        assert_eq!(rb.read(&mut sink), 4096);
        assert!(sink.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn ring_buffer_capacity_limits() {
        let mut rb = RingBuffer::new();
        assert_eq!(rb.available_to_write(), RING_BUFFER_SIZE - 1);
        // Oversized write clamps to capacity-1 (one slot kept for full/empty
        // disambiguation).
        let big = [7u8; RING_BUFFER_SIZE];
        assert_eq!(rb.write(&big), RING_BUFFER_SIZE - 1);
        assert_eq!(rb.available_to_write(), 0);
        // A second write accepts nothing while full.
        assert_eq!(rb.write(&[9u8]), 0);
        assert_eq!(rb.available_to_read(), RING_BUFFER_SIZE - 1);
        // Reading with a too-large destination clamps to available count.
        let mut out = [0u8; RING_BUFFER_SIZE];
        assert_eq!(rb.read(&mut out), RING_BUFFER_SIZE - 1);
        assert!(rb.is_empty());
    }

    #[test]
    fn ring_buffer_partial_reads_and_empty() {
        let mut rb = RingBuffer::new();
        assert_eq!(rb.write(b"hello"), 5);
        let mut out = [0u8; 2];
        assert_eq!(rb.read(&mut out), 2);
        assert_eq!(&out, b"he");
        assert_eq!(rb.available_to_read(), 3);
        let mut rest = [0u8; 4];
        assert_eq!(rb.read(&mut rest), 3);
        assert_eq!(&rest[..3], b"llo");
        // Reading an empty buffer returns 0.
        assert_eq!(rb.read(&mut rest), 0);
        assert!(rb.is_empty());
    }

    #[test]
    fn port_state_new_defaults() {
        let p = PortState::new();
        assert_eq!(p.baud, 0);
        assert_eq!(p.modem_control, 0);
        assert_eq!(p.break_state, 0);
        assert_eq!(p.wait_mask, 0);
        assert_eq!(p.pending_events, 0);
        assert_eq!(p.pending_read_irp, 0);
        assert_eq!(p.pending_event_irp, 0);
        assert!(p.rx.is_empty());
        assert_eq!(p.rx_buffer_size, RING_BUFFER_SIZE as u32);
        assert_eq!(p.tx_buffer_size, RING_BUFFER_SIZE as u32);
        assert_eq!(p.stats.ReceivedCount, 0);
        assert_eq!(p.stats.FrameErrorCount, 0);
        assert_eq!(p.line_control, [0; 3]);
        assert_eq!(p.chars, [0; 6]);
        assert_eq!(p.handflow, [0; 4]);
        assert_eq!(p.timeouts, [0; 5]);
    }

    #[test]
    fn pair_context_new_invariants() {
        let pc = PairContext::new(7);
        assert_eq!(pc.magic, COM0COM_MAGIC);
        assert_eq!(pc.magic, 0x434f_4d43); // "COMC"
        assert_eq!(pc.pair_id, 7);
        assert_eq!(pc.ref_count, 0);
        assert_eq!(pc.deleting, 0);
        assert_eq!(pc.lock, 0);
        assert_eq!(pc.port[0].rx_buffer_size, pc.port[1].rx_buffer_size);
        assert!(pc.port[0].rx.is_empty() && pc.port[1].rx.is_empty());
    }
}
