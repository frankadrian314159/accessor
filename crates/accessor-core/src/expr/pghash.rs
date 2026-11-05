//! PostgreSQL's hash functions (Bob Jenkins' lookup3, as `hash_bytes` has it), so that `hashtext(x)`, `hashint8(x)` and
//! the choice of a hash partition give the numbers PostgreSQL gives.

use crate::value::Value;

/// The seed hash partitioning hashes its key columns with.
pub(crate) const PARTITION_SEED: u64 = 0x7A5B_2236_7996_DCFD;

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(4);
    *b = b.wrapping_add(*a);
}

fn finish(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(14));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(11));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(25));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(16));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(4));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(14));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(24));
}

/// `hash_bytes_extended`: 64 bits, from `seed` (with a zero seed the low 32 bits are `hash_bytes`).
pub(crate) fn hash_bytes_extended(k: &[u8], seed: u64) -> u64 {
    let word = |s: &[u8]| u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16 | u32::from(s[3]) << 24;
    let init = 0x9e37_79b9u32.wrapping_add(k.len() as u32).wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (init, init, init);
    if seed != 0 {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix(&mut a, &mut b, &mut c);
    }
    let mut rest = k;
    while rest.len() >= 12 {
        a = a.wrapping_add(word(&rest[0..4]));
        b = b.wrapping_add(word(&rest[4..8]));
        c = c.wrapping_add(word(&rest[8..12]));
        mix(&mut a, &mut b, &mut c);
        rest = &rest[12..];
    }
    // the last 11 bytes; the lowest byte of c is kept for the length
    for (i, &byte) in rest.iter().enumerate() {
        let x = u32::from(byte);
        match i {
            0..=3 => a = a.wrapping_add(x << (8 * i)),
            4..=7 => b = b.wrapping_add(x << (8 * (i - 4))),
            _ => c = c.wrapping_add(x << (8 * (i - 8) + 8)),
        }
    }
    finish(&mut a, &mut b, &mut c);
    (u64::from(b) << 32) | u64::from(c)
}

pub(crate) fn hash_bytes(k: &[u8]) -> u32 {
    hash_bytes_extended(k, 0) as u32
}

/// `hash_uint32_extended`
pub(crate) fn hash_u32_extended(k: u32, seed: u64) -> u64 {
    let init = 0x9e37_79b9u32.wrapping_add(4).wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (init, init, init);
    if seed != 0 {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix(&mut a, &mut b, &mut c);
    }
    a = a.wrapping_add(k);
    finish(&mut a, &mut b, &mut c);
    (u64::from(b) << 32) | u64::from(c)
}

#[cfg(test)]
pub(crate) fn hash_u32(k: u32) -> u32 {
    hash_u32_extended(k, 0) as u32
}

/// `hashint8extended`, which agrees with the int2 and int4 hashes for the values they hold.
pub(crate) fn hash_i64_extended(v: i64, seed: u64) -> u64 {
    let (mut lo, hi) = (v as u32, (v >> 32) as u32);
    lo ^= if v >= 0 { hi } else { !hi };
    hash_u32_extended(lo, seed)
}

pub(crate) fn hash_i64(v: i64) -> u32 {
    hash_i64_extended(v, 0) as u32
}

/// The hash a value of a partition key column gets (64 bits, as `hashtextextended`, `hashint8extended`, ... give).
pub(crate) fn hash_value_extended(v: &Value, seed: u64) -> u64 {
    match v {
        Value::Int(i) => hash_i64_extended(*i, seed),
        Value::Bool(b) => hash_u32_extended(u32::from(*b), seed),
        Value::Text(s) => hash_bytes_extended(s.as_bytes(), seed),
        Value::Bytes(b) => hash_bytes_extended(b, seed),
        other => hash_bytes_extended(other.to_string().as_bytes(), seed),
    }
}

/// `hash_combine64`
pub(crate) fn combine64(mut a: u64, b: u64) -> u64 {
    a ^= b.wrapping_add(0x49a0_f4dd_15e5_a8e3).wrapping_add(a << 54).wrapping_add(a >> 7);
    a
}

/// The combined hash of a row's partition key values (a NULL contributes nothing).
pub(crate) fn partition_hash(keys: &[Value]) -> u64 {
    keys.iter().filter(|k| !k.is_null()).fold(0, |row, k| combine64(row, hash_value_extended(k, PARTITION_SEED)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_postgresql_hashes() {
        // select hashint4(1)
        assert_eq!(hash_u32(1) as i32, -1_905_060_026);
        assert_eq!(hash_i64(1), hash_u32(1));
        assert_eq!(hash_i64(-1), hash_u32(u32::MAX));
    }
}
