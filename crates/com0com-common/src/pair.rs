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
//!
//! Emulation state that the port driver needs (pacing carry, RNG state, XOFF
//! counter, noise counters) lives here too, because the port driver only ever
//! sees this structure. See `crate::emulation` for the pure logic that
//! operates on those fields.

use crate::protocol::Com0comPairParams;
use crate::serial_structs::SERIALPERF_STATS;

pub const COM0COM_MAGIC: u32 = 0x434f_4d43; // "COMC"
pub const RING_BUFFER_SIZE: usize = 16 * 1024;

/// Usable capacity of a [`RingBuffer`] — `RING_BUFFER_SIZE - 1`, one slot being
/// reserved to tell "full" apart from "empty".
///
/// Exposed as a plain constant as well as [`RingBuffer::capacity`] because the
/// kernel drivers need the value without materialising a 16 KiB `RingBuffer` on
/// the (very small) kernel stack.
pub const RING_CAPACITY: usize = RING_BUFFER_SIZE - 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RingBuffer {
    pub data: [u8; RING_BUFFER_SIZE],
    pub head: u32, // next byte to write
    pub tail: u32, // next byte to read
}

/// Default PRNG seed stored in `PortState::rng_state`. Mirrors
/// `emulation::DEFAULT_RNG_SEED`; duplicated as a literal because the field
/// initialiser must be a `const`.
pub const DEFAULT_RNG_SEED: u32 = 0x9E37_79B9;

impl RingBuffer {
    pub const fn new() -> Self {
        RingBuffer {
            data: [0u8; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
        }
    }

    /// Maximum number of unread bytes the buffer can hold. One slot is
    /// permanently reserved to distinguish "full" from "empty".
    pub const fn capacity(&self) -> usize {
        RING_CAPACITY
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
        for &b in &src[..n] {
            let pos = Self::mask(self.head);
            self.data[pos] = b;
            self.head = (self.head + 1) % RING_BUFFER_SIZE as u32;
        }
        n
    }

    /// Copy as many bytes as fit from `src`, additionally honouring `cap` as an
    /// absolute queue limit.
    ///
    /// Unlike [`RingBuffer::write`], `cap` bounds the *total* number of queued
    /// bytes (not just the free space): the room for this call is
    /// `min(available_to_write(), cap - available_to_read())`. This is what
    /// makes `IOCTL_SERIAL_SET_QUEUE_SIZE` effective — the ring keeps its
    /// physical size, but the usable window shrinks.
    pub fn write_capped(&mut self, src: &[u8], cap: usize) -> usize {
        let room = self
            .available_to_write()
            .min(cap.saturating_sub(self.available_to_read()));
        let n = room.min(src.len());
        for &b in &src[..n] {
            let pos = Self::mask(self.head);
            self.data[pos] = b;
            self.head = (self.head + 1) % RING_BUFFER_SIZE as u32;
        }
        n
    }

    /// Copy up to `dst.len()` bytes out of the buffer. Returns the count read.
    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let avail = self.available_to_read();
        let n = avail.min(dst.len());
        for slot in dst.iter_mut().take(n) {
            let pos = Self::mask(self.tail);
            *slot = self.data[pos];
            self.tail = (self.tail + 1) % RING_BUFFER_SIZE as u32;
        }
        n
    }

    pub fn is_empty(&self) -> bool {
        self.available_to_read() == 0
    }

    /// 就地清空。不要用 `*rb = RingBuffer::new()` 代替：那会先在栈上构造
    /// 16 KiB 的临时值，内核栈放不下。
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.data.fill(0);
    }
}

impl Default for RingBuffer {
    fn default() -> Self {
        RingBuffer::new()
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
    /// Effective RX capacity (bytes), i.e. the queue limit requested through
    /// `IOCTL_SERIAL_SET_QUEUE_SIZE`. Defaults to the ring capacity.
    pub queue_limit: u32,
    /// `LinePacer::last_100ns` (baud-rate emulation, `emu_br`).
    pub pacer_last_100ns: u64,
    /// `LinePacer::carry_half_bits` (baud-rate emulation, `emu_br`), in
    /// `emulation::PACER_CARRY_SCALE` units per half-bit.
    pub pacer_carry_half_bits: u64,
    /// `Xorshift32` state for noise injection (`emu_noise`).
    pub rng_state: u32,
    /// `XoffState::remaining` (bytes left before the port halts).
    pub xoff_remaining: u32,
    /// `XoffState::halted` as 0/1 (kept as `u8` for a stable `#[repr(C)]`
    /// layout — `bool` would work too, but 0/1 is explicit across the FFI).
    pub tx_halted: u8,
    /// Cumulative count of bytes corrupted by noise injection. Purely an
    /// observability counter (not part of `SERIALPERF_STATS`).
    pub noise_count: u32,
    /// 还有多少个后续字符应继续作为坏字符发送（上游 `brokeChars`）。
    /// 起始位/停止位故障会把它置为 `RAND() % 512`，跨多次写入保留。
    pub broke_chars: u16,
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
            queue_limit: RING_CAPACITY as u32,
            pacer_last_100ns: 0,
            pacer_carry_half_bits: 0,
            rng_state: DEFAULT_RNG_SEED,
            xoff_remaining: 0,
            tx_halted: 0,
            noise_count: 0,
            broke_chars: 0,
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

impl Default for PortState {
    fn default() -> Self {
        PortState::new()
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
    /// Per-pair emulation parameters, mirrored here from the bus driver's
    /// `PairSlot` so that the *port* driver (which only ever sees this
    /// structure) can read `emu_br` / `emu_noise` / `emu_overrun` /
    /// `noise_probability` without an extra IOCTL round-trip.
    pub params: Com0comPairParams,
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
            params: Com0comPairParams::new(),
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
    fn ring_buffer_capacity_matches_writable_window() {
        let mut rb = RingBuffer::new();
        assert_eq!(rb.capacity(), RING_BUFFER_SIZE - 1);
        assert_eq!(rb.capacity(), rb.available_to_write());
        // Capacity is a `const fn`, so it is also available without a value
        // being mutated.
        let _ = rb.write(&[1, 2, 3]);
        assert_eq!(rb.capacity(), RING_BUFFER_SIZE - 1);
        assert_eq!(rb.available_to_write(), RING_BUFFER_SIZE - 4);
    }

    #[test]
    fn write_capped_honours_queue_limit() {
        let mut rb = RingBuffer::new();
        // Cap below the physical capacity: only `cap` bytes may be queued.
        let src = [0x11u8; 8];
        assert_eq!(rb.write_capped(&src, 4), 4);
        assert_eq!(rb.available_to_read(), 4);
        // The queue is at its cap -> nothing more is accepted.
        assert_eq!(rb.write_capped(&src, 4), 0);
        // Draining frees room again, but only up to the cap.
        let mut sink = [0u8; 8];
        assert_eq!(rb.read(&mut sink), 4);
        assert_eq!(rb.write_capped(&src, 4), 4);
        // A cap larger than the physical buffer degrades to plain `write`.
        let mut big = RingBuffer::new();
        let big_src = [0x22u8; RING_BUFFER_SIZE];
        assert_eq!(
            big.write_capped(&big_src, RING_BUFFER_SIZE * 4),
            RING_BUFFER_SIZE - 1
        );
    }

    #[test]
    fn write_capped_zero_cap_writes_nothing() {
        let mut rb = RingBuffer::new();
        assert_eq!(rb.write_capped(&[1, 2, 3], 0), 0);
        assert!(rb.is_empty());
        // An empty source never writes anything either.
        assert_eq!(rb.write_capped(&[], 16), 0);
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
        // New emulation fields.
        assert_eq!(p.queue_limit, RING_BUFFER_SIZE as u32 - 1);
        assert_eq!(p.queue_limit, p.rx.capacity() as u32);
        assert_eq!(p.pacer_last_100ns, 0);
        assert_eq!(p.pacer_carry_half_bits, 0);
        assert_eq!(p.rng_state, DEFAULT_RNG_SEED);
        assert_eq!(p.xoff_remaining, 0);
        assert_eq!(p.tx_halted, 0);
        assert_eq!(p.noise_count, 0);
        assert_eq!(p.broke_chars, 0);
        assert_eq!(p.stats.ReceivedCount, 0);
        assert_eq!(p.stats.FrameErrorCount, 0);
        assert_eq!(p.stats.SerialOverrunErrorCount, 0);
        assert_eq!(p.stats.BufferOverrunErrorCount, 0);
        assert_eq!(p.stats.ParityErrorCount, 0);
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
        // Emulation params default to all-zero (no emulation enabled), which is
        // what the bus writes into the slot before CREATE_PAIR fills them in.
        assert_eq!(pc.params.emu_br, 0);
        assert_eq!(pc.params.emu_overrun, 0);
        assert_eq!(pc.params.emu_noise, 0);
        assert_eq!(pc.params.noise_probability, 0);
        assert_eq!(pc.params.exclusive_mode, 0);
        assert_eq!(pc.params.plugin_mode, 0);
        // 接线例外：默认必须是上游空 Modem 映射，否则端口驱动在建对后
        // 立刻读 MSR 会得到全零，CTS/DSR 永不有效。
        assert_eq!(pc.params.pin_wiring, crate::constants::DEFAULT_PIN_WIRING);
        assert_eq!(pc.port[0].queue_limit, pc.port[1].queue_limit);
    }

    // ------------------------------------------------------------------
    // Adversarial tests for the queue-limit semantics.
    // ------------------------------------------------------------------

    #[test]
    fn write_capped_queue_limit_scenario_from_the_design() {
        // RING_BUFFER_SIZE = 16384, 100 bytes already queued, queue_limit = 200,
        // a 500-byte write must enqueue exactly 100 bytes.
        let mut rb = RingBuffer::new();
        assert_eq!(rb.write(&[0xAAu8; 100]), 100);
        let src = [0xBBu8; 500];
        assert_eq!(rb.write_capped(&src, 200), 100);
        assert_eq!(rb.available_to_read(), 200);
        // At the cap: nothing more is accepted, and the leftover is dropped
        // (not staged) -- that is what `emu_overrun` charges as lost bytes.
        assert_eq!(rb.write_capped(&src, 200), 0);
        // Draining 50 frees exactly 50, no more.
        let mut sink = [0u8; 50];
        assert_eq!(rb.read(&mut sink), 50);
        assert!(sink.iter().all(|&b| b == 0xAA));
        assert_eq!(rb.write_capped(&src, 200), 50);
        assert_eq!(rb.available_to_read(), 200);
        // Draining everything restores the full 200-byte window.
        let mut all = [0u8; RING_BUFFER_SIZE];
        assert_eq!(rb.read(&mut all), 200);
        assert!(rb.is_empty());
        assert_eq!(rb.write_capped(&src, 200), 200);
    }

    #[test]
    fn write_capped_wraps_across_the_ring_boundary() {
        let mut rb = RingBuffer::new();
        // Park head/tail 8 bytes before the end of the physical buffer.
        let block = [0x5Cu8; RING_BUFFER_SIZE - 8];
        assert_eq!(rb.write(&block), RING_BUFFER_SIZE - 8);
        let mut sink = [0u8; RING_BUFFER_SIZE];
        assert_eq!(rb.read(&mut sink), RING_BUFFER_SIZE - 8);
        assert!(sink[..RING_BUFFER_SIZE - 8].iter().all(|&b| b == 0x5C));
        assert_eq!(rb.head, (RING_BUFFER_SIZE - 8) as u32);
        assert_eq!(rb.tail, (RING_BUFFER_SIZE - 8) as u32);

        // A 16-byte write now straddles index 16383 -> 0.
        assert_eq!(rb.write_capped(&[0x77u8; 16], 64), 16);
        assert_eq!(rb.available_to_read(), 16);
        let mut out = [0u8; 16];
        assert_eq!(rb.read(&mut out), 16);
        assert!(out.iter().all(|&b| b == 0x77));
        assert!(rb.is_empty());
        // The cap still applies after the wrap: 200 bytes in, 64 accepted.
        assert_eq!(rb.write_capped(&[0x99u8; 200], 64), 64);
        assert_eq!(rb.available_to_read(), 64);
    }

    #[test]
    fn write_capped_equals_write_when_the_cap_is_not_binding() {
        let mut a = RingBuffer::new();
        let mut b = RingBuffer::new();
        let src = [0x3Cu8; 100];
        assert_eq!(a.write_capped(&src, usize::MAX), b.write(&src));
        assert_eq!(a.available_to_read(), b.available_to_read());
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn port_state_default_queue_limit_lets_data_through() {
        // Regression guard for the historical bug: a zeroed `queue_limit`
        // makes `write_capped` swallow every byte.
        let p = PortState::new();
        assert!(p.queue_limit > 0);
        let mut rb = RingBuffer::new();
        assert_eq!(rb.write_capped(&[1u8, 2, 3], p.queue_limit as usize), 3);
        assert_eq!(p.queue_limit as usize, rb.capacity());
        // Same for a freshly built PairContext (what the bus allocates).
        let pc = PairContext::new(0);
        assert!(pc.port[0].queue_limit > 0 && pc.port[1].queue_limit > 0);
        // The RNG seed must never be zero, or xorshift32 degenerates to 0.
        assert_ne!(pc.port[0].rng_state, 0);
        assert_ne!(pc.port[1].rng_state, 0);
    }
}
