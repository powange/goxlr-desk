//! The device validates an uploaded firmware chunk by chunk, each time returning a hash that is
//! fed back with the next chunk. It's a CRC-32 (IEEE polynomial, reflected) kept as a raw
//! register: starting from 0, without the usual inversion in and out. Found by comparing the
//! answers of a GoXLR during a real update (1.4.3.110, 625 chunks) with the usual candidates;
//! the whole file's CRC is the device's final hash.

/// The hash the device answers for `data`, continuing from `seed` (its previous answer, 0 for
/// the first chunk).
pub fn device_hash(seed: u32, data: &[u8]) -> u32 {
    let mut register = seed;
    for byte in data {
        register ^= *byte as u32;
        for _ in 0..8 {
            register = if register & 1 != 0 {
                (register >> 1) ^ 0xEDB8_8320
            } else {
                register >> 1
            };
        }
    }
    register
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(device_hash(0, b"123456789"), 0x2DFD_2D88);
        // Two answers of the GoXLR during the update: chunks 1 and 2 aren't known here, but the
        // chaining is: a hash continued from another is the hash of both parts.
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 3) as u8).collect();
        let first = device_hash(0, &data[..1024]);
        assert_eq!(device_hash(first, &data[1024..]), device_hash(0, &data));
    }
}
