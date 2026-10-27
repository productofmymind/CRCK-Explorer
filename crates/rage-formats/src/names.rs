//! Names for the hashes in self-describing metadata. Structure, member and
//! enum names are stored as exact-case JOAAT hashes; this table turns them
//! back into text. The built-in list (`names/core.txt`) covers the map
//! formats this crate reads; anything else can be added at runtime from
//! files of one name per line, such as a list harvested from the game's own
//! XML metadata.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::hash::rage_joaat;

/// The built-in name list, one name per line, `#` comments allowed.
pub const CORE_NAMES: &str = include_str!("names/core.txt");

/// The structure, member, enum and value names the game hashes, as
/// The reference tool's MetaNames table lists them, each verified against its hash. These are the
/// schema names no archive scan can recover — `CVehicleModelColorIndices`
/// is an `itemType`, never a file — so they are built in.
pub const REFERENCE_NAMES: &str = include_str!("names/reference.txt");

/// The shader parameter names of the reference tool's `ShaderParamNames` enum, one per line in
/// file order. Their hashes are those of the *lowercased* name, so [`NameTable::core`]
/// loads them with [`NameTable::add_list_lowercase`].
pub const SHADER_PARAM_NAMES: &str = include_str!("names/shader_params.txt");

/// A hash → name lookup.
#[derive(Debug, Clone, Default)]
pub struct NameTable {
    map: HashMap<u32, String>,
}

impl NameTable {
    /// No names at all: every hash prints as `hash_XXXXXXXX`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The built-in names: the shader parameter names, this crate's own list, then the reference tool's.
    pub fn core() -> Self {
        let mut t = Self::default();
        // first: a lowercase spelling in the other lists must not hide the enum's own case
        t.add_list_lowercase(SHADER_PARAM_NAMES);
        t.add_list(CORE_NAMES);
        t.add_list(REFERENCE_NAMES);
        t
    }

    /// Like [`add_list`](Self::add_list) for names hashed lowercased (`GetHash(name.ToLowerInvariant())`):
    /// each is filed under the hash of its lowercase form but keeps its own case.
    pub fn add_list_lowercase(&mut self, text: &str) -> usize {
        let mut added = 0;
        for line in text.lines() {
            let name = line.trim();
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            if self.add_lowercase(name) {
                added += 1;
            }
        }
        added
    }

    /// Adds one name under the hash of its lowercase form. False when that hash was already present.
    pub fn add_lowercase(&mut self, name: &str) -> bool {
        let hash = rage_joaat(&name.to_lowercase());
        if self.map.contains_key(&hash) {
            return false;
        }
        self.map.insert(hash, name.to_owned());
        true
    }

    /// Adds every name in `text` (one per line; blank lines and `#`
    /// comments skipped). Returns how many names were new.
    pub fn add_list(&mut self, text: &str) -> usize {
        let mut added = 0;
        for line in text.lines() {
            let name = line.trim();
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            if self.add(name) {
                added += 1;
            }
        }
        added
    }

    /// Adds one name. False when its hash was already present.
    pub fn add(&mut self, name: &str) -> bool {
        let hash = rage_joaat(name);
        if self.map.contains_key(&hash) {
            return false;
        }
        self.map.insert(hash, name.to_owned());
        true
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The name of `hash`, if known.
    pub fn get(&self, hash: u32) -> Option<&str> {
        self.map.get(&hash).map(String::as_str)
    }

    /// The name of `hash`, or `hash_XXXXXXXX` when unknown (and `0` for a
    /// zero hash, which never names anything).
    pub fn resolve(&self, hash: u32) -> Cow<'_, str> {
        match self.get(hash) {
            Some(n) => Cow::Borrowed(n),
            None => Cow::Owned(unknown(hash)),
        }
    }
}

/// The placeholder for a hash with no known name.
pub fn unknown(hash: u32) -> String {
    if hash == 0 {
        "0".to_owned()
    } else {
        format!("hash_{hash:08X}")
    }
}

/// True when `s` is a placeholder [`unknown`] produced; its hash, if so.
pub fn parse_unknown(s: &str) -> Option<u32> {
    let hex = s.strip_prefix("hash_")?;
    (hex.len() == 8).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_names_include_the_structures_the_fixed_readers_use() {
        let t = NameTable::core();
        assert_eq!(t.get(3_545_841_574), Some("CMapData"));
        assert_eq!(t.get(3_461_354_627), Some("CEntityDef"));
        assert_eq!(t.get(164_374_718), Some("CMloInstanceDef"));
        assert_eq!(t.get(273_704_021), Some("CMloArchetypeDef"));
        assert_eq!(t.get(2_477_165_103), Some("CPackFileMetaData"));
        assert!(t.len() > 20_000);
        // Schema names that only a built-in table can supply.
        assert_eq!(t.get(rage_joaat("CVehicleModelColorIndices")), Some("CVehicleModelColorIndices"));
        assert_eq!(t.get(rage_joaat("CVehicleModelInfoVarGlobal")), Some("CVehicleModelInfoVarGlobal"));
    }

    #[test]
    fn every_reference_name_is_its_own_hash_source() {
        // The list is filtered on generation; this keeps it that way.
        for line in REFERENCE_NAMES.lines() {
            let name = line.trim();
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "{name:?}");
        }
        assert!(REFERENCE_NAMES.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).count() > 20_000);
    }

    #[test]
    fn unknown_hashes_print_as_placeholders_that_parse_back() {
        let t = NameTable::empty();
        assert_eq!(t.resolve(0xDEADBEEF), "hash_DEADBEEF");
        assert_eq!(t.resolve(0), "0");
        assert_eq!(parse_unknown("hash_DEADBEEF"), Some(0xDEADBEEF));
        assert_eq!(parse_unknown("CMapData"), None);
    }

    #[test]
    fn extra_lists_add_without_replacing() {
        let mut t = NameTable::core();
        assert_eq!(t.add_list("# comment\n\nCMapData\nMyOwnStruct\n"), 1);
        assert_eq!(t.get(rage_joaat("MyOwnStruct")), Some("MyOwnStruct"));
    }
}

#[cfg(test)]
mod shader_param_tests {
    use super::*;

    #[test]
    fn shader_parameter_names_match_references_enum_values() {
        for (name, value) in [("DiffuseSampler", 4_059_966_321u32), ("specularFactor", 376_311_761), ("BumpSampler", 1_186_448_975)] {
            assert_eq!(rage_joaat(&name.to_lowercase()), value, "{name}");
            assert_eq!(NameTable::core().get(value), Some(name));
        }
    }
}
