//! Editing a `.ynd` (vehicle and ped path nodes of one 512 m cell): move nodes, change their flags and street name, add and remove nodes and links,
//! with undo. Nodes are numbered by their place in the file and links name `(cell, node number)`, so removing a node renumbers the ones after it and
//! every link that points at them. Saving builds the file and reads it back; a file that does not read back the same is not written.
use anyhow::{bail, Context, Result};
use rage_formats::ynd::{parse_node_dictionary, serialize_node_dictionary, Node, NodeDictionary, NodeLink, NodeSpecial};

const MAX_LINKS: usize = 31;

pub struct YndDocument {
    dict: NodeDictionary,
    undo: Vec<NodeDictionary>,
    redo: Vec<NodeDictionary>,
    saved: NodeDictionary,
}

fn quantise(p: [f32; 3]) -> (i16, i16, i16) {
    ((p[0] * 4.0) as i16, (p[1] * 4.0) as i16, (p[2] * 32.0) as i16)
}

fn link_count(n: &Node) -> usize {
    (n.link_count_flags >> 3) as usize
}

fn set_link_count(n: &mut Node, c: usize) {
    n.link_count_flags = (n.link_count_flags & 7) | ((c as u8) << 3);
}

impl YndDocument {
    pub fn open(bytes: &[u8]) -> Result<Self> {
        let dict = parse_node_dictionary(bytes).context("reading the path node file")?;
        Ok(Self::from_dictionary(dict))
    }

    pub fn from_dictionary(dict: NodeDictionary) -> Self {
        Self { saved: dict.clone(), dict, undo: vec![], redo: vec![] }
    }

    pub fn dictionary(&self) -> &NodeDictionary {
        &self.dict
    }

    pub fn node_count(&self) -> usize {
        self.dict.nodes.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.dict != self.saved
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(d) => {
                self.redo.push(std::mem::replace(&mut self.dict, d));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(d) => {
                self.undo.push(std::mem::replace(&mut self.dict, d));
                true
            }
            None => false,
        }
    }

    /// run an edit as one undo step; when it fails nothing changes
    fn edit(&mut self, f: impl FnOnce(&mut NodeDictionary) -> Result<()>) -> Result<()> {
        let before = self.dict.clone();
        match f(&mut self.dict) {
            Ok(()) => {
                recount(&mut self.dict);
                self.undo.push(before);
                self.redo.clear();
                Ok(())
            }
            Err(e) => {
                self.dict = before;
                Err(e)
            }
        }
    }

    pub fn node_position(&self, i: usize) -> Option<[f32; 3]> {
        self.dict.nodes.get(i).map(|n| [n.position_x as f32 / 4.0, n.position_y as f32 / 4.0, n.position_z as f32 / 32.0])
    }

    /// the links leaving node `i`
    pub fn links_of(&self, i: usize) -> Vec<NodeLink> {
        let Some(n) = self.dict.nodes.get(i) else { return vec![] };
        let start = n.link_id as usize;
        self.dict.links.get(start..start + link_count(n)).map(|s| s.to_vec()).unwrap_or_default()
    }

    /// Move node `i` (the position is kept to the file's precision: a quarter metre in x and y, a thirty-second in z).
    pub fn set_position(&mut self, i: usize, p: [f32; 3]) -> Result<()> {
        if i >= self.node_count() {
            bail!("no node {i}");
        }
        if p.iter().any(|v| !v.is_finite()) || p[0].abs() >= 8191.0 || p[1].abs() >= 8191.0 || p[2].abs() >= 1023.0 {
            bail!("a node must lie within 8,191 m in x and y and 1,023 m in z of the origin");
        }
        self.edit(|d| {
            let (x, y, z) = quantise(p);
            let n = &mut d.nodes[i];
            (n.position_x, n.position_y, n.position_z) = (x, y, z);
            Ok(())
        })
    }

    /// Set the five flag bytes of node `i` (`flags0`..`flags4`).
    pub fn set_flags(&mut self, i: usize, flags: [u8; 5]) -> Result<()> {
        if i >= self.node_count() {
            bail!("no node {i}");
        }
        self.edit(|d| {
            let n = &mut d.nodes[i];
            n.flags0 = flags[0];
            n.flags1 = flags[1];
            n.flags2 = flags[2];
            n.flags3 = flags[3];
            n.flags4 = flags[4];
            Ok(())
        })
    }

    pub fn set_street_name(&mut self, i: usize, hash: u32) -> Result<()> {
        if i >= self.node_count() {
            bail!("no node {i}");
        }
        self.edit(|d| {
            d.nodes[i].street_name = hash;
            Ok(())
        })
    }

    /// Add a node at `p` with no links; returns its number. It takes the cell of the file's other nodes.
    pub fn add_node(&mut self, p: [f32; 3]) -> Result<usize> {
        if self.node_count() >= u16::MAX as usize {
            bail!("a path node file holds at most 65,535 nodes");
        }
        let area = self.dict.nodes.first().map_or(0, |n| n.area_id);
        let at = self.node_count();
        self.edit(|d| {
            let (x, y, z) = quantise(p);
            d.nodes.push(Node { area_id: area, node_id: at as u16, link_id: d.links.len() as u16, position_x: x, position_y: y, position_z: z, ..Default::default() });
            Ok(())
        })?;
        Ok(at)
    }

    /// Add a link from node `from` to node `to` of this file (appended to the node's links). Returns its place among them.
    pub fn add_link(&mut self, from: usize, to: usize, flags: [u8; 3]) -> Result<usize> {
        let n = self.node_count();
        if from >= n || to >= n || from == to {
            bail!("a link joins two different nodes of the file");
        }
        if link_count(&self.dict.nodes[from]) >= MAX_LINKS {
            bail!("a node has at most {MAX_LINKS} links");
        }
        if self.links_of(from).iter().any(|l| l.area_id == self.dict.nodes[to].area_id && l.node_id as usize == to) {
            bail!("node {from} already links to node {to}");
        }
        let (a, b) = (self.node_position(from).unwrap_or([0.0; 3]), self.node_position(to).unwrap_or([0.0; 3]));
        let length = (((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt().round()).clamp(0.0, 255.0) as u8;
        let mut place = 0;
        self.edit(|d| {
            let mut per = per_node(d);
            place = per[from].len();
            per[from].push(NodeLink { area_id: d.nodes[to].area_id, node_id: to as u16, flags0: flags[0], flags1: flags[1], flags2: flags[2], link_length: length });
            flatten(d, per);
            Ok(())
        })?;
        Ok(place)
    }

    /// Remove the `k`th link leaving node `from`.
    pub fn remove_link(&mut self, from: usize, k: usize) -> Result<()> {
        if from >= self.node_count() || k >= link_count(&self.dict.nodes[from]) {
            bail!("no such link");
        }
        self.edit(|d| {
            let mut per = per_node(d);
            per[from].remove(k);
            flatten(d, per);
            Ok(())
        })
    }

    /// Remove node `i` with its links and the links of this file that lead to it; the nodes after it are renumbered. Refused for a junction
    /// node (its junction data would be left behind).
    pub fn remove_node(&mut self, i: usize) -> Result<()> {
        if i >= self.node_count() {
            bail!("no node {i}");
        }
        let area = self.dict.nodes[i].area_id;
        if self.dict.junction_refs.iter().any(|r| r.area_id == area && r.node_id as usize == i) {
            bail!("node {i} belongs to a junction: its junction data would be left behind");
        }
        self.edit(|d| {
            let mut per = per_node(d);
            per.remove(i);
            d.nodes.remove(i);
            for links in per.iter_mut() {
                links.retain(|l| !(l.area_id == area && l.node_id as usize == i));
                for l in links.iter_mut() {
                    if l.area_id == area && l.node_id as usize > i {
                        l.node_id -= 1;
                    }
                }
            }
            for (idx, n) in d.nodes.iter_mut().enumerate() {
                if n.area_id == area {
                    n.node_id = idx as u16;
                }
            }
            for r in d.junction_refs.iter_mut() {
                if r.area_id == area && r.node_id as usize > i {
                    r.node_id -= 1;
                }
            }
            flatten(d, per);
            Ok(())
        })
    }

    /// The file's bytes, read back and compared (node and link counts, positions): an error says what would change; nothing is written then.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serialize_node_dictionary(&self.dict).context("building the path node file")?;
        let back = parse_node_dictionary(&bytes).context("reading the built file back")?;
        if back.nodes != self.dict.nodes || back.links != self.dict.links {
            bail!("saving would change the nodes or links: nothing was written");
        }
        Ok(bytes)
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.dict.clone();
    }

}

/// each node's links, in node order
fn per_node(d: &NodeDictionary) -> Vec<Vec<NodeLink>> {
    d.nodes
        .iter()
        .map(|n| {
            let s = n.link_id as usize;
            d.links.get(s..s + link_count(n)).map(|x| x.to_vec()).unwrap_or_default()
        })
        .collect()
}

/// store the links node by node and point each node at its own run
fn flatten(d: &mut NodeDictionary, per: Vec<Vec<NodeLink>>) {
    d.links.clear();
    for (n, links) in d.nodes.iter_mut().zip(per) {
        n.link_id = d.links.len() as u16;
        set_link_count(n, links.len());
        d.links.extend(links);
    }
}

/// the vehicle and ped node counts the header keeps
fn recount(d: &mut NodeDictionary) {
    let ped = d.nodes.iter().filter(|n| NodeSpecial::from_value(n.flags1 >> 3).is_ped()).count() as u32;
    d.nodes_count_ped = ped;
    d.nodes_count_vehicle = d.nodes.len() as u32 - ped;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_nodes() -> YndDocument {
        let mut d = YndDocument::from_dictionary(NodeDictionary::default());
        d.dict.nodes.push(Node { area_id: 7, node_id: 0, ..Default::default() });
        for p in [[0.0, 0.0, 10.0], [10.0, 0.0, 10.0], [10.0, 10.0, 10.0]] {
            d.add_node(p).unwrap();
        }
        d.dict.nodes.remove(0);
        for (k, n) in d.dict.nodes.iter_mut().enumerate() {
            n.node_id = k as u16;
            n.area_id = 7;
        }
        d.undo.clear();
        d
    }

    #[test]
    fn nodes_move_to_the_files_precision_and_undo_restores() {
        let mut d = three_nodes();
        d.set_position(1, [12.3, -4.6, 20.1]).unwrap();
        let p = d.node_position(1).unwrap();
        assert!((p[0] - 12.25).abs() < 1e-4 && (p[1] + 4.5).abs() < 1e-4 && (p[2] - 20.09375).abs() < 1e-4, "{p:?}");
        assert!(d.is_dirty());
        assert!(d.undo() && d.node_position(1).unwrap()[0] == 10.0);
        assert!(d.set_position(9, [0.0; 3]).is_err() && d.set_position(0, [f32::NAN, 0.0, 0.0]).is_err() && d.set_position(0, [9000.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn links_are_added_in_place_removed_and_the_other_nodes_link_ids_follow() {
        let mut d = three_nodes();
        d.add_link(0, 1, [1, 0, 0]).unwrap();
        d.add_link(1, 2, [2, 0, 0]).unwrap();
        d.add_link(0, 2, [3, 0, 0]).unwrap();
        // node 0's links come first, then node 1's
        assert_eq!(d.links_of(0).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(d.links_of(1).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(d.links_of(0)[0].link_length, 10);
        assert!(d.add_link(0, 1, [0; 3]).is_err() && d.add_link(0, 0, [0; 3]).is_err() && d.add_link(0, 9, [0; 3]).is_err());
        d.remove_link(0, 0).unwrap();
        assert_eq!(d.links_of(0).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(d.links_of(1).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![2], "node 1 still has its own link");
        assert!(d.remove_link(2, 0).is_err());
    }

    #[test]
    fn removing_a_node_removes_links_to_it_and_renumbers_the_rest() {
        let mut d = three_nodes();
        d.add_link(0, 1, [0; 3]).unwrap();
        d.add_link(0, 2, [0; 3]).unwrap();
        d.add_link(2, 0, [0; 3]).unwrap();
        d.add_link(2, 1, [0; 3]).unwrap();
        d.remove_node(1).unwrap();
        assert_eq!(d.node_count(), 2);
        assert_eq!(d.links_of(0).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![1], "the link to the removed node is gone, node 2 is now node 1");
        assert_eq!(d.links_of(1).iter().map(|l| l.node_id).collect::<Vec<_>>(), vec![0]);
        assert_eq!((d.dict.nodes[1].node_id, d.dict.links.len()), (1, 2));
        assert_eq!(d.dict.nodes_count_vehicle, 2);
        d.undo();
        assert_eq!(d.node_count(), 3);
    }

    #[test]
    fn saving_reads_back_the_same_and_a_junction_node_is_kept() {
        let mut d = three_nodes();
        d.add_link(0, 1, [1, 2, 3]).unwrap();
        d.set_flags(2, [8, 16, 4, 0, 0]).unwrap();
        let back = YndDocument::open(&d.to_bytes().unwrap()).unwrap();
        assert_eq!((back.node_count(), back.links_of(0).len(), back.dict.nodes[2].flags0), (3, 1, 8));
        d.dict.junction_refs.push(rage_formats::ynd::NodeJunctionRef { area_id: 7, node_id: 1, junction_id: 0, unk0: 0 });
        assert!(d.remove_node(1).is_err());
        d.mark_saved();
        assert!(!d.is_dirty());
    }
}
