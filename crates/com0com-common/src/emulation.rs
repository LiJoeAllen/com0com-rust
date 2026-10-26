//! Pure, `no_std`, 100% safe-Rust building blocks for the com0com "P2"
//! emulation features (baud-rate pacing, noise injection, overrun emulation,
//! XOFF counting and queue sizing).
//!
//! Everything here is deterministic and side-effect free so it can be unit
//! tested on the host. The kernel drivers (`com0comport`) only wire these
//! helpers into their IRP paths; they never re-implement the arithmetic.
//!
//! Rationale: the port driver cannot be compiled without the WDK on this
//! machine, so all decision-making logic is pushed down into this crate where
//! it *is* compiled, clippy'd and tested.

/// Number of 100-nanosecond ticks in one second.
///
/// Same unit as the interrupt time returned by `KeQueryInterruptTime`, which is
/// what the kernel caller feeds into [`LinePacer`].
pub const TICKS_PER_SEC_100NS: u64 = 10_000_000;

/// Upper bound on the number of bytes a single [`LinePacer::bytes_available`]
/// call may report. Guards against a pathological `u64` elapsed time producing
/// an absurd budget (and against a caller looping on the result).
pub const MAX_PACER_BYTES: u32 = 1 << 20;

/// Default PRNG seed. Also used as the fallback for a zero seed (a xorshift
/// state of 0 is a fixed point and would generate zeros forever).
pub const DEFAULT_RNG_SEED: u32 = 0x9E37_79B9;

/// Deterministic 32-bit xorshift PRNG (Marsaglia's `xorshift32`).
///
/// Used for noise injection so that a given seed reproduces byte-for-byte the
/// same corruption pattern — which is what makes the driver testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Xorshift32(pub u32);

impl Xorshift32 {
    /// Create a generator. A seed of `0` falls back to [`DEFAULT_RNG_SEED`].
    pub fn new(seed: u32) -> Self {
        Xorshift32(if seed == 0 { DEFAULT_RNG_SEED } else { seed })
    }

    /// Advance the state and return the next pseudo-random `u32`.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Return a pseudo-random value in `[0, n)`. Returns `0` when `n == 0`.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.next_u32() % n
    }
}

impl Default for Xorshift32 {
    fn default() -> Self {
        Xorshift32::new(DEFAULT_RNG_SEED)
    }
}

/// Half-bits in one UART frame.
///
/// Half-bits are used instead of bits so that the 1.5-stop-bit frame format
/// stays an exact integer (no floating point, no rounding drift in the pacer).
///
/// Frame = 1 start bit + `word_length` data bits + optional parity bit +
/// stop bits. `word_length == 0` is treated as 8 (the power-on default).
///
/// `stop_bits` follows `SERIAL_LINE_CONTROL.StopBits`:
/// `0` -> 1 stop bit, `1` -> 1.5 stop bits, `2` -> 2 stop bits, anything else
/// falls back to 1 stop bit.
pub fn frame_half_bits(word_length: u8, parity: u8, stop_bits: u8) -> u32 {
    let data: u32 = if word_length == 0 {
        8
    } else {
        word_length as u32
    };
    let parity_bits: u32 = if parity == 0 { 0 } else { 1 };
    let stop_half: u32 = match stop_bits {
        0 => 2, // 1 stop bit
        1 => 3, // 1.5 stop bits
        2 => 4, // 2 stop bits
        _ => 2, // unknown -> 1 stop bit
    };
    2 * (1 + data + parity_bits) + stop_half
}

/// Fixed-point scale of [`LinePacer::carry_half_bits`].
///
/// The carry is kept in 1/65536 half-bit units (Q16) instead of whole
/// half-bits. Rationale: a writer that submits small chunks faster than
/// `TICKS_PER_SEC_100NS / (2 * baud)` ticks apart would, with an integer
/// half-bit carry, truncate *every* call down to zero half-bits and therefore
/// never make progress at all (a hang). Keeping the sub-tick remainder costs
/// nothing — the field stays a single `u64` — and makes the pacer exact to
/// within 1/65536 of a half-bit.
pub const PACER_CARRY_SCALE: u64 = 1 << 16;

/// Baud-rate pacing state for one direction of one port.
///
/// The kernel side stores the two fields inside `PortState` (as two plain
/// `u64`s so the `#[repr(C)]` layout stays trivial) and reconstitutes this
/// struct for the duration of a write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinePacer {
    /// Interrupt time (100ns units) of the last accounted transfer.
    pub last_100ns: u64,
    /// Leftover half-bits from previous rounds, in 1/`PACER_CARRY_SCALE`
    /// half-bit units. Never lost.
    pub carry_half_bits: u64,
}

impl LinePacer {
    /// Restart pacing at `now_100ns` with an empty carry.
    pub fn reset(&mut self, now_100ns: u64) {
        self.last_100ns = now_100ns;
        self.carry_half_bits = 0;
    }

    /// The carry expressed in whole half-bits (the fractional part dropped).
    pub fn carry_whole_half_bits(&self) -> u64 {
        self.carry_half_bits / PACER_CARRY_SCALE
    }

    /// Return how many bytes the line should have delivered since the last
    /// call, and advance the internal state accordingly.
    ///
    /// * `baud == 0` or `frame_half_bits == 0` means "no rate limit"; the
    ///   function returns `u32::MAX` and leaves the state untouched. The caller
    ///   still has to clamp by the buffer capacity.
    /// * `now_100ns <= last_100ns` (time did not advance, or went backwards)
    ///   yields only what the carry can already produce; `last` is not updated.
    /// * The result is clamped to [`MAX_PACER_BYTES`]; the leftover half-bits
    ///   stay in `carry_half_bits`, so no throughput is silently lost.
    pub fn bytes_available(&mut self, now_100ns: u64, baud: u32, frame_half_bits: u32) -> u32 {
        if baud == 0 || frame_half_bits == 0 {
            return u32::MAX;
        }
        let frame = frame_half_bits as u64;
        let frame_scaled = (frame as u128) * (PACER_CARRY_SCALE as u128);
        let carry = self.carry_half_bits as u128;

        if now_100ns <= self.last_100ns {
            // No new time budget: only the carry can produce bytes.
            if carry < frame_scaled {
                return 0;
            }
            let bytes = carry / frame_scaled;
            self.carry_half_bits = (carry % frame_scaled) as u64;
            return clamp_pacer_bytes(bytes);
        }

        let elapsed = now_100ns - self.last_100ns;
        // `elapsed * baud * 2 * SCALE` overflows u64 for any non-trivial
        // elapsed time, so the whole product is evaluated in u128. The carry is
        // already in scaled half-bit units, so it is added after the tick
        // division — which is arithmetically the same as
        // `elapsed * baud * 2 / TICKS + carry`, just without truncating the
        // sub-tick remainder.
        let total_scaled = (elapsed as u128)
            .saturating_mul(baud as u128)
            .saturating_mul(2)
            .saturating_mul(PACER_CARRY_SCALE as u128)
            / (TICKS_PER_SEC_100NS as u128)
            + carry;

        let bytes = total_scaled / frame_scaled;
        // `frame_scaled` is at most 2^32 * 2^16, so the modulo always fits u64
        // even when `total_scaled` does not.
        self.carry_half_bits = (total_scaled % frame_scaled) as u64;
        self.last_100ns = now_100ns;
        clamp_pacer_bytes(bytes)
    }
}

/// Clamp a raw byte budget to [`MAX_PACER_BYTES`] without overflowing.
fn clamp_pacer_bytes(bytes: u128) -> u32 {
    if bytes > MAX_PACER_BYTES as u128 {
        MAX_PACER_BYTES
    } else {
        bytes as u32
    }
}

/// 上游 com0com 的噪声概率分母：概率字段以 1/100_000_000 为单位。
///
/// 比旧的 0.01% 单位更细，可直接表达上游 `--noise=1e-6` 这类小数概率。
/// `0` 表示不注入，[`NOISE_PROBABILITY_ONE`] 表示每个字符都出错。
pub const NOISE_PROBABILITY_ONE: u32 = 100_000_000;

/// 一次 [`inject_line_noise`] 调用产生的错误统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoiseReport {
    /// 数据被替换或翻转的字符数（起始位错误、坏字符运行、数据位错误）。
    pub corrupted: u32,
    /// 校验错误次数（累加到接收端的 `ParityErrorCount`）。
    pub parity_errors: u32,
    /// 帧错误次数（累加到接收端的 `FrameErrorCount`）。
    pub frame_errors: u32,
}

impl NoiseReport {
    /// 是否记录到任何线路错误（用于决定是否置 `SERIAL_EV_ERR`）。
    pub fn has_errors(&self) -> bool {
        self.corrupted != 0 || self.parity_errors != 0 || self.frame_errors != 0
    }
}

/// 归一化 `SERIAL_LINE_CONTROL.WordLength`：0 表示上电默认的 8 位。
pub fn effective_word_length(word_length: u8) -> u8 {
    if word_length == 0 {
        8
    } else {
        word_length
    }
}

/// 计算接收端的数据位掩码，等价于上游 com0com 的 `AllDataBits` 语义。
///
/// 默认（`all_data_bits == false`）把数据截断到两端字长的较小值，最多 8 位：
/// 任意一端配置为 7 位时，读到的第 8 位都会被清零。`all_data_bits == true`
/// 时保留全部 8 位，兼容只按 8 位处理数据的应用。掩码必须在上完噪声之后
/// 应用，否则噪声可能把数据位重新置回来（上游也是这个顺序）。
pub fn data_mask(all_data_bits: bool, local_word_length: u8, remote_word_length: u8) -> u8 {
    if all_data_bits {
        return 0xFF;
    }
    let bits = 8u8
        .min(effective_word_length(local_word_length))
        .min(effective_word_length(remote_word_length));
    if bits >= 8 {
        0xFF
    } else {
        (1u8 << bits) - 1
    }
}

/// 就地套用 [`data_mask`]。`0xFF` 时直接返回，避免整块内存的无谓写入。
pub fn apply_data_mask(buf: &mut [u8], mask: u8) {
    if mask != 0xFF {
        for byte in buf.iter_mut() {
            *byte &= mask;
        }
    }
}

/// 概率命中判定，沿用上游 `prob > RAND() % C0C_PROBABILITY_ONE` 的语义。
fn noise_hits(rng: &mut Xorshift32, probability: u32) -> bool {
    probability > rng.below(NOISE_PROBABILITY_ONE)
}

/// 生成一个坏字符：随机数据，并按上游规则随机附带校验/帧错误。
fn garbage_char(parity: u8, report: &mut NoiseReport, rng: &mut Xorshift32) -> u8 {
    if parity != 0 && rng.below(2) > 0 {
        report.parity_errors += 1;
    }
    if rng.below(2) > 0 {
        report.frame_errors += 1;
    }
    rng.next_u32() as u8
}

/// 模拟单个字符在线路上的一位故障，对应上游 `BrokeChar()`。
///
/// `word_length` / `parity` 取发送端的线路控制；`broke_chars` 是跨写入保持的
/// “后续坏字符”计数器（上游 `brokeChars`）。起始位或停止位出错会把它设为
/// `RAND() % 512`，随后最多 511 个字符直接变成坏字符，模拟一次线路干扰。
fn broke_char(
    rng: &mut Xorshift32,
    probability: u32,
    word_length: u8,
    parity: u8,
    broke_chars: &mut u16,
    byte: &mut u8,
    report: &mut NoiseReport,
) {
    if !noise_hits(rng, probability) {
        return;
    }
    let word_length = effective_word_length(word_length);
    let parity_bits = u32::from(parity != 0);
    let slots = 1 + 1 + u32::from(word_length) + parity_bits;
    let mut r = rng.below(slots);
    if r == 0 {
        // 起始位错误：当前字符变成坏字符，并开启一段连续坏字符。
        *broke_chars = rng.below(512) as u16;
        *byte = garbage_char(parity, report, rng);
        report.corrupted += 1;
        return;
    }
    r -= 1;
    if r == 0 {
        // 停止位错误：数据不变，但接收端看到帧错误，同样会牵连后续字符。
        *broke_chars = rng.below(512) as u16;
        report.frame_errors += 1;
        return;
    }
    r -= 1;
    if r < u32::from(word_length) {
        // 数据位错误：翻转指定位；ODD/EVEN 校验会同时报校验错。
        // 9 位字长时第 9 位在 u8 上不可见，与上游的截断行为一致。
        if r < 8 {
            *byte ^= 1u8 << r;
        }
        report.corrupted += 1;
        if parity == 1 || parity == 2 {
            report.parity_errors += 1;
        }
        return;
    }
    // 剩下的是校验位槽位：只有启用了校验才会命中。
    report.parity_errors += 1;
}

/// 对一段待发送数据注入线路噪声，返回本次的错误统计。
///
/// * `probability` 以 [`NOISE_PROBABILITY_ONE`] 为分母，`0..=NOISE_PROBABILITY_ONE`。
/// * `word_length`（0 视作 8）和 `parity`（`ODD_PARITY`/`EVEN_PARITY` 等）取自
///   发送端线路控制，决定故障位槽位和各错误类型。
/// * `broke_chars` 是调用方持久保存的坏字符计数器；跨调用保留，模拟上游
///   一次干扰影响后续多个字符的行为。调用方应在端口空闲/关闭时清零。
///
/// 数据位掩码不在这里应用：调用方必须在上完噪声之后再调用
/// [`apply_data_mask`]，顺序与上游一致。
pub fn inject_line_noise(
    rng: &mut Xorshift32,
    probability: u32,
    word_length: u8,
    parity: u8,
    broke_chars: &mut u16,
    buf: &mut [u8],
) -> NoiseReport {
    let mut report = NoiseReport::default();
    if probability == 0 && *broke_chars == 0 {
        return report;
    }
    for byte in buf.iter_mut() {
        if *broke_chars > 0 {
            *broke_chars -= 1;
            *byte = garbage_char(parity, &mut report, rng);
            report.corrupted += 1;
        } else {
            broke_char(
                rng,
                probability,
                word_length,
                parity,
                broke_chars,
                byte,
                &mut report,
            );
        }
    }
    report
}

/// Clamp a requested queue size (`SERIAL_QUEUE_SIZE.InSize` / `.OutSize`) into
/// `[1, max]`.
///
/// `req == 0` means "leave the current size alone"; `max` is returned so the
/// caller can apply it idempotently (it is also the largest legal value, so no
/// caller ever grows past the physical ring buffer).
pub fn clamp_queue_size(req: u32, max: u32) -> u32 {
    if req == 0 {
        return max;
    }
    if req > max {
        return max;
    }
    req
}

/// XOFF counter state for one transmitting port.
///
/// Models `IOCTL_SERIAL_XOFF_COUNTER`: after the counter is armed, only
/// `remaining` more bytes may be handed to the peer; once it is exhausted the
/// port halts until `SET_XON` / `PURGE` clears it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XoffState {
    /// Bytes still allowed to be delivered before halting (0 = unarmed).
    pub remaining: u32,
    /// Set once `remaining` has been drained.
    pub halted: bool,
}

impl XoffState {
    /// Armed state for `count` bytes; `count == 0` cancels a previous counter.
    pub fn on_set(&mut self, count: u32) {
        if count == 0 {
            self.remaining = 0;
            self.halted = false;
            return;
        }
        self.remaining = count;
        self.halted = false;
    }

    /// How many of `want` bytes may be delivered right now. Drains `remaining`
    /// and latches `halted` when it reaches zero.
    pub fn admit(&mut self, want: usize) -> usize {
        if self.halted {
            return 0;
        }
        if self.remaining == 0 {
            // No counter armed: unlimited.
            return want;
        }
        let capped: u32 = if want > u32::MAX as usize {
            u32::MAX
        } else {
            want as u32
        };
        let n = if self.remaining < capped {
            self.remaining
        } else {
            capped
        };
        self.remaining -= n;
        if self.remaining == 0 {
            self.halted = true;
        }
        n as usize
    }

    /// Cancel the counter and un-halt the port (`SET_XON` / `PURGE`).
    pub fn clear(&mut self) {
        self.remaining = 0;
        self.halted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xorshift_seed_zero_falls_back() {
        assert_eq!(Xorshift32::new(0).0, DEFAULT_RNG_SEED);
        assert_eq!(Xorshift32::new(0).0, 0x9E37_79B9);
        // A non-zero seed is kept verbatim.
        assert_eq!(Xorshift32::new(42).0, 42);
        assert_eq!(Xorshift32::default().0, DEFAULT_RNG_SEED);
    }

    #[test]
    fn xorshift_is_deterministic_and_moves() {
        let seed = 0x1234_5678u32;
        let mut a = Xorshift32::new(seed);
        let mut b = Xorshift32::new(seed);
        let a1 = a.next_u32();
        let b1 = b.next_u32();
        // Same seed -> same stream.
        assert_eq!(a1, b1);
        // The state advanced, so the generator never repeats its seed/output.
        assert_ne!(a1, seed);
        let a2 = a.next_u32();
        let b2 = b.next_u32();
        assert_eq!(a2, b2);
        assert_ne!(a1, a2);
    }

    #[test]
    fn xorshift_below_is_bounded() {
        let mut rng = Xorshift32::new(7);
        assert_eq!(rng.below(0), 0);
        for _ in 0..128 {
            assert!(rng.below(10) < 10);
            assert_eq!(rng.below(1), 0);
            assert!(rng.below(10000) < 10000);
        }
    }

    #[test]
    fn frame_half_bits_matches_uart_frames() {
        // 8N1: (1 start + 8 data + 0 parity) * 2 + 2 = 20
        assert_eq!(frame_half_bits(8, 0, 0), 20);
        // 8N1.5 -> 21, 8N2 -> 22
        assert_eq!(frame_half_bits(8, 0, 1), 21);
        assert_eq!(frame_half_bits(8, 0, 2), 22);
        // Parity adds one bit (two half-bits).
        assert_eq!(frame_half_bits(8, 1, 0), 22);
        assert_eq!(frame_half_bits(8, 2, 0), 22);
        // 7E2: (1 + 7 + 1) * 2 + 4 = 22
        assert_eq!(frame_half_bits(7, 2, 2), 22);
        // Word length 0 falls back to 8.
        assert_eq!(frame_half_bits(0, 0, 0), 20);
        // Unknown stop-bit encoding falls back to 1 stop bit: (1+5)*2+2 = 14.
        assert_eq!(frame_half_bits(5, 0, 3), 14);
    }

    #[test]
    fn pacer_zero_baud_or_frame_is_unlimited() {
        let mut p = LinePacer::default();
        assert_eq!(p.bytes_available(1_000, 0, 20), u32::MAX);
        assert_eq!(p.bytes_available(1_000, 9600, 0), u32::MAX);
        // State untouched by the "unlimited" path.
        assert_eq!(p.last_100ns, 0);
        assert_eq!(p.carry_half_bits, 0);
    }

    #[test]
    fn pacer_no_time_advance_returns_zero() {
        let mut p = LinePacer::default();
        p.reset(5_000);
        assert_eq!(p.bytes_available(5_000, 9600, 20), 0);
        assert_eq!(p.bytes_available(1_000, 9600, 20), 0);
        assert_eq!(p.last_100ns, 5_000);
    }

    #[test]
    fn pacer_9600_8n1_delivers_960_bytes_per_second() {
        let mut p = LinePacer::default();
        p.reset(0);
        // One whole second at 9600 8N1.
        let n = p.bytes_available(TICKS_PER_SEC_100NS, 9600, frame_half_bits(8, 0, 0));
        assert_eq!(n, 960);
        // A second second delivers the same amount.
        let n2 = p.bytes_available(2 * TICKS_PER_SEC_100NS, 9600, frame_half_bits(8, 0, 0));
        assert_eq!(n2, 960);
    }

    #[test]
    fn pacer_carry_is_never_lost() {
        let mut p = LinePacer::default();
        p.reset(0);
        let frame = frame_half_bits(8, 0, 0); // 20 half-bits
        let mut total: u32 = 0;
        // 10 ms steps at 9600 8N1 = exactly 192 half-bits per step, so 100
        // steps must deliver exactly one second worth of data (960 bytes).
        for step in 1..=100u64 {
            total += p.bytes_available(step * 100_000, 9600, frame);
        }
        assert_eq!(total, 960);
        assert!(p.carry_whole_half_bits() < frame as u64);
    }

    #[test]
    fn pacer_sub_tick_remainder_is_not_truncated_away() {
        let mut p = LinePacer::default();
        p.reset(0);
        let frame = frame_half_bits(8, 0, 0); // 20 half-bits
                                              // 52 us steps: each call earns 0.9984 half-bits, i.e. *less than one*.
                                              // An integer half-bit carry would round every call down to zero and the
                                              // port would never transmit; the Q16 carry accumulates instead.
        let mut total: u32 = 0;
        for step in 1..=210u64 {
            total += p.bytes_available(step * 520, 9600, frame);
        }
        // 210 * 52 us = 10.92 ms -> 10.92 ms * 960 B/s = 10.48 bytes.
        assert_eq!(total, 10);
    }

    #[test]
    fn pacer_fine_grained_writes_still_progress() {
        let mut p = LinePacer::default();
        p.reset(0);
        let frame = frame_half_bits(8, 0, 0);
        let mut total: u32 = 0;
        // 100 us steps (1000 ticks) for 104 ms.
        for step in 1..=1040u64 {
            total += p.bytes_available(step * 1_000, 9600, frame);
        }
        // 104 ms at 960 B/s = 99.8 bytes -> 99 whole bytes, no starvation.
        assert_eq!(total, 99);
    }

    #[test]
    fn pacer_huge_elapsed_does_not_overflow() {
        let mut p = LinePacer::default();
        p.reset(0);
        // u64::MAX ticks at 4 Mbit/s: the intermediate product needs u128.
        let n = p.bytes_available(u64::MAX, 4_000_000, 20);
        assert_eq!(n, MAX_PACER_BYTES);
        // Worst case: maximum baud as well.
        let mut q = LinePacer::default();
        q.reset(0);
        assert_eq!(q.bytes_available(u64::MAX, u32::MAX, 20), MAX_PACER_BYTES);
        // The carry is always smaller than one frame, even in the worst case.
        assert!(q.carry_whole_half_bits() < 20);
    }

    #[test]
    fn pacer_reset_clears_state() {
        let mut p = LinePacer {
            last_100ns: 123,
            carry_half_bits: 19,
        };
        p.reset(1_000);
        assert_eq!(p.last_100ns, 1_000);
        assert_eq!(p.carry_half_bits, 0);
        assert_eq!(p.bytes_available(1_000, 9600, 20), 0);
    }

    #[test]
    fn line_noise_zero_probability_is_identity() {
        let mut rng = Xorshift32::new(1);
        let mut broke_chars = 0u16;
        let mut buf = [0xA5u8; 32];
        let before = buf;
        let report = inject_line_noise(&mut rng, 0, 8, 0, &mut broke_chars, &mut buf);
        assert_eq!(report, NoiseReport::default());
        assert!(!report.has_errors());
        assert_eq!(buf, before);
        assert_eq!(broke_chars, 0);
        // 空输入即使在满概率下也不产生错误。
        let mut empty: [u8; 0] = [];
        let report = inject_line_noise(
            &mut rng,
            NOISE_PROBABILITY_ONE,
            8,
            2,
            &mut broke_chars,
            &mut empty,
        );
        assert!(!report.has_errors());
    }

    #[test]
    fn line_noise_full_probability_reports_all_fault_classes() {
        let mut rng = Xorshift32::new(99);
        let mut broke_chars = 0u16;
        let mut buf = [0x00u8; 4096];
        // EVEN 校验：数据位错误和校验位错误都会报校验错。
        let report = inject_line_noise(
            &mut rng,
            NOISE_PROBABILITY_ONE,
            8,
            2,
            &mut broke_chars,
            &mut buf,
        );
        assert!(report.corrupted > 0, "没有坏字符: {report:?}");
        assert!(report.parity_errors > 0, "没有校验错误: {report:?}");
        assert!(report.frame_errors > 0, "没有帧错误: {report:?}");
        assert!(report.has_errors());
        // 每个字符至少经历一次故障判定（坏字符运行中的字符除外，但它们
        // 会以 corrupted 计入），所以三类计数之和不会少于字符数。
        assert!(
            report.corrupted + report.parity_errors + report.frame_errors >= 4096,
            "错误计数偏少: {report:?}"
        );
    }

    #[test]
    fn line_noise_is_reproducible_for_a_seed() {
        let run = |seed: u32| {
            let mut rng = Xorshift32::new(seed);
            let mut broke = 0u16;
            let mut buf = [0x5Au8; 256];
            let report = inject_line_noise(&mut rng, 5_000_000, 8, 1, &mut broke, &mut buf);
            (buf, report, broke, rng.0)
        };
        let (a, ra, ba, sa) = run(0x00C0_FFEE);
        let (b, rb, bb, sb) = run(0x00C0_FFEE);
        assert_eq!(a, b);
        assert_eq!(ra, rb);
        assert_eq!(ba, bb);
        assert_eq!(sa, sb);
        // 不同种子必须产生不同的损坏模式。
        let (c, rc, _, _) = run(0x0BAD_F00D);
        assert_ne!(a, c);
        assert!(rc.has_errors());
    }

    #[test]
    fn broken_char_runs_consume_the_counter_before_new_faults() {
        let mut rng = Xorshift32::new(7);
        let mut broke_chars = 3u16;
        let mut buf = [0x11u8; 8];
        // 概率为 0 时不会触发新故障，只消费预置的 3 个坏字符。
        let report = inject_line_noise(&mut rng, 0, 8, 0, &mut broke_chars, &mut buf);
        assert_eq!(broke_chars, 0);
        assert_eq!(report.corrupted, 3);
        assert_eq!(&buf[3..], &[0x11u8; 5]);
    }

    #[test]
    fn data_mask_matches_upstream_all_data_bits() {
        // 默认截断到两端字长的较小值。
        assert_eq!(data_mask(false, 7, 8), 0x7F);
        assert_eq!(data_mask(false, 8, 7), 0x7F);
        assert_eq!(data_mask(false, 5, 8), 0x1F);
        assert_eq!(data_mask(false, 8, 8), 0xFF);
        // 0 按 8 位处理，与未配置的线路控制一致。
        assert_eq!(data_mask(false, 0, 0), 0xFF);
        // AllDataBits 强制保留 8 位。
        assert_eq!(data_mask(true, 5, 7), 0xFF);
        // 9 位字长也不可能产生超过 u8 的掩码。
        assert_eq!(data_mask(false, 9, 9), 0xFF);
    }

    #[test]
    fn apply_data_mask_only_touches_high_bits() {
        let mut buf = [0xFFu8, 0x80, 0x7F, 0x01];
        apply_data_mask(&mut buf, 0x7F);
        assert_eq!(buf, [0x7F, 0x00, 0x7F, 0x01]);
        let mut all = [0xA5u8; 4];
        apply_data_mask(&mut all, 0xFF);
        assert_eq!(all, [0xA5u8; 4]);
    }

    #[test]
    fn clamp_queue_size_bounds() {
        let max = 16 * 1024 - 1;
        // 0 means "no change" -> the maximum (idempotent for the caller).
        assert_eq!(clamp_queue_size(0, max), max);
        assert_eq!(clamp_queue_size(1, max), 1);
        assert_eq!(clamp_queue_size(1024, max), 1024);
        assert_eq!(clamp_queue_size(max, max), max);
        // Oversized requests are clamped down to the physical capacity.
        assert_eq!(clamp_queue_size(max + 1, max), max);
        assert_eq!(clamp_queue_size(u32::MAX, max), max);
        // A zero maximum can only produce zero.
        assert_eq!(clamp_queue_size(16, 0), 0);
    }

    #[test]
    fn xoff_state_unarmed_passes_everything() {
        let mut s = XoffState::default();
        assert_eq!(s.admit(0), 0);
        assert_eq!(s.admit(4096), 4096);
        assert!(!s.halted);
        assert_eq!(s.remaining, 0);
    }

    #[test]
    fn xoff_state_counts_down_then_halts() {
        let mut s = XoffState::default();
        s.on_set(5);
        assert_eq!(s.remaining, 5);
        assert!(!s.halted);

        assert_eq!(s.admit(3), 3);
        assert_eq!(s.remaining, 2);
        assert!(!s.halted);

        // Wants more than what is left: partial delivery, then halt.
        assert_eq!(s.admit(10), 2);
        assert_eq!(s.remaining, 0);
        assert!(s.halted);

        // Halted: nothing gets through until cleared.
        assert_eq!(s.admit(1), 0);
        assert!(s.halted);

        s.clear();
        assert_eq!(s.remaining, 0);
        assert!(!s.halted);
        assert_eq!(s.admit(7), 7);
    }

    #[test]
    fn xoff_state_set_zero_cancels_previous_counter() {
        let mut s = XoffState::default();
        s.on_set(4);
        assert_eq!(s.admit(4), 4);
        assert!(s.halted);
        // Re-arming with 0 cancels (and un-halts) the previous counter.
        s.on_set(0);
        assert_eq!(s.remaining, 0);
        assert!(!s.halted);
        assert_eq!(s.admit(16), 16);
    }

    #[test]
    fn xoff_state_admit_clamps_huge_requests() {
        let mut s = XoffState::default();
        s.on_set(3);
        // usize::MAX is larger than u32::MAX; must not wrap.
        assert_eq!(s.admit(usize::MAX), 3);
        assert!(s.halted);
    }

    #[test]
    fn ticks_constant_matches_100ns() {
        assert_eq!(TICKS_PER_SEC_100NS, 10_000_000);
        assert_eq!(MAX_PACER_BYTES, 1_048_576);
    }

    // ------------------------------------------------------------------
    // Adversarial tests. Each expectation below is computed by hand from the
    // UART / rate arithmetic rather than read off the implementation.
    // ------------------------------------------------------------------

    #[test]
    fn frame_half_bits_full_table_is_hand_computed() {
        // frame = 2 * (1 start + data + parity) + stop_half, stop_half being
        // 2 / 3 / 4 for 1 / 1.5 / 2 stop bits.
        for &wl in &[5u8, 6, 7, 8, 9] {
            for &par in &[0u8, 1] {
                let base = 2 * (1 + wl as u32 + par as u32);
                assert_eq!(frame_half_bits(wl, par, 0), base + 2, "1 stop wl={wl}");
                assert_eq!(frame_half_bits(wl, par, 1), base + 3, "1.5 stop wl={wl}");
                assert_eq!(frame_half_bits(wl, par, 2), base + 4, "2 stop wl={wl}");
                assert_eq!(frame_half_bits(wl, par, 7), base + 2, "fallback wl={wl}");
            }
        }
        // 1.5 stop bits must be exactly one half-bit more than 1 stop bit --
        // that is the whole reason half-bits exist.
        assert_eq!(frame_half_bits(8, 0, 1) - frame_half_bits(8, 0, 0), 1);
        assert_eq!(frame_half_bits(8, 0, 2) - frame_half_bits(8, 0, 1), 1);
        // 5N1 -> (1 + 5) * 2 + 2 = 14.
        assert_eq!(frame_half_bits(5, 0, 0), 14);
        // 9E1 -> (1 + 9 + 1) * 2 + 2 = 24.
        assert_eq!(frame_half_bits(9, 2, 0), 24);
    }

    #[test]
    fn pacer_one_second_delivers_baud_over_bits_per_frame() {
        // bytes/s = baud / bits_per_frame; the half-bit arithmetic must land on
        // the truncated integer in every case.
        let cases: [(u32, u8, u8, u8, u32); 5] = [
            (9600, 8, 0, 0, 960),       // 8N1 -> 10 bits/frame
            (115_200, 8, 0, 0, 11_520), // 8N1
            (9600, 7, 2, 0, 960),       // 7E1 -> also 10 bits/frame
            (9600, 8, 0, 2, 872),       // 8N2 -> 11 bits/frame; 9600/11 = 872.72
            (1200, 8, 1, 0, 109),       // 8O1 -> 11 bits/frame; 1200/11 = 109.09
        ];
        for (baud, wl, par, sb, expect) in cases {
            let mut p = LinePacer::default();
            let n = p.bytes_available(TICKS_PER_SEC_100NS, baud, frame_half_bits(wl, par, sb));
            assert_eq!(n, expect, "baud={baud} {wl}/{par}/{sb}");
        }
        // 8N1.5 at 9600: 19200 half-bits/s over a 21 half-bit frame = 914.28.
        let mut p = LinePacer::default();
        assert_eq!(
            p.bytes_available(TICKS_PER_SEC_100NS, 9600, frame_half_bits(8, 0, 1)),
            914
        );
    }

    #[test]
    fn pacer_time_going_backwards_neither_panics_nor_advances() {
        let mut p = LinePacer::default();
        p.reset(1_000_000);
        // Bank exactly two frames worth of carry.
        p.carry_half_bits = 2 * 20 * PACER_CARRY_SCALE;
        let n = p.bytes_available(1_000, 9600, 20);
        assert_eq!(n, 2);
        // `last` must NOT move backwards in time with the caller.
        assert_eq!(p.last_100ns, 1_000_000);
        assert_eq!(p.carry_half_bits, 0);
        // A second backwards call has nothing left and must not manufacture a
        // budget out of the (negative) elapsed time.
        assert_eq!(p.bytes_available(0, 9600, 20), 0);
        assert_eq!(p.last_100ns, 1_000_000);
        assert_eq!(p.carry_half_bits, 0);
    }

    #[test]
    fn pacer_chunked_calls_do_not_drift_from_a_single_call() {
        // This is the property the Q16 carry exists for: N small steps must
        // deliver (almost) the same total as one step of the same length.
        // An integer half-bit carry would truncate every sub-frame step to
        // zero and the total would collapse.
        let frame = frame_half_bits(8, 0, 0); // 20 half-bits
        for &(step_ticks, steps) in &[(520u64, 210u64), (100u64, 1_000u64), (1u64, 100_000u64)] {
            let mut chunked = LinePacer::default();
            let mut total: u128 = 0;
            for s in 1..=steps {
                total += chunked.bytes_available(s * step_ticks, 9600, frame) as u128;
            }
            let mut single = LinePacer::default();
            let expect = single.bytes_available(steps * step_ticks, 9600, frame) as u128;
            assert!(expect > 0, "sanity: step={step_ticks} yields nothing");
            let diff = total.abs_diff(expect);
            assert!(
                diff * 100 <= expect * 5,
                "drift too large for step={step_ticks}: chunked={total} single={expect}"
            );
        }
        // The headline case, spelled out: 210 x 52 us == 10.92 ms.
        let mut chunked = LinePacer::default();
        let mut total: u32 = 0;
        for s in 1..=210u64 {
            total += chunked.bytes_available(s * 520, 9600, frame);
        }
        let mut single = LinePacer::default();
        assert_eq!(single.bytes_available(210 * 520, 9600, frame), 10);
        assert_eq!(total, 10);
    }

    #[test]
    fn pacer_huge_elapsed_keeps_carry_bounded() {
        let mut p = LinePacer::default();
        p.reset(0);
        assert_eq!(p.bytes_available(u64::MAX, u32::MAX, 20), MAX_PACER_BYTES);
        // Whatever the elapsed time, the retained carry is < 1 frame.
        assert!(p.carry_half_bits < 20 * PACER_CARRY_SCALE);
        assert_eq!(p.last_100ns, u64::MAX);
        // A follow-up call at the same timestamp must not panic.
        assert_eq!(p.bytes_available(u64::MAX, u32::MAX, 20), 0);
    }

    #[test]
    fn pacer_zero_budget_never_blocks_the_caller_forever() {
        // `budget == u32::MAX` (baud == 0) is the "no limit" sentinel; the
        // caller clamps with `min`, so the important property is that the
        // value really is the maximum and that state stays untouched.
        let mut p = LinePacer::default();
        p.reset(123);
        assert_eq!(p.bytes_available(999, 0, 20), u32::MAX);
        assert_eq!(p.last_100ns, 123);
        assert_eq!(p.carry_half_bits, 0);
        // min(len, u32::MAX) == len for any legal write length.
        assert_eq!(4096usize.min(u32::MAX as usize), 4096);
    }

    #[test]
    fn line_noise_low_probability_is_sparse_but_hits_something() {
        // 1e-4 概率、4096 字节：期望约 0.4 次故障。用多个种子保证至少
        // 有一个种子命中，同时任何一次都不会把整块数据打满。
        let mut hits = 0;
        for seed in 1..=64u32 {
            let mut rng = Xorshift32::new(seed * 7919);
            let mut broke = 0u16;
            let mut buf = [0x00u8; 4096];
            let report = inject_line_noise(&mut rng, 10_000, 8, 1, &mut broke, &mut buf);
            assert!(
                report.corrupted + report.parity_errors + report.frame_errors < 4096,
                "低概率下错误过多: {report:?}"
            );
            if report.has_errors() {
                hits += 1;
            }
        }
        assert!(hits > 0, "64 个种子都没有命中 1e-4 概率");
    }

    #[test]
    fn line_noise_never_leaves_high_bits_set_after_mask() {
        // 上游顺序：先注入噪声，再套掩码。这里验证掩码确实能把噪声
        // 重新置高的位清掉，7 位字长下结果必须全部 <= 0x7F。
        let mut rng = Xorshift32::new(0xDEAD_BEEF);
        let mut broke = 0u16;
        let mut buf = [0xFFu8; 1024];
        let _ = inject_line_noise(
            &mut rng,
            NOISE_PROBABILITY_ONE / 2,
            7,
            1,
            &mut broke,
            &mut buf,
        );
        apply_data_mask(&mut buf, data_mask(false, 7, 8));
        assert!(buf.iter().all(|byte| *byte <= 0x7F));
    }

    #[test]
    fn line_noise_probability_boundaries_are_inclusive() {
        // probability == ONE 时每个字符都命中（排除坏字符运行后仍有故障）。
        let mut rng = Xorshift32::new(3);
        let mut broke = 0u16;
        let mut one = [0x00u8];
        let report = inject_line_noise(&mut rng, NOISE_PROBABILITY_ONE, 8, 0, &mut broke, &mut one);
        assert!(report.has_errors());
        // probability == 0 且无坏字符运行时不消耗 RNG。
        let mut rng = Xorshift32::new(3);
        let before = rng.0;
        let mut broke = 0u16;
        let mut buf = [0x00u8; 16];
        let report = inject_line_noise(&mut rng, 0, 8, 0, &mut broke, &mut buf);
        assert!(!report.has_errors());
        assert_eq!(rng.0, before);
    }

    #[test]
    fn xoff_state_exact_drain_latches_halt() {
        let mut s = XoffState::default();
        s.on_set(u32::MAX);
        assert_eq!(s.admit(10), 10);
        assert_eq!(s.remaining, u32::MAX - 10);
        assert!(!s.halted);
        // Draining exactly what is left must latch the halt (not one byte
        // later), otherwise the counter would over-deliver.
        assert_eq!(s.admit((u32::MAX - 10) as usize), (u32::MAX - 10) as usize);
        assert_eq!(s.remaining, 0);
        assert!(s.halted);
        assert_eq!(s.admit(1), 0);
        s.clear();
        assert_eq!(s.admit(usize::MAX), usize::MAX);
        assert!(!s.halted);
    }

    #[test]
    fn clamp_queue_size_is_idempotent_and_bounded() {
        let max = 16 * 1024 - 1;
        for &r in &[0u32, 1, 2, 200, max - 1, max, max + 1, u32::MAX] {
            let c = clamp_queue_size(r, max);
            assert!(c >= 1 && c <= max, "req={r} produced {c}");
            // Re-applying the clamp to its own output must be a no-op.
            assert_eq!(clamp_queue_size(c, max), c, "not idempotent for req={r}");
        }
    }
}
