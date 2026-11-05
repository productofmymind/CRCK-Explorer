//! Train track files (`trains*.dat`): a text file with one node per line, `x y z` followed by the node's other values (kept as written).
//! Lines that are not nodes (comments, headers, blank lines) are kept where they are. Edits are undoable; untouched lines are written back
//! exactly as read.
use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
enum Line {
    /// a node: position, the rest of the line after the three numbers, the original text while unedited
    Node { pos: [f32; 3], rest: String, raw: Option<String> },
    Other(String),
}

pub struct TrainTrack {
    lines: Vec<Line>,
    newline: &'static str,
    ends_with_newline: bool,
    undo: Vec<Vec<Line>>,
    redo: Vec<Vec<Line>>,
    saved: Vec<Line>,
}

fn parse_line(text: &str) -> Line {
    let t = text.trim();
    if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
        return Line::Other(text.to_string());
    }
    let mut it = t.split_whitespace();
    let mut pos = [0f32; 3];
    for p in pos.iter_mut() {
        match it.next().and_then(|s| s.parse::<f32>().ok()) {
            Some(v) if v.is_finite() => *p = v,
            _ => return Line::Other(text.to_string()),
        }
    }
    Line::Node { pos, rest: it.collect::<Vec<_>>().join(" "), raw: Some(text.to_string()) }
}

impl TrainTrack {
    pub fn parse(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let lines: Vec<Line> = text.lines().map(parse_line).collect();
        Self { saved: lines.clone(), lines, newline, ends_with_newline: text.ends_with('\n'), undo: vec![], redo: vec![] }
    }

    pub fn to_text(&self) -> String {
        let mut out = self
            .lines
            .iter()
            .map(|l| match l {
                Line::Other(s) => s.clone(),
                Line::Node { raw: Some(r), .. } => r.clone(),
                Line::Node { pos, rest, raw: None } => {
                    let r = if rest.is_empty() { String::new() } else { format!(" {rest}") };
                    format!("{} {} {}{r}", pos[0], pos[1], pos[2])
                }
            })
            .collect::<Vec<_>>()
            .join(self.newline);
        if self.ends_with_newline {
            out.push_str(self.newline);
        }
        out
    }

    fn nodes(&self) -> Vec<usize> {
        self.lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Node { .. })).map(|(i, _)| i).collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes().len()
    }

    pub fn node(&self, n: usize) -> Option<([f32; 3], String)> {
        match self.lines.get(*self.nodes().get(n)?)? {
            Line::Node { pos, rest, .. } => Some((*pos, rest.clone())),
            _ => None,
        }
    }

    /// the length of the track along its nodes, in metres
    pub fn length(&self) -> f32 {
        let p: Vec<[f32; 3]> = (0..self.node_count()).filter_map(|n| self.node(n).map(|x| x.0)).collect();
        p.windows(2).map(|w| ((w[0][0] - w[1][0]).powi(2) + (w[0][1] - w[1][1]).powi(2) + (w[0][2] - w[1][2]).powi(2)).sqrt()).sum()
    }

    pub fn is_dirty(&self) -> bool {
        self.lines != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.lines.clone();
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(l) => {
                self.redo.push(std::mem::replace(&mut self.lines, l));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(l) => {
                self.undo.push(std::mem::replace(&mut self.lines, l));
                true
            }
            None => false,
        }
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.lines.clone());
        self.redo.clear();
    }

    fn line_of(&self, n: usize) -> Result<usize> {
        match self.nodes().get(n) {
            Some(&i) => Ok(i),
            None => bail!("no node {n}"),
        }
    }

    fn check(p: [f32; 3]) -> Result<()> {
        if p.iter().any(|v| !v.is_finite() || v.abs() > 100_000.0) {
            bail!("a position must be a finite number within 100,000 m");
        }
        Ok(())
    }

    pub fn move_node(&mut self, n: usize, p: [f32; 3]) -> Result<()> {
        Self::check(p)?;
        let i = self.line_of(n)?;
        self.checkpoint();
        if let Line::Node { pos, raw, .. } = &mut self.lines[i] {
            *pos = p;
            *raw = None;
        }
        Ok(())
    }

    /// Insert a node after node `n` (or first when `n` is `None`) with the other values `rest` (copied from the node before when empty).
    pub fn insert_node(&mut self, after: Option<usize>, p: [f32; 3], rest: Option<&str>) -> Result<usize> {
        Self::check(p)?;
        let (at_line, number, inherited) = match after {
            Some(n) => {
                let i = self.line_of(n)?;
                let r = match &self.lines[i] {
                    Line::Node { rest, .. } => rest.clone(),
                    _ => String::new(),
                };
                (i + 1, n + 1, r)
            }
            None => (self.nodes().first().copied().unwrap_or(self.lines.len()), 0, String::new()),
        };
        self.checkpoint();
        self.lines.insert(at_line, Line::Node { pos: p, rest: rest.map(str::to_string).unwrap_or(inherited), raw: None });
        Ok(number)
    }

    pub fn remove_node(&mut self, n: usize) -> Result<()> {
        let i = self.line_of(n)?;
        self.checkpoint();
        self.lines.remove(i);
        Ok(())
    }

    /// run the track the other way: the nodes are written in reverse order (other lines stay where they are)
    pub fn reverse(&mut self) {
        self.checkpoint();
        let idx = self.nodes();
        let nodes: Vec<Line> = idx.iter().map(|&i| self.lines[i].clone()).collect();
        for (&i, l) in idx.iter().zip(nodes.into_iter().rev()) {
            self.lines[i] = l;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# track one\r\n0 0 0 1\r\n10.5 0 0 1\r\n\r\n10.5 20 5 2 extra\r\n";

    #[test]
    fn untouched_files_write_back_exactly_and_nodes_are_found() {
        let t = TrainTrack::parse(SAMPLE);
        assert_eq!(t.node_count(), 3);
        assert_eq!(t.to_text(), SAMPLE);
        assert_eq!(t.node(2).unwrap(), ([10.5, 20.0, 5.0], "2 extra".to_string()));
        assert!((t.length() - (10.5 + (400.0f32 + 25.0).sqrt())).abs() < 1e-3);
    }

    #[test]
    fn edits_change_only_their_lines_and_undo() {
        let mut t = TrainTrack::parse(SAMPLE);
        t.move_node(1, [11.0, 1.0, 0.0]).unwrap();
        assert!(t.to_text().contains("11 1 0 1\r\n") && t.to_text().starts_with("# track one\r\n0 0 0 1\r\n"));
        let n = t.insert_node(Some(0), [5.0, 0.0, 0.0], None).unwrap();
        assert_eq!((n, t.node(1).unwrap().1.as_str()), (1, "1"));
        t.remove_node(0).unwrap();
        assert_eq!(t.node_count(), 3);
        assert!(t.move_node(9, [0.0; 3]).is_err() && t.move_node(0, [f32::NAN, 0.0, 0.0]).is_err());
        assert!(t.undo() && t.undo() && t.undo());
        assert_eq!(t.to_text(), SAMPLE);
        assert!(!t.is_dirty() || t.to_text() == SAMPLE);
    }

    #[test]
    fn reversing_keeps_comments_in_place() {
        let mut t = TrainTrack::parse(SAMPLE);
        t.reverse();
        let text = t.to_text();
        assert!(text.starts_with("# track one\r\n10.5 20 5 2 extra\r\n"), "{text}");
        assert!(text.ends_with("0 0 0 1\r\n"));
    }
}
