//! CRC32C（Castagnoli）与 LevelDB 风格掩码。

/// CRC32C 反射多项式。
const POLY: u32 = 0x82F6_3B78;

/// 掩码常量。
const MASK_DELTA: u32 = 0xa282_ead8;

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = make_table();

/// 对多段数据连续计算 CRC32C（避免为拼接分配缓冲区）。
pub fn crc32c_many(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for part in parts {
        for &byte in *part {
            let index = ((crc ^ byte as u32) & 0xFF) as usize;
            crc = (crc >> 8) ^ TABLE[index];
        }
    }
    !crc
}

/// 对单段数据计算 CRC32C。
pub fn crc32c(data: &[u8]) -> u32 {
    crc32c_many(&[data])
}

/// 施加掩码，避免"数据尾部补零导致 CRC 不变"的漏检。
pub fn mask(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(MASK_DELTA)
}

/// 去除掩码。
pub fn unmask(masked: u32) -> u32 {
    masked.wrapping_sub(MASK_DELTA).rotate_left(15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn mask_roundtrip() {
        for crc in [0u32, 1, 0xFFFF_FFFF, 0xE306_9283] {
            assert_eq!(unmask(mask(crc)), crc);
        }
        assert_ne!(mask(0), 0);
    }

    #[test]
    fn many_matches_single() {
        assert_eq!(
            crc32c_many(&[&b"123"[..], &b"456"[..], &b"789"[..]]),
            crc32c(b"123456789")
        );
    }
}
