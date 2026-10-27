//! The bounding-volume hierarchy of a `BoundBVH` or a large `BoundComposite`, ported from the reference tool's
//! `Bounds.cs`: the `BVH` block (128 bytes), `BVHNode_s` and `BVHTreeInfo_s` (16 bytes each), and
//! `BVHBuilder` with `BVHBuilderNode` (`Build`, `UpdateMinMax`, `GatherNodes`, `GatherTrees`).
//!
//! The builder is a port, not a reimplementation: the game walks the nodes in the order the reference tool lays
//! them out, so the split axis, the tie rules, the child order and the tree cut must all agree.

use anyhow::Result;

use super::base::{read_simple_list64, read_simple_list64b, read_struct_array, write_simple_list64, write_simple_list64b, StructArray};
use super::bounds::{vmax, vmin};
use super::*;

/// `BVHBuilder.MaxTreeNodeCount`: the most nodes any tree holds.
const MAX_TREE_NODES: usize = 127;

/// `BVHNode_s`: a node's box in units of the BVH's quantum, then its item id and count. A leaf has
/// `item_count` items starting at `item_id`; an inner node has `item_count == 0` and `item_id` = its node count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BvhNode { pub min: [i16; 3], pub max: [i16; 3], pub item_id: i16, pub item_count: i16 }

impl Pod for BvhNode {
    const SIZE: usize = 16;
    fn write(&self, w: &mut Writer) { for v in self.min.into_iter().chain(self.max) { w.i16(v); } w.i16(self.item_id); w.i16(self.item_count); }
    fn read(b: &[u8]) -> Self {
        let at = |i: usize| i16::read(&b[2 * i..]);
        BvhNode { min: [at(0), at(1), at(2)], max: [at(3), at(4), at(5)], item_id: at(6), item_count: at(7) }
    }
}

/// `BVHTreeInfo_s`: a tree's box and the node range `node_index1..node_index2` it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BvhTree { pub min: [i16; 3], pub max: [i16; 3], pub node_index1: i16, pub node_index2: i16 }

impl Pod for BvhTree {
    const SIZE: usize = 16;
    fn write(&self, w: &mut Writer) { for v in self.min.into_iter().chain(self.max) { w.i16(v); } w.i16(self.node_index1); w.i16(self.node_index2); }
    fn read(b: &[u8]) -> Self {
        let at = |i: usize| i16::read(&b[2 * i..]);
        BvhTree { min: [at(0), at(1), at(2)], max: [at(3), at(4), at(5)], node_index1: at(6), node_index2: at(7) }
    }
}

/// `BVH` (128 bytes). `nodes` and `trees` are `StructArray<BvhNode>` / `StructArray<BvhTree>` blocks; the
/// two list headers are written inline (they are the block's parts in the reference tool). The `w` component of
/// the five vectors is `float.NaN`.
pub struct Bvh {
    pub nodes: Option<BlockId>, pub nodes_count: u32, pub nodes_capacity: u32,
    pub bb_min: Vec4, pub bb_max: Vec4, pub bb_center: Vec4, pub quantum_inverse: Vec4, pub quantum: Vec4,
    pub trees: Option<BlockId>, pub trees_count: usize,
}

impl Bvh {
    /// `BVH.Read`: the nodes are read up to their capacity, the trees up to their count.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let (nodes_ptr, nodes_count, nodes_capacity) = read_simple_list64b(&mut c);
        c.skip(16);
        let (bb_min, bb_max, bb_center, quantum_inverse, quantum) = (c.vec4(), c.vec4(), c.vec4(), c.vec4(), c.vec4());
        let (trees_ptr, trees_count, _) = read_simple_list64(&mut c);
        c.check()?;
        let nodes = read_struct_array::<BvhNode>(r, g, nodes_ptr, nodes_capacity as usize)?;
        let trees = read_struct_array::<BvhTree>(r, g, trees_ptr, trees_count as usize)?;
        let id = g.add(Bvh { nodes, nodes_count, nodes_capacity, bb_min, bb_max, bb_center, quantum_inverse, quantum, trees, trees_count: trees_count as usize });
        r.cache::<Self>(va, id);
        Ok(Some(id))
    }
}

impl Block for Bvh {
    fn length(&self) -> usize { 128 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { [self.nodes, self.trees].into_iter().flatten().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        write_simple_list64b(w, g, self.nodes, self.nodes_count, self.nodes_capacity);
        w.zeros(16);
        for v in [self.bb_min, self.bb_max, self.bb_center, self.quantum_inverse, self.quantum] { w.vec4(v); }
        write_simple_list64(w, g, self.trees, self.trees_count, "BVH trees")?;
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `BVHBuilderItem`: a box and the index the item is known by (a polygon or a composite child).
#[derive(Clone, Copy, Debug)]
pub struct BvhItem { pub min: Vec3, pub max: Vec3, pub index: usize }

/// What [`build_bvh`] made: the BVH and, for an item threshold above 1 (where the builder regroups the items
/// by node), the original `index` of each item in its new position; otherwise the indices in their old order.
pub struct BvhBuilt { pub bvh: Bvh, pub item_order: Vec<usize> }

/// C# `float.NaN` is `0xFFC00000`, the sign bit set, unlike Rust's `f32::NAN`.
fn cs_nan() -> f32 { f32::from_bits(0xFFC0_0000) }

/// C# `(short)float`: truncation to a 32-bit integer, then to 16 bits (so it wraps rather than saturates).
fn to_short(f: f32) -> i16 { f as i32 as i16 }

/// C# `float.CompareTo`: NaN sorts before every number and equals NaN.
fn cmp_f32(a: f32, b: f32) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if a < b { Less } else if a > b { Greater } else if a == b { Equal } else if a.is_nan() { if b.is_nan() { Equal } else { Less } } else { Greater }
}

/// The reference tool's `Vector3.CompareTo` extension (`Vectors.cs:57`): X, then Y, then Z.
fn cmp_vec3(a: Vec3, b: Vec3) -> std::cmp::Ordering { cmp_f32(a.x, b.x).then(cmp_f32(a.y, b.y)).then(cmp_f32(a.z, b.z)) }

fn vmul3(a: Vec3, b: Vec3) -> Vec3 { Vec3::new(a.x * b.x, a.y * b.y, a.z * b.z) }

/// A non-null item, as the builder holds it; nodes refer to these by position.
struct Item { min: Vec3, max: Vec3, index: usize }

/// `BVHBuilderNode`. `items` is `None` for an inner node (`Items = null` after a split).
struct Node { children: Vec<Node>, items: Option<Vec<usize>>, min: Vec3, max: Vec3, index: usize }

impl Node {
    fn leaf(items: Vec<usize>) -> Node { Node { children: Vec::new(), items: Some(items), min: Vec3::ZERO, max: Vec3::ZERO, index: 0 } }
    fn total_nodes(&self) -> usize { 1 + self.children.iter().map(Node::total_nodes).sum::<usize>() }
    fn total_items(&self) -> usize { self.items.as_ref().map_or(0, Vec::len) + self.children.iter().map(Node::total_items).sum::<usize>() }

    /// `BVHBuilderNode.Build`.
    fn build(&mut self, all: &[Item], threshold: usize) {
        self.update_min_max(all);
        let Some(items) = &self.items else { return };
        if items.len() <= threshold { return; }

        let mut avgsum = Vec3::ZERO;
        for &i in items { avgsum = avgsum + all[i].min; avgsum = avgsum + all[i].max; }
        let avg = avgsum * (0.5 / items.len() as f32);
        let centre = |i: usize| (all[i].min + all[i].max) * 0.5;
        let (mut cx, mut cy, mut cz) = (0, 0, 0);
        for &i in items {
            let c = centre(i);
            if c.x < avg.x { cx += 1; }
            if c.y < avg.y { cy += 1; }
            if c.z < avg.z { cz += 1; }
        }
        let target = items.len() as f32 / 2.0;
        let (dx, dy, dz) = ((target - cx as f32).abs(), (target - cy as f32).abs(), (target - cz as f32).abs());
        let axis = if dx <= dy && dx <= dz { 0 } else if dy <= dz { 1 } else { 2 };

        let (mut l1, mut l2): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        for &i in items {
            let c = centre(i);
            let above = match axis { 0 => c.x > avg.x, 1 => c.y > avg.y, _ => c.z > avg.z };
            if above { l1.push(i); } else { l2.push(i); }
        }

        if l1.is_empty() || l2.is_empty() { // don't get stuck: sort by Min then Max and halve
            let mut l3: Vec<usize> = l1.iter().chain(&l2).copied().collect();
            if l3.is_empty() { return; }
            // C# `List.Sort` is unstable above 16 items; items with identical boxes may come out in another order there.
            l3.sort_by(|&a, &b| cmp_vec3(all[a].min, all[b].min).then(cmp_vec3(all[a].max, all[b].max)));
            let half = l3.len() / 2;
            l2 = l3.split_off(half);
            l1 = l3;
        }

        self.items = None;
        let mut n1 = Node::leaf(l1);
        n1.build(all, threshold);
        let mut n2 = Node::leaf(l2);
        n2.build(all, threshold);
        // `Children.Sort(b.TotalItems.CompareTo(a.TotalItems))` over two nodes only swaps when the second is larger.
        self.children = if n2.total_items() > n1.total_items() { vec![n2, n1] } else { vec![n1, n2] };
    }

    /// `UpdateMinMax`.
    fn update_min_max(&mut self, all: &[Item]) {
        let (mut min, mut max) = (Vec3::new(f32::MAX, f32::MAX, f32::MAX), Vec3::new(f32::MIN, f32::MIN, f32::MIN));
        for &i in self.items.iter().flatten() { min = vmin(min, all[i].min); max = vmax(max, all[i].max); }
        for c in &mut self.children { c.update_min_max(all); min = vmin(min, c.min); max = vmax(max, c.max); }
        self.min = min;
        self.max = max;
    }

    /// `GatherNodes`: pre-order; each node's `index` is its position.
    fn number(&mut self, next: &mut usize) {
        self.index = *next;
        *next += 1;
        for c in &mut self.children { c.number(next); }
    }
    fn gather_nodes<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for c in &self.children { c.gather_nodes(out); }
    }
    /// `GatherTrees`: a node holding more than 127 nodes that has children yields its children's trees.
    fn gather_trees<'a>(&'a self, out: &mut Vec<&'a Node>) {
        if self.total_nodes() > MAX_TREE_NODES && !self.children.is_empty() {
            for c in &self.children { c.gather_trees(out); }
        } else {
            out.push(self);
        }
    }
}

/// `BVHBuilder.Build`: `None` entries are skipped but counted for the capacity of a composite's node list
/// (`item_threshold <= 1`, which pads the nodes to `2 * items + 1` with `{ item_id: 1 }` nodes). The nodes and
/// trees arrays are added to `g`; the caller adds the returned [`Bvh`] itself.
pub fn build_bvh(g: &mut Graph, items: &[Option<BvhItem>], item_threshold: usize) -> BvhBuilt {
    let all: Vec<Item> = items.iter().flatten().map(|i| Item { min: i.min, max: i.max, index: i.index }).collect();
    let (mut min, mut max) = (Vec3::new(f32::MAX, f32::MAX, f32::MAX), Vec3::new(f32::MIN, f32::MIN, f32::MIN));
    for i in &all { min = vmin(min, i.min); max = vmax(max, i.max); }
    let cen = (min + max) * 0.5;
    let q = vmax((min - cen).abs(), (max - cen).abs());
    let quantum = Vec3::new(q.x / 32767.0, q.y / 32767.0, q.z / 32767.0);
    let qi = Vec3::new(1.0 / quantum.x, 1.0 / quantum.y, 1.0 / quantum.z);
    let w = |v: Vec3| Vec4::new(v.x, v.y, v.z, cs_nan());

    let mut root = Node::leaf((0..all.len()).collect());
    root.build(&all, item_threshold);
    root.number(&mut 0);
    let (mut nodes, mut trees) = (Vec::new(), Vec::new());
    root.gather_nodes(&mut nodes);
    root.gather_trees(&mut trees);

    // Above one item per node the items are regrouped by node, so a node's items are contiguous.
    let mut position: Vec<usize> = all.iter().map(|i| i.index).collect();
    let mut item_order: Vec<usize> = position.clone();
    if item_threshold > 1 {
        item_order.clear();
        for n in &nodes {
            for &i in n.items.iter().flatten() { position[i] = item_order.len(); item_order.push(all[i].index); }
        }
    }

    let quantise = |v: Vec3| { let d = vmul3(v - cen, qi); [to_short(d.x), to_short(d.y), to_short(d.z)] };
    let mut records: Vec<BvhNode> = nodes.iter().map(|n| {
        let first = n.items.as_ref().and_then(|l| l.first()).map_or(0, |&i| position[i]);
        let leaf = n.total_nodes() <= 1;
        BvhNode {
            min: quantise(n.min), max: quantise(n.max),
            item_id: if leaf { first as i16 } else { n.total_nodes() as i16 },
            item_count: if leaf { n.total_items() as i16 } else { 0 },
        }
    }).collect();
    let tree_records: Vec<BvhTree> = trees.iter().map(|t| BvhTree {
        min: quantise(t.min), max: quantise(t.max), node_index1: t.index as i16, node_index2: (t.index + t.total_nodes()) as i16,
    }).collect();

    let nodes_count = records.len();
    if item_threshold <= 1 {
        let capacity = items.len() * 2 + 1;
        while records.len() < capacity { records.push(BvhNode { min: [0; 3], max: [0; 3], item_id: 1, item_count: 0 }); }
    }
    let nodes_capacity = records.len();
    let trees_count = tree_records.len();
    let bvh = Bvh {
        nodes: Some(g.add(StructArray { items: records })), nodes_count: nodes_count as u32, nodes_capacity: nodes_capacity as u32,
        bb_min: w(min), bb_max: w(max), bb_center: w(cen), quantum_inverse: w(qi), quantum: w(quantum),
        trees: Some(g.add(StructArray { items: tree_records })), trees_count,
    };
    BvhBuilt { bvh, item_order }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxes(n: usize, step: f32) -> Vec<Option<BvhItem>> {
        (0..n).map(|i| Some(BvhItem { min: Vec3::new(i as f32 * step, 0.0, 0.0), max: Vec3::new(i as f32 * step + 1.0, 1.0, 1.0), index: i })).collect()
    }
    fn bits(v: Vec4) -> [u32; 4] { [v.x.to_bits(), v.y.to_bits(), v.z.to_bits(), v.w.to_bits()] }

    #[test]
    fn bvh_of_eight_unit_boxes_splits_by_balanced_axis_and_quantises() {
        let items = boxes(8, 2.0);
        let mut g = Graph::new();
        let b = build_bvh(&mut g, &items, 4);
        let nodes = g.get::<StructArray<BvhNode>>(b.bvh.nodes.unwrap());
        assert_eq!(nodes.items.len(), 3, "root + two leaves of 4");
        assert_eq!((nodes.items[0].item_count, nodes.items[0].item_id), (0, 3));
        assert_eq!(nodes.items[1].item_count, 4);
        // The reference tool keeps the children in split order on a tie and the split puts the items above the mean
        // centre (x = 7.5) first, so node 1 is the upper leaf and node 2 the lower one. (The brief's sketch
        // expected -32767 in node 1, which is the lower leaf's.)
        assert!(nodes.items[1].min[0] > 0 && nodes.items[1].max[0] >= 32766, "truncation toward zero can leave the top at 32766");
        assert!(nodes.items[2].min[0] <= -32766 && nodes.items[2].max[0] < 0, "the lower leaf starts at the box edge (32767 quanta, truncated toward zero)");
        assert_eq!((nodes.items[1].item_id, nodes.items[2].item_id), (0, 4));
        assert_eq!(b.item_order, vec![4, 5, 6, 7, 0, 1, 2, 3]);
        let trees = g.get::<StructArray<BvhTree>>(b.bvh.trees.unwrap());
        assert_eq!((trees.items[0].node_index1, trees.items[0].node_index2), (0, 3));
        assert_eq!((b.bvh.nodes_count, b.bvh.nodes_capacity, b.bvh.trees_count), (3, 3, 1), "geometries are not padded");
        assert!(b.bvh.bb_min.w.is_nan() && b.bvh.quantum.w.to_bits() == 0xFFC0_0000, "C# float.NaN");
    }

    #[test]
    fn composite_thresholds_keep_item_indices_and_pad_the_nodes() {
        let mut items = boxes(3, 2.0);
        items.insert(1, None);
        let mut g = Graph::new();
        let b = build_bvh(&mut g, &items, 1);
        let nodes = &g.get::<StructArray<BvhNode>>(b.bvh.nodes.unwrap()).items;
        assert_eq!(b.item_order, vec![0, 1, 2], "no regrouping: the original indices");
        assert_eq!((b.bvh.nodes_count, b.bvh.nodes_capacity), (5, 9), "capacity counts the null item: 2 * 4 + 1");
        assert!(nodes[..5].iter().filter(|n| n.item_count == 1).all(|n| (0..3).contains(&n.item_id)));
        assert!(nodes[5..].iter().all(|n| n.item_id == 1 && n.item_count == 0 && n.min == [0; 3]));
    }

    #[test]
    fn a_tree_is_cut_below_a_root_of_more_than_127_nodes() {
        // 100 single-item leaves make a root of 199 nodes: too big for one tree, so the trees are its two children.
        let items = boxes(100, 2.0);
        let mut g = Graph::new();
        let b = build_bvh(&mut g, &items, 1);
        let trees = &g.get::<StructArray<BvhTree>>(b.bvh.trees.unwrap()).items;
        assert_eq!(trees.len(), 2);
        assert_eq!((trees[0].node_index1, trees[0].node_index2), (1, 100));
        assert_eq!((trees[1].node_index1, trees[1].node_index2), (100, 199));
        let nodes = &g.get::<StructArray<BvhNode>>(b.bvh.nodes.unwrap()).items;
        assert_eq!((nodes[0].item_count, nodes[0].item_id, nodes[1].item_id), (0, 199, 99), "an inner node holds its node count");
    }

    #[test]
    fn identical_boxes_fall_back_to_halving() {
        let items: Vec<_> = (0..6).map(|i| Some(BvhItem { min: Vec3::ZERO, max: Vec3::new(1.0, 1.0, 1.0), index: i })).collect();
        let mut g = Graph::new();
        let b = build_bvh(&mut g, &items, 4);
        let nodes = &g.get::<StructArray<BvhNode>>(b.bvh.nodes.unwrap()).items;
        assert_eq!((nodes.len(), nodes[1].item_count, nodes[2].item_count), (3, 3, 3));
    }

    #[test]
    fn a_bvh_block_reads_back_what_it_wrote() {
        use crate::blocks::base::PagesInfo;
        let items = boxes(8, 2.0);
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let built = build_bvh(&mut g, &items, 4).bvh;
        let id = g.add(built);
        assert_eq!(g.length(id), 128);
        let file = g.build(id, pages, 43).unwrap();
        let mut r = Reader::open(&file).unwrap();
        let mut g2 = Graph::new();
        let id2 = Bvh::read(&mut r, &mut g2, crate::resource::SYSTEM_BASE).unwrap().unwrap();
        let (a, b) = (g.get::<Bvh>(id), g2.get::<Bvh>(id2));
        assert_eq!((a.nodes_count, a.nodes_capacity, a.trees_count), (b.nodes_count, b.nodes_capacity, b.trees_count));
        assert_eq!(bits(b.bb_center), bits(a.bb_center));
        assert_eq!(g2.get::<StructArray<BvhNode>>(b.nodes.unwrap()).items, g.get::<StructArray<BvhNode>>(a.nodes.unwrap()).items);
        assert_eq!(g2.get::<StructArray<BvhTree>>(b.trees.unwrap()).items, g.get::<StructArray<BvhTree>>(a.trees.unwrap()).items);
    }
}
