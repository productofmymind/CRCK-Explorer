# Patches to rage-formats 0.5.1 (Unlicense, VIRUXE/rage-formats)

Vendored so the Meta writer can be fixed for the archetype editor. Found by rebuilding every `.ytyp` of a retail install and comparing the trees.

1. **Writer, fixed-size float arrays** (`meta_write.rs`, `T_BYTE_ARRAY`): an array member with a float element type (`CLightAttrDef.posn`, `dir`, `tangent`, ... inside `CExtensionDefLightEffect`) was written through the integer path, so each float became the bits of its truncated integer (zeros, denormals, NaN). The reader was right. Now floats are written as floats. About 90 of the 2643 `.ytyp` files of a retail install lost light data before this (the interior files `v_int_*`).
2. **Reader, geometry bone palette** (`ydd.rs`): `DrawableGeometry` now carries `bone_ids` (the palette a skinned geometry's vertex blend indices point into; pointer at +0x68, count at +0x72, the same fields `blocks/drawable.rs` already reads). Added so the viewer can name a skinned geometry after the bone its vertices follow.
3. **Reader, blend indices stored as a Colour** (`vertex.rs`, `to_unified_vertices`): the indices were only taken when the component type was `UByte4`; vehicle geometries store them as `Colour`, so every vehicle vertex read as bone 0. Both types are the raw four bytes.

To upstream as PRs; drop this vendored copy once released.
