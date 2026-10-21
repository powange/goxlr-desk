//! The device validates an uploaded firmware chunk by chunk, each time returning a 32 bit hash
//! that is fed back with the next chunk. How it computes it isn't documented, so the daemon
//! can't check it yet. These are the usual candidates, computed on the same bytes from the same
//! seed: the update logs which ones give the device's answer, so the right one can be found
//! and then checked for real.

/// A reflected CRC-32 register (polynomial given reversed), from `register`, without final xor.
fn crc32_reflected(poly: u32, mut register: u32, data: &[u8]) -> u32 {
    for byte in data {
        register ^= *byte as u32;
        for _ in 0..8 {
            register = if register & 1 != 0 {
                (register >> 1) ^ poly
            } else {
                register >> 1
            };
        }
    }
    register
}

/// A CRC-32 register shifted MSB first (BZIP2 / MPEG-2 polynomial), without final xor.
fn crc32_msb(mut register: u32, data: &[u8]) -> u32 {
    for byte in data {
        register ^= (*byte as u32) << 24;
        for _ in 0..8 {
            register = if register & 0x8000_0000 != 0 {
                (register << 1) ^ 0x04C1_1DB7
            } else {
                register << 1
            };
        }
    }
    register
}

fn adler32(seed: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (seed & 0xffff, seed >> 16);
    if seed == 0 {
        a = 1;
    }
    for byte in data {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

const IEEE: u32 = 0xEDB8_8320;
const CASTAGNOLI: u32 = 0x82F6_3B78;

/// Each candidate's hash of `data`, continuing from `seed` (the device's previous answer, 0 for
/// the first chunk). A CRC is chained either as a finished value (inverted in and out, as zlib
/// does) or as a raw register (starting from 0xFFFFFFFF or from 0).
pub fn candidates(seed: u32, data: &[u8]) -> Vec<(&'static str, u32)> {
    let first = seed == 0;
    let raw_start = |start: u32| if first { start } else { seed };
    let sum_bytes = data
        .iter()
        .fold(seed, |sum, byte| sum.wrapping_add(*byte as u32));
    let sum_words = data
        .chunks(4)
        .map(|word| {
            let mut bytes = [0u8; 4];
            bytes[..word.len()].copy_from_slice(word);
            u32::from_le_bytes(bytes)
        })
        .fold(seed, |sum, word| sum.wrapping_add(word));
    vec![
        ("crc32", !crc32_reflected(IEEE, !seed, data)),
        (
            "crc32-raw-ffffffff",
            crc32_reflected(IEEE, raw_start(0xFFFF_FFFF), data),
        ),
        ("crc32-raw-0", crc32_reflected(IEEE, raw_start(0), data)),
        ("crc32c", !crc32_reflected(CASTAGNOLI, !seed, data)),
        (
            "crc32c-raw-ffffffff",
            crc32_reflected(CASTAGNOLI, raw_start(0xFFFF_FFFF), data),
        ),
        ("crc32-bzip2", !crc32_msb(!seed, data)),
        ("crc32-mpeg2", crc32_msb(raw_start(0xFFFF_FFFF), data)),
        ("crc32-msb-raw-0", crc32_msb(raw_start(0), data)),
        ("adler32", adler32(seed, data)),
        ("sum-bytes", sum_bytes),
        ("sum-words-le", sum_words),
    ]
}

/// The candidates giving `answer`.
pub fn matching(seed: u32, data: &[u8], answer: u32) -> Vec<&'static str> {
    candidates(seed, data)
        .into_iter()
        .filter(|(_, hash)| *hash == answer)
        .map(|(name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(name: &str, seed: u32, data: &[u8]) -> u32 {
        candidates(seed, data)
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap()
            .1
    }

    #[test]
    fn the_candidates_give_their_published_check_values() {
        let data = b"123456789";
        assert_eq!(hash("crc32", 0, data), 0xCBF4_3926);
        assert_eq!(hash("crc32c", 0, data), 0xE306_9283);
        assert_eq!(hash("crc32-bzip2", 0, data), 0xFC89_1918);
        assert_eq!(hash("crc32-mpeg2", 0, data), 0x0376_E6E7);
        assert_eq!(hash("adler32", 0, data), 0x091E_01DE);
    }

    #[test]
    fn chained_chunks_give_the_whole_hash() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 + 3) as u8).collect();
        for name in [
            "crc32",
            "crc32-raw-ffffffff",
            "crc32c",
            "crc32-bzip2",
            "crc32-mpeg2",
            "adler32",
            "sum-bytes",
        ] {
            let whole = hash(name, 0, &data);
            let first = hash(name, 0, &data[..1200]);
            let second = hash(name, first, &data[1200..]);
            assert_eq!(second, whole, "{name}");
        }
    }
}
