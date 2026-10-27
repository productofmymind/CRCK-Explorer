//! RAGE's Jenkins one-at-a-time hash, the key space every name in these
//! formats is looked up by (texture names, archetype names, shader names…).
//! Callers lowercase first where the game does; this function hashes the
//! bytes exactly as given.

/// Jenkins one-at-a-time over the bytes of `s`, exactly as RAGE computes it.
pub fn rage_joaat(s: &str) -> u32 {
    let mut hash: u32 = 0;
    for b in s.bytes() {
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
    use super::rage_joaat;

    #[test]
    fn matches_known_hashes() {
        // Same value ytyp.rs checks for the CMloArchetypeDef structure hash.
        assert_eq!(rage_joaat("CMloArchetypeDef"), 273_704_021);
        assert_eq!(rage_joaat(""), 0);
    }
}
