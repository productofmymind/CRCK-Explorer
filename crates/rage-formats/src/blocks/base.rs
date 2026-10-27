use super::*;

pub struct StringBlock(pub String);
impl Block for StringBlock {
    fn length(&self) -> usize { self.0.len() + 1 }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> { w.bytes(self.0.as_bytes()); w.u8(0); Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

pub struct StructArray<T: Pod> { pub items: Vec<T> }
impl<T: Pod + 'static> Block for StructArray<T> {
    fn length(&self) -> usize { self.items.len() * T::SIZE }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> { for i in &self.items { i.write(w) } Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

pub struct RawBytes { pub data: Vec<u8>, pub section: Section }
impl Block for RawBytes {
    fn length(&self) -> usize { self.data.len() }
    fn section(&self) -> Section { self.section }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> { w.bytes(&self.data); Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `ResourcePointerArray64<T>`: 8 bytes per entry, a 0 for a null entry.
pub struct PointerArray64 { pub items: Vec<Option<BlockId>> }
impl Block for PointerArray64 {
    fn length(&self) -> usize { 8 * self.items.len() }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.items.iter().flatten().copied().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> { for i in &self.items { w.u64(g.ptr(*i)) } Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `ResourcePagesInfo` (ResourceFile.cs:95): sized for 128 pages while the
/// layout is computed, written with the real counts (`ResourceBuilder.Build`).
pub struct PagesInfo { pub system_pages: u8, pub graphics_pages: u8, capacity: usize }
impl Default for PagesInfo { fn default() -> Self { Self { system_pages: 128, graphics_pages: 0, capacity: 128 } } }
impl Block for PagesInfo {
    fn length(&self) -> usize { 16 + 8 * self.capacity }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> {
        w.u32(0); w.u32(0); w.u8(self.system_pages); w.u8(self.graphics_pages); w.u16(0); w.u32(0);
        w.zeros(8 * self.capacity); Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `ResourceFileBase.Write` (ResourceFile.cs:38): VFT, 1, pages-info pointer.
pub fn write_file_base(w: &mut Writer, g: &Graph, vft: u32, pages_info: Option<BlockId>) { w.u32(vft); w.u32(1); w.u64(g.ptr(pages_info)); }
/// `ResourceSimpleList64<T>` / `_s` / `_uint` (ResourceBaseTypes.cs:739-): pointer, count u16, capacity u16, 4 zero bytes.
/// `what` names the entries for the error when there are more than 65535.
pub fn write_simple_list64(w: &mut Writer, g: &Graph, data: Option<BlockId>, count: usize, what: &str) -> Result<()> {
    let n = count_u16(count, what)?;
    w.u64(g.ptr(data)); w.u16(n); w.u16(n); w.u32(0);
    Ok(())
}
/// `ResourceSimpleList64b_s<T>` (ResourceBaseTypes.cs:912): pointer, count u32, capacity u32.
pub fn write_simple_list64b(w: &mut Writer, g: &Graph, data: Option<BlockId>, count: u32, capacity: u32) { w.u64(g.ptr(data)); w.u32(count); w.u32(capacity); }
/// `ResourcePointerList64<T>` (ResourceBaseTypes.cs:1562): same 16-byte header over a `PointerArray64`.
pub fn write_pointer_list64(w: &mut Writer, g: &Graph, array: Option<BlockId>, count: usize, what: &str) -> Result<()> { write_simple_list64(w, g, array, count, what) }

/// `ResourceSimpleList64<T>` header: pointer, count, capacity (4 bytes of padding skipped).
pub fn read_simple_list64(c: &mut Cursor) -> (u64, u16, u16) { let p = c.u64(); let n = c.u16(); let cap = c.u16(); c.skip(4); (p, n, cap) }
/// `ResourceSimpleList64b_s<T>` header: pointer, count u32, capacity u32.
pub fn read_simple_list64b(c: &mut Cursor) -> (u64, u32, u32) { let p = c.u64(); let n = c.u32(); let cap = c.u32(); (p, n, cap) }
/// The raw pointers of a `ResourcePointerArray64`; the caller resolves each through the pool.
pub fn read_pointer_array64(r: &mut Reader, _g: &mut Graph, va: u64, n: usize) -> Result<Vec<u64>> { r.u64s(va, n) }
pub fn read_string_block(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
    if va == 0 { return Ok(None); }
    if let Some(id) = r.cached_as::<StringBlock>(va)? { return Ok(Some(id)); }
    let s = r.string(va)?.unwrap();
    let id = g.add(StringBlock(s)); r.cache::<StringBlock>(va, id); Ok(Some(id))
}
/// A plain array: at an address already read as a block of another type (a stray pointer, often of an
/// empty array, that some modded files carry) it is read again as its own block and left out of the pool,
/// as `ResourceDataReader.ReadBlock` does, rather than refused.
pub fn read_struct_array<T: Pod + 'static>(r: &mut Reader, g: &mut Graph, va: u64, n: usize) -> Result<Option<BlockId>> {
    if va == 0 { return Ok(None); }
    let pool = match r.cached_as::<StructArray<T>>(va) {
        Ok(Some(id)) => return Ok(Some(id)),
        Ok(None) => true,
        Err(_) => false,
    };
    let items = r.structs::<T>(va, n)?;
    let id = g.add(StructArray { items });
    if pool { r.cache::<StructArray<T>>(va, id); }
    Ok(Some(id))
}
pub fn read_pages_info(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
    if va == 0 { return Ok(None); }
    if let Some(id) = r.cached_as::<PagesInfo>(va)? { return Ok(Some(id)); }
    let mut c = r.cursor(va)?; c.skip(8);
    let (system_pages, graphics_pages) = (c.u8(), c.u8()); c.check()?;
    let id = g.add(PagesInfo { system_pages, graphics_pages, capacity: 128 }); r.cache::<PagesInfo>(va, id); Ok(Some(id))
}
