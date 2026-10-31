//! The byte-oriented MySQL functions: `AES_ENCRYPT`/`AES_DECRYPT`, `CRC32`, `COMPRESS`/`UNCOMPRESS`, the UUID
//! converters and the `INET6_*` functions.

/// CRC-32 (IEEE) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1B } else { 0 }
}

fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

fn sboxes() -> ([u8; 256], [u8; 256]) {
    let mut sbox = [0u8; 256];
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1B } else { 0 };
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    let mut inv = [0u8; 256];
    for (i, s) in sbox.iter().enumerate() {
        inv[*s as usize] = i as u8;
    }
    (sbox, inv)
}

/// AES with a 128, 192 or 256-bit key.
pub struct Aes {
    rounds: usize,
    keys: Vec<[u8; 16]>,
    sbox: [u8; 256],
    inv: [u8; 256],
}

impl Aes {
    pub fn new(key: &[u8]) -> Aes {
        let (sbox, inv) = sboxes();
        let nk = key.len() / 4;
        let rounds = nk + 6;
        let mut w: Vec<[u8; 4]> = key.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t.rotate_left(1);
                t = t.map(|b| sbox[b as usize]);
                t[0] ^= rcon;
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| sbox[b as usize]);
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let keys = w.chunks(4).map(|c| std::array::from_fn(|i| c[i / 4][i % 4])).collect();
        Aes { rounds, keys, sbox, inv }
    }

    fn add_key(&self, s: &mut [u8; 16], r: usize) {
        for (b, k) in s.iter_mut().zip(&self.keys[r]) {
            *b ^= k;
        }
    }

    fn shift_rows(s: &mut [u8; 16], inverse: bool) {
        let old = *s;
        for c in 0..4 {
            for r in 0..4 {
                if inverse {
                    s[((c + r) % 4) * 4 + r] = old[c * 4 + r];
                } else {
                    s[c * 4 + r] = old[((c + r) % 4) * 4 + r];
                }
            }
        }
    }

    fn mix(s: &mut [u8; 16], inverse: bool) {
        let m: [u8; 4] = if inverse { [14, 11, 13, 9] } else { [2, 3, 1, 1] };
        for c in 0..4 {
            let col = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
            for r in 0..4 {
                s[c * 4 + r] = (0..4).fold(0, |acc, k| acc ^ gmul(col[k], m[(k + 4 - r) % 4]));
            }
        }
    }

    pub fn encrypt_block(&self, s: &mut [u8; 16]) {
        self.add_key(s, 0);
        for r in 1..self.rounds {
            s.iter_mut().for_each(|b| *b = self.sbox[*b as usize]);
            Self::shift_rows(s, false);
            Self::mix(s, false);
            self.add_key(s, r);
        }
        s.iter_mut().for_each(|b| *b = self.sbox[*b as usize]);
        Self::shift_rows(s, false);
        self.add_key(s, self.rounds);
    }

    pub fn decrypt_block(&self, s: &mut [u8; 16]) {
        self.add_key(s, self.rounds);
        for r in (1..self.rounds).rev() {
            Self::shift_rows(s, true);
            s.iter_mut().for_each(|b| *b = self.inv[*b as usize]);
            self.add_key(s, r);
            Self::mix(s, true);
        }
        Self::shift_rows(s, true);
        s.iter_mut().for_each(|b| *b = self.inv[*b as usize]);
        self.add_key(s, 0);
    }
}

/// MySQL's key: the bytes folded (by xor) onto a key of `size` bytes.
pub fn fold_key(key: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for (i, b) in key.iter().enumerate() {
        out[i % size] ^= b;
    }
    out
}

/// `AES_ENCRYPT` with PKCS7 padding; ECB, or CBC when an initialisation vector is given.
pub fn aes_encrypt(data: &[u8], key: &[u8], iv: Option<&[u8]>, bits: usize) -> Vec<u8> {
    let aes = Aes::new(&fold_key(key, bits / 8));
    let pad = 16 - data.len() % 16;
    let mut buf = data.to_vec();
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    let mut prev: [u8; 16] = [0; 16];
    if let Some(iv) = iv {
        for (p, b) in prev.iter_mut().zip(iv) {
            *p = *b;
        }
    }
    let mut out = Vec::with_capacity(buf.len());
    for chunk in buf.chunks(16) {
        let mut block: [u8; 16] = chunk.try_into().expect("a whole block");
        if iv.is_some() {
            block.iter_mut().zip(prev).for_each(|(b, p)| *b ^= p);
        }
        aes.encrypt_block(&mut block);
        prev = block;
        out.extend_from_slice(&block);
    }
    out
}

/// `AES_DECRYPT`; `None` when the data or the padding is not right.
pub fn aes_decrypt(data: &[u8], key: &[u8], iv: Option<&[u8]>, bits: usize) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let aes = Aes::new(&fold_key(key, bits / 8));
    let mut prev: [u8; 16] = [0; 16];
    if let Some(iv) = iv {
        for (p, b) in prev.iter_mut().zip(iv) {
            *p = *b;
        }
    }
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let cipher: [u8; 16] = chunk.try_into().expect("a whole block");
        let mut block = cipher;
        aes.decrypt_block(&mut block);
        if iv.is_some() {
            block.iter_mut().zip(prev).for_each(|(b, p)| *b ^= p);
        }
        prev = cipher;
        out.extend_from_slice(&block);
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || pad > out.len() || out[out.len() - pad..].iter().any(|b| *b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// `COMPRESS`: the length (4 bytes, little-endian) and a zlib stream (of stored blocks).
pub fn compress(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut out = (data.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&[0x78, 0x01]);
    let mut chunks = data.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        out.extend_from_slice(&(c.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// `UNCOMPRESS` of what [`compress`] wrote; `None` for anything else.
pub fn uncompress(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return Some(Vec::new());
    }
    if data.len() < 6 {
        return None;
    }
    let want = u32::from_le_bytes(data[..4].try_into().ok()?) as usize;
    let mut i = 6;
    let mut out = Vec::new();
    loop {
        let last = *data.get(i)? & 1 == 1;
        if *data.get(i)? >> 1 & 3 != 0 {
            return None;
        }
        let len = u16::from_le_bytes([*data.get(i + 1)?, *data.get(i + 2)?]) as usize;
        out.extend_from_slice(data.get(i + 5..i + 5 + len)?);
        i += 5 + len;
        if last {
            break;
        }
    }
    (out.len() == want).then_some(out)
}

/// The 16 bytes of a UUID text (with or without dashes and braces).
pub fn uuid_bytes(s: &str) -> Option<[u8; 16]> {
    let t = s.trim();
    let t = t.strip_prefix('{').and_then(|x| x.strip_suffix('}')).unwrap_or(t);
    let hex: String = if t.len() == 36 {
        let b = t.as_bytes();
        if [8, 13, 18, 23].iter().any(|&i| b[i] != b'-') {
            return None;
        }
        t.chars().filter(|c| *c != '-').collect()
    } else if t.len() == 32 {
        t.to_string()
    } else {
        return None;
    };
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// `UUID_TO_BIN(uuid, swap)`: with `swap` the time-low part moves behind the time-high part.
pub fn uuid_to_bin(b: [u8; 16], swap: bool) -> [u8; 16] {
    if !swap {
        return b;
    }
    let mut o = [0u8; 16];
    o[0..2].copy_from_slice(&b[6..8]);
    o[2..4].copy_from_slice(&b[4..6]);
    o[4..8].copy_from_slice(&b[0..4]);
    o[8..].copy_from_slice(&b[8..]);
    o
}

/// `BIN_TO_UUID(bin, swap)`.
pub fn bin_to_uuid(b: &[u8], swap: bool) -> Option<String> {
    let b: [u8; 16] = b.try_into().ok()?;
    let o = if swap {
        let mut o = [0u8; 16];
        o[0..4].copy_from_slice(&b[4..8]);
        o[4..6].copy_from_slice(&b[2..4]);
        o[6..8].copy_from_slice(&b[0..2]);
        o[8..].copy_from_slice(&b[8..]);
        o
    } else {
        b
    };
    let h: String = o.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_matches_the_standard_vectors() {
        let key: Vec<u8> = (0u8..16).collect();
        let plain: Vec<u8> = (0u8..16).map(|i| i * 0x11).collect();
        let aes = Aes::new(&key);
        let mut b: [u8; 16] = plain.clone().try_into().unwrap();
        aes.encrypt_block(&mut b);
        assert_eq!(b.iter().map(|x| format!("{x:02x}")).collect::<String>(), "69c4e0d86a7b0430d8cdb78070b4c55a");
        aes.decrypt_block(&mut b);
        assert_eq!(b.to_vec(), plain);
        let key256: Vec<u8> = (0u8..32).collect();
        let mut b: [u8; 16] = plain.try_into().unwrap();
        Aes::new(&key256).encrypt_block(&mut b);
        assert_eq!(b.iter().map(|x| format!("{x:02x}")).collect::<String>(), "8ea2b7ca516745bfeafc49904b496089");
    }

    #[test]
    fn crc32_and_compress_round_trip() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(uncompress(&compress(b"hello hello")).as_deref(), Some(&b"hello hello"[..]));
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
