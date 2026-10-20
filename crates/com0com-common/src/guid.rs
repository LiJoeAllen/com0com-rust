//! Pure conversion between the little-endian 16-byte GUID representation used
//! across `com0com-common` and the 4-field numeric form used by both
//! `windows_sys::core::GUID` (user mode) and `wdk_sys::GUID` (kernel mode).
//! Both consumers share the same C layout (`Data1`/`data1` u32, `Data2` u16,
//! `Data3` u16, `Data4` `[u8; 8]`), so one host-tested conversion serves both.

/// The 4 numeric components of a GUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuidParts {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// Interpret 16 little-endian GUID bytes as the 4 numeric components.
pub fn guid_parts(b: &[u8; 16]) -> GuidParts {
    GuidParts {
        data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        data2: u16::from_le_bytes([b[4], b[5]]),
        data3: u16::from_le_bytes([b[6], b[7]]),
        data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
    }
}

/// Serialize the 4 numeric components back to 16 little-endian GUID bytes.
pub fn guid_bytes(p: &GuidParts) -> [u8; 16] {
    let d1 = p.data1.to_le_bytes();
    let d2 = p.data2.to_le_bytes();
    let d3 = p.data3.to_le_bytes();
    [
        d1[0], d1[1], d1[2], d1[3], d2[0], d2[1], d3[0], d3[1], p.data4[0], p.data4[1], p.data4[2],
        p.data4[3], p.data4[4], p.data4[5], p.data4[6], p.data4[7],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{GUID_COM0COM_BUS_BYTES, GUID_DEVINTERFACE_COMPORT};

    #[test]
    fn bus_guid_parts_are_correct() {
        // GUID_COM0COM_BUS = 900C0CC0-2211-4433-5566-778899AABBCC
        let p = guid_parts(&GUID_COM0COM_BUS_BYTES);
        assert_eq!(p.data1, 0x900C_0CC0);
        assert_eq!(p.data2, 0x2211);
        assert_eq!(p.data3, 0x4433);
        assert_eq!(p.data4, [0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC]);
        assert_eq!(guid_bytes(&p), GUID_COM0COM_BUS_BYTES);
    }

    #[test]
    fn comport_guid_parts_are_correct() {
        // GUID_DEVINTERFACE_COMPORT = 86E0D1E0-8089-D011-9CE4-08003E301F73
        let p = guid_parts(&GUID_DEVINTERFACE_COMPORT);
        assert_eq!(p.data1, 0x86E0_D1E0);
        assert_eq!(p.data2, 0x8089);
        assert_eq!(p.data3, 0xD011);
        assert_eq!(p.data4, [0x9C, 0xE4, 0x08, 0x00, 0x3E, 0x30, 0x1F, 0x73]);
        assert_eq!(guid_bytes(&p), GUID_DEVINTERFACE_COMPORT);
    }

    #[test]
    fn guid_bytes_roundtrip_arbitrary() {
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let p = guid_parts(&bytes);
        assert_eq!(guid_bytes(&p), bytes);
        assert_eq!(guid_parts(&guid_bytes(&p)), p);
    }
}
