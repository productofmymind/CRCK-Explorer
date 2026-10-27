use aes::Aes256;
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};

use super::keys::GtaKeys;

pub fn decrypt_aes(data: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut buf = data.to_vec();
    let block_count = buf.len() / 16;
    for i in 0..block_count {
        let block = GenericArray::from_mut_slice(&mut buf[i * 16..(i + 1) * 16]);
        cipher.decrypt_block(block);
    }
    buf
}

pub fn encrypt_aes(data: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut buf = data.to_vec();
    let block_count = buf.len() / 16;
    for i in 0..block_count {
        let block = GenericArray::from_mut_slice(&mut buf[i * 16..(i + 1) * 16]);
        cipher.encrypt_block(block);
    }
    buf
}

pub fn get_ng_key_idx(name: &str, length: u32) -> usize {
    let hash = jenkins_hash(name);
    ((hash.wrapping_add(length).wrapping_add(101 - 40)) % 101) as usize
}

pub fn decrypt_ng(data: &[u8], keys: &GtaKeys, name: &str, length: u32) -> Vec<u8> {
    let key_idx = get_ng_key_idx(name, length);
    let key = &keys.ng_keys[key_idx];
    // CRCK Explorer patch: the 17 round keys are built once per call (they were a heap Vec built again for every 16-byte block).
    let word = |i: usize| u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    let mut sub_keys = [[0u32; 4]; 17];
    for (i, k) in sub_keys.iter_mut().enumerate() {
        *k = [word(4 * i), word(4 * i + 1), word(4 * i + 2), word(4 * i + 3)];
    }

    // the trailing partial block (if any) stays as it is
    let mut result = data.to_vec();
    for block in result.chunks_exact_mut(16) {
        let b: &mut [u8; 16] = block.try_into().unwrap();
        *b = decrypt_ng_block(b, &sub_keys, &keys.ng_decrypt_tables);
    }
    result
}

fn decrypt_ng_block(
    data: &[u8; 16],
    sub_keys: &[[u32; 4]; 17],
    tables: &[[[u32; 256]; 16]; 17],
) -> [u8; 16] {
    let mut buf = *data;
    buf = decrypt_ng_round_a(&buf, &sub_keys[0], &tables[0]);
    buf = decrypt_ng_round_a(&buf, &sub_keys[1], &tables[1]);
    for k in 2..=15 {
        buf = decrypt_ng_round_b(&buf, &sub_keys[k], &tables[k]);
    }
    buf = decrypt_ng_round_a(&buf, &sub_keys[16], &tables[16]);
    buf
}

fn decrypt_ng_round_a(data: &[u8; 16], key: &[u32; 4], table: &[[u32; 256]; 16]) -> [u8; 16] {
    let x1 = table[0][data[0] as usize]
        ^ table[1][data[1] as usize]
        ^ table[2][data[2] as usize]
        ^ table[3][data[3] as usize]
        ^ key[0];
    let x2 = table[4][data[4] as usize]
        ^ table[5][data[5] as usize]
        ^ table[6][data[6] as usize]
        ^ table[7][data[7] as usize]
        ^ key[1];
    let x3 = table[8][data[8] as usize]
        ^ table[9][data[9] as usize]
        ^ table[10][data[10] as usize]
        ^ table[11][data[11] as usize]
        ^ key[2];
    let x4 = table[12][data[12] as usize]
        ^ table[13][data[13] as usize]
        ^ table[14][data[14] as usize]
        ^ table[15][data[15] as usize]
        ^ key[3];

    let mut result = [0u8; 16];
    result[0..4].copy_from_slice(&x1.to_le_bytes());
    result[4..8].copy_from_slice(&x2.to_le_bytes());
    result[8..12].copy_from_slice(&x3.to_le_bytes());
    result[12..16].copy_from_slice(&x4.to_le_bytes());
    result
}

fn decrypt_ng_round_b(data: &[u8; 16], key: &[u32; 4], table: &[[u32; 256]; 16]) -> [u8; 16] {
    let x1 = table[0][data[0] as usize]
        ^ table[7][data[7] as usize]
        ^ table[10][data[10] as usize]
        ^ table[13][data[13] as usize]
        ^ key[0];
    let x2 = table[1][data[1] as usize]
        ^ table[4][data[4] as usize]
        ^ table[11][data[11] as usize]
        ^ table[14][data[14] as usize]
        ^ key[1];
    let x3 = table[2][data[2] as usize]
        ^ table[5][data[5] as usize]
        ^ table[8][data[8] as usize]
        ^ table[15][data[15] as usize]
        ^ key[2];
    let x4 = table[3][data[3] as usize]
        ^ table[6][data[6] as usize]
        ^ table[9][data[9] as usize]
        ^ table[12][data[12] as usize]
        ^ key[3];

    [
        (x1 >> 0) as u8, (x1 >> 8) as u8, (x1 >> 16) as u8, (x1 >> 24) as u8,
        (x2 >> 0) as u8, (x2 >> 8) as u8, (x2 >> 16) as u8, (x2 >> 24) as u8,
        (x3 >> 0) as u8, (x3 >> 8) as u8, (x3 >> 16) as u8, (x3 >> 24) as u8,
        (x4 >> 0) as u8, (x4 >> 8) as u8, (x4 >> 16) as u8, (x4 >> 24) as u8,
    ]
}

pub fn jenkins_hash(name: &str) -> u32 {
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut hash: u32 = 0;
    for &b in bytes {
        hash = hash.wrapping_add(b as u32);
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash = hash.wrapping_add(hash << 15);
    hash
}


#[cfg(test)]
mod tests {
    use super::*;

    /// the original per-block algorithm (round keys rebuilt for every block), kept as the reference
    fn reference(data: &[u8], keys: &GtaKeys, name: &str, length: u32) -> Vec<u8> {
        let key = &keys.ng_keys[get_ng_key_idx(name, length)];
        let k: Vec<u32> = key.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let mut out = data.to_vec();
        for (i, block) in data.chunks_exact(16).enumerate() {
            let sub: Vec<[u32; 4]> = (0..17).map(|j| [k[4 * j], k[4 * j + 1], k[4 * j + 2], k[4 * j + 3]]).collect();
            let t = &keys.ng_decrypt_tables;
            let mut b: [u8; 16] = block.try_into().unwrap();
            b = decrypt_ng_round_a(&b, &sub[0], &t[0]);
            b = decrypt_ng_round_a(&b, &sub[1], &t[1]);
            for r in 2..=15 {
                b = decrypt_ng_round_b(&b, &sub[r], &t[r]);
            }
            b = decrypt_ng_round_a(&b, &sub[16], &t[16]);
            out[i * 16..i * 16 + 16].copy_from_slice(&b);
        }
        out
    }

    #[test]
    fn ng_decrypt_matches_the_per_block_reference() {
        // deterministic pseudo-random keys and tables (no game keys needed)
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u32
        };
        let mut tables = Box::new([[[0u32; 256]; 16]; 17]);
        for t in tables.iter_mut().flatten().flatten() {
            *t = next();
        }
        let ng_keys = (0..101).map(|_| (0..272).map(|_| next() as u8).collect()).collect();
        let keys = GtaKeys { aes_key: [0; 32], ng_keys, ng_decrypt_tables: tables };
        for len in [0usize, 5, 16, 33, 4096 + 7] {
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            assert_eq!(decrypt_ng(&data, &keys, "x64e.rpf", len as u32), reference(&data, &keys, "x64e.rpf", len as u32), "length {len}");
        }
    }
}
