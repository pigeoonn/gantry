//! MD5 and SHA-256 of a file, for checking that a download is the file the site published. MD5 is
//! not a defence against an attacker (Nexus only publishes MD5); it catches damaged and swapped downloads.

use std::io::{self, Read};

fn feed(r: &mut impl Read, mut block: impl FnMut(&[u8; 64]), pad_big_endian: bool) -> io::Result<()> {
    let mut buf = [0u8; 64];
    let mut filled = 0usize;
    let mut total: u64 = 0;
    let mut chunk = [0u8; 65536];
    loop {
        let n = r.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        let mut data = &chunk[..n];
        while !data.is_empty() {
            let take = (64 - filled).min(data.len());
            buf[filled..filled + take].copy_from_slice(&data[..take]);
            filled += take;
            data = &data[take..];
            if filled == 64 {
                block(&buf);
                filled = 0;
            }
        }
    }
    // padding: 0x80, zeros, then the length in bits (little endian for MD5, big endian for SHA-256)
    buf[filled] = 0x80;
    filled += 1;
    if filled > 56 {
        buf[filled..].fill(0);
        block(&buf);
        filled = 0;
    }
    buf[filled..56].fill(0);
    let bits = total.wrapping_mul(8);
    buf[56..].copy_from_slice(&if pad_big_endian { bits.to_be_bytes() } else { bits.to_le_bytes() });
    block(&buf);
    Ok(())
}

pub fn md5_hex(r: &mut impl Read) -> io::Result<String> {
    const S: [u32; 64] = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
    let mut k = [0u32; 64];
    for (i, v) in k.iter_mut().enumerate() {
        *v = ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32;
    }
    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    feed(
        r,
        |b| {
            let mut m = [0u32; 16];
            for (w, c) in m.iter_mut().zip(b.as_chunks::<4>().0) {
                *w = u32::from_le_bytes(*c);
            }
            let [mut a, mut bb, mut c, mut d] = h;
            for i in 0..64 {
                let (f, g) = match i / 16 {
                    0 => ((bb & c) | (!bb & d), i),
                    1 => ((d & bb) | (!d & c), (5 * i + 1) % 16),
                    2 => (bb ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (bb | !d), (7 * i) % 16),
                };
                let t = d;
                d = c;
                c = bb;
                bb = bb.wrapping_add(a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]).rotate_left(S[i]));
                a = t;
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(bb);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
        },
        false,
    )?;
    Ok(h.iter().flat_map(|x| x.to_le_bytes()).map(|b| format!("{b:02x}")).collect())
}

pub fn sha256_hex(r: &mut impl Read) -> io::Result<String> {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    feed(
        r,
        |b| {
            let mut w = [0u32; 64];
            for (i, c) in b.chunks(4).enumerate() {
                w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for i in 0..8 {
                h[i] = h[i].wrapping_add(v[i]);
            }
        },
        true,
    )?;
    Ok(h.iter().flat_map(|x| x.to_be_bytes()).map(|b| format!("{b:02x}")).collect())
}

pub fn md5_file(p: &std::path::Path) -> io::Result<String> {
    md5_hex(&mut std::fs::File::open(p)?)
}

pub fn sha256_file(p: &std::path::Path) -> io::Result<String> {
    sha256_hex(&mut std::fs::File::open(p)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md5(s: &[u8]) -> String {
        md5_hex(&mut &s[..]).unwrap()
    }
    fn sha(s: &[u8]) -> String {
        sha256_hex(&mut &s[..]).unwrap()
    }

    #[test]
    fn md5_matches_the_published_test_vectors() {
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"), "57edf4a22be3c955ac49da2e2107b67a");
    }

    #[test]
    fn sha256_matches_the_published_test_vectors() {
        assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    }

    #[test]
    fn lengths_around_the_block_size_and_big_inputs_are_handled() {
        for n in [55usize, 56, 57, 63, 64, 65, 119, 120, 128] {
            let data = vec![b'a'; n];
            // the same data in tiny pieces gives the same answer
            struct Dribble<'a>(&'a [u8]);
            impl Read for Dribble<'_> {
                fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                    let k = self.0.len().min(out.len()).min(7);
                    out[..k].copy_from_slice(&self.0[..k]);
                    self.0 = &self.0[k..];
                    Ok(k)
                }
            }
            assert_eq!(md5_hex(&mut Dribble(&data)).unwrap(), md5(&data), "md5 {n}");
            assert_eq!(sha256_hex(&mut Dribble(&data)).unwrap(), sha(&data), "sha {n}");
        }
        // a million 'a' (the classic long vector)
        let million = vec![b'a'; 1_000_000];
        assert_eq!(md5(&million), "7707d6ae4e027c70eea2a935c2296f21");
        assert_eq!(sha(&million), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }
}
