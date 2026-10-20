//! Pure, host-testable helpers for PnP instance/compatible IDs and COM names.
//!
//! These functions produce *only* byte/ASCII output and never reference `wdk`
//! or `windows` types, so the **exact same logic** that runs inside the kernel
//! drivers (`com0combus` PDO enumeration, `com0comport` COM naming) is
//! unit-tested on the host with a plain `cargo test`. Keeping the encoding in
//! one place removes the duplicated copies that used to live in
//! `com0combus/src/pdo.rs` and `com0comport/src/comname.rs`.

/// Max bytes for an instance ID buffer (`"c0c0_<hex8>_<A|B>"` plus NUL).
pub const INSTANCE_ID_MAX: usize = 32;
/// Max bytes for a compatible ID buffer (same shape, roomier prefix slot).
pub const COMPAT_ID_MAX: usize = 48;

/// Write the ASCII instance/compatible ID `"c0c0_<hex8>_<role>"` into `out`.
///
/// `role` is typically `b'A'` or `b'B'`. Returns the number of bytes written
/// (not counting any trailing NUL the caller may append). If `out` is too small
/// only as much as fits is written and the function returns `out.len()`, never
/// panicking or writing past the buffer.
pub fn build_instance_id(out: &mut [u8], pair_id: u32, role: u8) -> usize {
    let prefix = b"c0c0_";
    let mut i = 0;
    while i < prefix.len() && i < out.len() {
        out[i] = prefix[i];
        i += 1;
    }
    for k in (0..8).rev() {
        let nibble = ((pair_id >> (k * 4)) & 0xf) as u8;
        if i < out.len() {
            out[i] = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + (nibble - 10)
            };
            i += 1;
        }
    }
    if i < out.len() {
        out[i] = role;
        i += 1;
    }
    i
}

/// Compatible ID has the same shape as the instance ID (`"c0c0_<hex8>_<A|B>"`);
/// `port_index` 0 maps to `A`, 1 maps to `B`.
pub fn build_compat_id(out: &mut [u8], pair_id: u32, port_index: u8) -> usize {
    let role = if port_index == 0 { b'A' } else { b'B' };
    build_instance_id(out, pair_id, role)
}

/// Convert a `u32` to its decimal ASCII digits (most-significant first),
/// NUL-padded to 12 bytes. `0` yields `"0"`.
pub fn format_decimal(mut v: u32) -> [u8; 12] {
    let mut d = [0u8; 12];
    if v == 0 {
        d[0] = b'0';
        return d;
    }
    let mut i = 0;
    while v > 0 && i < 12 {
        d[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    // The digits were written least-significant first; reverse them.
    let mut j = 0;
    let mut k = i;
    while j < k {
        k -= 1;
        d.swap(j, k);
        j += 1;
    }
    d
}

/// Extract the leading ASCII bytes from a null-terminated wide (`u16`) string.
/// Stops at the first NUL or when the 32-byte destination is full; the result
/// is always NUL-terminated within the 32-byte buffer.
pub fn wide_to_ascii(wide: &[u16]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < wide.len() && i < out.len() - 1 {
        let c = wide[i];
        if c == 0 {
            break;
        }
        out[i] = c as u8;
        i += 1;
    }
    out
}

/// Encode ASCII `ascii` as UTF-16 into `out`, appending a NUL. Returns the
/// number of `u16` slots written (including the trailing NUL).
pub fn ascii_to_unicode_bytes(ascii: &[u8], out: &mut [u16]) -> usize {
    let mut i = 0;
    while i < ascii.len() && i < out.len() - 1 {
        out[i] = ascii[i] as u16;
        i += 1;
    }
    out[i] = 0;
    i + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_basic_shape_and_hex() {
        let mut buf = [0u8; 32];
        let n = build_instance_id(&mut buf, 0x0000_0042, b'B');
        assert_eq!(n, 14);
        assert_eq!(&buf[..n], b"c0c0_00000042B");
    }

    #[test]
    fn instance_id_lowercase_hex_for_digits_over_9() {
        // pair_id 0xABCDEF12 -> lowercase hex "abcdef12".
        let mut buf = [0u8; 32];
        let n = build_instance_id(&mut buf, 0xABCD_EF12, b'A');
        assert_eq!(&buf[..n], b"c0c0_abcdef12A");
    }

    #[test]
    fn instance_id_full_range_max_value() {
        let mut buf = [0u8; 32];
        let n = build_instance_id(&mut buf, 0xFFFF_FFFF, b'A');
        assert_eq!(&buf[..n], b"c0c0_ffffffffA");
    }

    #[test]
    fn instance_id_zero_pair_id() {
        let mut buf = [0u8; 32];
        let n = build_instance_id(&mut buf, 0, b'A');
        assert_eq!(&buf[..n], b"c0c0_00000000A");
    }

    #[test]
    fn compat_id_uses_port_role() {
        let mut buf = [0u8; 48];
        let a = build_compat_id(&mut buf, 1, 0);
        assert_eq!(&buf[..a], b"c0c0_00000001A");
        let b = build_compat_id(&mut buf, 1, 1);
        assert_eq!(&buf[..b], b"c0c0_00000001B");
    }

    #[test]
    fn instance_id_does_not_panic_on_tiny_buffer() {
        let mut buf = [0u8; 4];
        let n = build_instance_id(&mut buf, 0xABCD, b'A');
        assert!(n <= buf.len());
        // The prefix "c0c0" (4 bytes) must fit; the rest is truncated safely.
        assert_eq!(&buf[..n.min(4)], b"c0c0");
    }

    #[test]
    fn format_decimal_zero_and_values() {
        assert_eq!(format_decimal(0), [b'0', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(format_decimal(7), [b'7', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            format_decimal(123),
            [b'1', b'2', b'3', 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(format_decimal(4294967295), *b"4294967295\0\0");
    }

    #[test]
    fn wide_to_ascii_null_terminated() {
        let mut wide = [0u16; 32];
        for (i, c) in "COM15".encode_utf16().enumerate() {
            wide[i] = c;
        }
        let out = wide_to_ascii(&wide);
        assert_eq!(&out[..5], b"COM15");
        assert_eq!(out[5], 0);
    }

    #[test]
    fn wide_to_ascii_stops_at_nul() {
        let wide: [u16; 8] = [b'C' as u16, b'O' as u16, 0, b'X' as u16, 0, 0, 0, 0];
        let out = wide_to_ascii(&wide);
        assert_eq!(&out[..2], b"CO");
        assert_eq!(out[2], 0);
    }

    #[test]
    fn ascii_to_unicode_roundtrip() {
        let ascii = b"COM9";
        let mut buf = [0u16; 16];
        let n = ascii_to_unicode_bytes(ascii, &mut buf);
        assert_eq!(n, 5); // 4 chars + NUL
        assert_eq!(buf[0], b'C' as u16);
        assert_eq!(buf[4], 0);
        // no_std crate: no Vec in tests; use a fixed-size array.
        let back = [buf[0] as u8, buf[1] as u8, buf[2] as u8, buf[3] as u8];
        assert_eq!(&back, ascii);
    }
}
