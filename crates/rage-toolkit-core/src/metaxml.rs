//! A validator for the XML side of GTA V modding: `.meta` files and the reference tool-style XML (`*.ytyp.xml`, `*.ymap.xml`, ...).
//!
//! Two layers, because they answer different questions:
//!
//! 1. **Syntax.** A small scanner reads the text once and reports *every* well-formedness mistake with its line and column and a
//!    way to fix it (mismatched or unclosed tags, a missing quote, an attribute with no value, a bare `&`, two roots, ...). A strict
//!    parser such as the one in .NET (what the reference tool uses) stops at the first error; this one keeps going. To avoid false positives
//!    the scanner never has the last word: `roxmltree` (a strict XML 1.0 parser) is run as well, and if it accepts the text every
//!    scanner finding is dropped; if it rejects the text and the scanner found nothing, its message is reported instead.
//! 2. **Structure.** When the text is well-formed, the root element names a structure the game knows (`CMapTypes`,
//!    `CHandlingDataMgr`, `CVehicleModelInfoVarGlobal`, ...). Every element below it is checked against that structure's definition
//!    from the reference tool's table (`rage-formats`' built-in schema): members that do not exist (the game ignores them, so a typo silently
//!    does nothing), values that are not numbers, enum names that are not members, arrays that should hold `<Item>`s, and a few
//!    format-specific rules (duplicate archetype names).
//!
//! Severities: `Error` = the game or the reference tool will fail or misread it, `Warning` = very likely a mistake, `Info` = worth knowing.
use rage_formats::schema::{MetaEntryDef, MetaStructDef, PsoEntryDef, PsoStructDef};
use rage_formats::{rage_joaat, NameTable, Schema};
use roxmltree::{Document, Node};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    /// stable id for scripts and tests, e.g. `XML_UNCLOSED`
    pub code: &'static str,
    /// 1-based
    pub line: usize,
    /// 1-based, in characters
    pub col: usize,
    /// characters to underline (at least 1; never past the end of the line)
    pub len: usize,
    pub message: String,
    /// how to fix it (empty when there is nothing more to say)
    pub fix: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
    /// the root element's name when the text is well-formed (`CMapTypes`, ...)
    pub root: Option<String>,
    /// whether the structure layer ran (the root is a structure the game knows)
    pub structure_checked: bool,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error).count()
    }
}

const MAX_DIAGNOSTICS: usize = 300;

/// Check `text`. Cheap enough to run on every pause in typing for files of a few hundred KB.
pub fn check(text: &str) -> Report {
    let chars: Vec<char> = text.chars().collect();
    let mut sc = Scanner::new(&chars);
    sc.scan();
    let mut diagnostics = sc.out;
    // The game's own files break these rules and load fine (452 comments with "--", blank lines before <?xml?>, attributes with no space
    // between them), so they are warnings: strict XML tools such as the reference tool's .NET parser refuse them.
    for d in diagnostics.iter_mut() {
        match d.code {
            "XML_DECL_POSITION" => {
                d.message = "the <?xml ...?> declaration is not the very first thing in the file; the game accepts this, strict XML tools do not".into();
                d.severity = Severity::Warning;
            }
            "XML_COMMENT_DASHES" => {
                d.message = "'--' inside a comment is against the XML rules; the game accepts it, strict XML tools (the reference tool) refuse the file".into();
                d.severity = Severity::Warning;
            }
            "XML_ATTR_SPACE" => {
                d.message = format!("{}; the game accepts this, strict XML tools do not", d.message);
                d.severity = Severity::Warning;
            }
            _ => {}
        }
    }

    // the strict parser has the last word on syntax
    let parsed = Document::parse(text.trim_start_matches('\u{feff}'));
    let mut report = Report::default();
    match &parsed {
        Ok(_) => diagnostics.clear(),
        Err(e) if diagnostics.is_empty() => {
            let p = e.pos();
            let namespace = matches!(e, roxmltree::Error::UnknownNamespace(..));
            diagnostics.push(Diagnostic {
                severity: if namespace { Severity::Warning } else { Severity::Error },
                code: "XML_INVALID",
                line: p.row as usize,
                col: p.col as usize,
                len: 1,
                message: format!("not valid XML: {e}"),
                fix: if namespace { "the game's own files use XInclude (xi:include) like this; elsewhere declare the prefix with xmlns:...".into() } else { "fix the XML at this position; the game and the reference tool reject the whole file".into() },
            });
        }
        Err(_) => {}
    }
    if let Ok(doc) = &parsed {
        report.root = Some(doc.root_element().tag_name().name().to_string());
        let mut st = Structure::new(Schema::builtin(), doc, &mut diagnostics);
        st.run();
        report.structure_checked = st.checked;
        domain_rules(doc.root_element().tag_name().name(), text, &mut diagnostics);
    }
    diagnostics.sort_by_key(|d| (d.line, d.col));
    diagnostics.truncate(MAX_DIAGNOSTICS);
    report.diagnostics = diagnostics;
    report
}

// ---- layer 1: syntax ------------------------------------------------------------------------------------------------------

struct Scanner<'a> {
    c: &'a [char],
    /// char index where each line starts
    starts: Vec<usize>,
    out: Vec<Diagnostic>,
}

fn is_name_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == ':'
}
fn is_name_char(ch: char) -> bool {
    is_name_start(ch) || ch.is_ascii_digit() || ch == '-' || ch == '.'
}

impl<'a> Scanner<'a> {
    fn new(c: &'a [char]) -> Self {
        let mut starts = vec![0];
        for (i, ch) in c.iter().enumerate() {
            if *ch == '\n' {
                starts.push(i + 1);
            }
        }
        Self { c, starts, out: vec![] }
    }

    fn pos(&self, i: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|&s| s <= i).max(1);
        (line, i - self.starts[line - 1] + 1)
    }

    fn line_end(&self, i: usize) -> usize {
        let mut j = i;
        while j < self.c.len() && self.c[j] != '\n' && self.c[j] != '\r' {
            j += 1;
        }
        j
    }

    fn err(&mut self, code: &'static str, i: usize, len: usize, message: String, fix: &str) {
        let (line, col) = self.pos(i.min(self.c.len()));
        let room = self.line_end(i.min(self.c.len())).saturating_sub(i);
        self.out.push(Diagnostic { severity: Severity::Error, code, line, col, len: len.min(room.max(1)).max(1), message, fix: fix.to_string() });
    }

    fn starts_with(&self, i: usize, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.c.get(i + k) == Some(&ch))
    }

    fn find(&self, from: usize, s: &str) -> Option<usize> {
        self.find_before(from, self.c.len(), s)
    }

    /// first `s` that starts in `from..to`
    fn find_before(&self, from: usize, to: usize, s: &str) -> Option<usize> {
        (from..to.min(self.c.len())).find(|&i| self.starts_with(i, s))
    }

    fn name_at(&self, i: usize) -> usize {
        let mut j = i;
        if j < self.c.len() && is_name_start(self.c[j]) {
            j += 1;
            while j < self.c.len() && is_name_char(self.c[j]) {
                j += 1;
            }
        }
        j
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.c.len() && self.c[i].is_whitespace() {
            i += 1;
        }
        i
    }

    fn text(&self, a: usize, b: usize) -> String {
        self.c[a..b].iter().collect()
    }

    /// `&` in text or in an attribute value must start an entity.
    fn check_entities(&mut self, a: usize, b: usize) {
        let mut i = a;
        while i < b {
            if self.c[i] == '&' {
                let end = (i + 1..b.min(i + 12)).find(|&k| self.c[k] == ';');
                let ok = end.map(|e| {
                    let body = self.text(i + 1, e);
                    matches!(body.as_str(), "lt" | "gt" | "amp" | "quot" | "apos") || body.strip_prefix("#x").map(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit())).unwrap_or(false) || body.strip_prefix('#').map(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())).unwrap_or(false)
                });
                if ok != Some(true) {
                    self.err("XML_AMPERSAND", i, 1, "a bare '&' is not allowed in XML".into(), "write &amp; for an ampersand (also &lt; for <, &gt; for >, &quot; for \" and &apos; for ')");
                }
            }
            i += 1;
        }
    }

    fn scan(&mut self) {
        let n = self.c.len();
        let mut i = if self.c.first() == Some(&'\u{feff}') { 1 } else { 0 };
        let first_content = i;
        // open elements: name, index of its '<'
        let mut stack: Vec<(String, usize)> = vec![];
        let mut roots = 0usize;
        let mut root_closed = false;
        while i < n {
            if self.c[i] != '<' {
                let j = (i..n).find(|&k| self.c[k] == '<').unwrap_or(n);
                if stack.is_empty() {
                    if let Some(k) = (i..j).find(|&k| !self.c[k].is_whitespace()) {
                        self.err("XML_TEXT_OUTSIDE_ROOT", k, 1, "text outside the root element".into(), "everything must be inside the one root element; remove this text or move it inside");
                    }
                } else {
                    self.check_entities(i, j);
                    if let Some(k) = self.find_before(i, j, "]]>") {
                        self.err("XML_CDATA_END", k, 3, "']]>' is not allowed in text".into(), "write ]]&gt; instead");
                    }
                }
                i = j;
                continue;
            }
            if self.starts_with(i, "<!--") {
                match self.find(i + 4, "-->") {
                    None => {
                        self.err("XML_COMMENT_UNCLOSED", i, 4, "this comment is never closed".into(), "end the comment with -->");
                        i = n;
                    }
                    Some(e) => {
                        if let Some(k) = self.find_before(i + 4, e, "--") {
                            self.err("XML_COMMENT_DASHES", k, 2, "'--' is not allowed inside a comment".into(), "remove the double dash or use a single - ; XML comments cannot contain --");
                        }
                        i = e + 3;
                    }
                }
            } else if self.starts_with(i, "<![CDATA[") {
                match self.find(i + 9, "]]>") {
                    None => {
                        self.err("XML_CDATA_UNCLOSED", i, 9, "this CDATA section is never closed".into(), "end it with ]]>");
                        i = n;
                    }
                    Some(e) => i = e + 3,
                }
            } else if self.starts_with(i, "<?") {
                match self.find(i + 2, "?>") {
                    None => {
                        self.err("XML_PI_UNCLOSED", i, 2, "this <? ... ?> declaration is never closed".into(), "end it with ?>");
                        i = n;
                    }
                    Some(e) => {
                        let is_decl = self.starts_with(i, "<?xml") && self.c.get(i + 5).map(|c| c.is_whitespace() || *c == '?').unwrap_or(false);
                        if is_decl && i != first_content {
                            self.err("XML_DECL_POSITION", i, 5, "the <?xml ...?> declaration must be the very first thing in the file".into(), "remove the blank lines or text before it");
                        }
                        i = e + 2;
                    }
                }
            } else if self.starts_with(i, "<!") {
                i = self.find(i, ">").map(|e| e + 1).unwrap_or(n);
            } else if self.starts_with(i, "</") {
                i = self.end_tag(i, &mut stack, &mut root_closed);
            } else {
                i = self.start_tag(i, &mut stack, &mut roots, &mut root_closed);
            }
        }
        for (name, at) in stack.iter().rev() {
            let (line, _) = self.pos(*at);
            let (name, at, line) = (name.clone(), *at, line);
            self.err("XML_UNCLOSED", at, name.chars().count() + 1, format!("<{name}> (line {line}) is never closed"), &format!("add </{name}> where this element ends"));
        }
        if roots == 0 && self.out.is_empty() {
            self.err("XML_NO_ROOT", 0, 1, "the file has no root element".into(), "a .meta file is one root element, for example <CHandlingDataMgr> ... </CHandlingDataMgr>");
        }
    }

    /// `</name>` at `i`; returns the index after it.
    fn end_tag(&mut self, i: usize, stack: &mut Vec<(String, usize)>, root_closed: &mut bool) -> usize {
        let ns = i + 2;
        let ne = self.name_at(ns);
        let name = self.text(ns, ne);
        let after = self.skip_ws(ne);
        let mut next = after;
        if name.is_empty() {
            self.err("XML_END_TAG_NAME", i, 2, "a closing tag needs the element name".into(), "write </name> with the name of the element it closes");
            return self.find(i, ">").map(|e| e + 1).unwrap_or(self.c.len());
        }
        if self.c.get(after) == Some(&'>') {
            next = after + 1;
        } else {
            self.err("XML_TAG_UNCLOSED", i, ne - i, format!("the closing tag </{name}> is missing its '>'"), "end the tag with >");
            if let Some(e) = (after..self.c.len()).find(|&k| self.c[k] == '>' || self.c[k] == '<') {
                next = if self.c[e] == '>' { e + 1 } else { e };
            }
        }
        match stack.last() {
            None => self.err("XML_END_WITHOUT_START", i, ne - i, format!("</{name}> closes an element that was never opened"), "remove this closing tag, or add the opening tag before it"),
            Some((top, _)) if *top == name => {
                stack.pop();
                if stack.is_empty() {
                    *root_closed = true;
                }
            }
            Some((top, topat)) => {
                let (top, topat) = (top.clone(), *topat);
                let (topline, _) = self.pos(topat);
                if let Some(depth) = stack.iter().rposition(|(n, _)| *n == name) {
                    // elements between the match and the top were never closed
                    for (n, at) in stack.split_off(depth + 1).into_iter().rev() {
                        let (l, _) = self.pos(at);
                        self.err("XML_UNCLOSED", at, n.chars().count() + 1, format!("<{n}> (line {l}) is not closed before </{name}>"), &format!("add </{n}> before the </{name}> on line {}", self.pos(i).0));
                    }
                    stack.pop();
                    if stack.is_empty() {
                        *root_closed = true;
                    }
                } else {
                    let case = stack.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name));
                    let fix = if case { format!("XML is case-sensitive: the open element is <{top}>; write </{top}>") } else { format!("write </{top}> here, or remove this tag") };
                    self.err("XML_MISMATCH", i, ne - i, format!("</{name}> does not match the open element <{top}> (line {topline})"), &fix);
                }
            }
        }
        next
    }

    /// `<name attributes>` or `<name attributes/>` at `i`; returns the index after it.
    fn start_tag(&mut self, i: usize, stack: &mut Vec<(String, usize)>, roots: &mut usize, root_closed: &mut bool) -> usize {
        let n = self.c.len();
        let ns = i + 1;
        let ne = self.name_at(ns);
        if ne == ns {
            self.err("XML_LT", i, 1, "'<' must be followed by an element name".into(), "to write a literal < in text or an attribute use &lt;");
            return i + 1;
        }
        let name = self.text(ns, ne);
        let mut j = ne;
        let mut seen: HashSet<String> = HashSet::new();
        let mut self_closing = false;
        let mut closed = false;
        loop {
            let before = j;
            j = self.skip_ws(j);
            if j >= n || self.c[j] == '<' {
                self.err("XML_TAG_UNCLOSED", ns, ne - ns, format!("the tag <{name}> is missing its closing '>'"), "end the tag with > (or /> for an element with no content)");
                break;
            }
            if self.c[j] == '>' {
                j += 1;
                closed = true;
                break;
            }
            if self.starts_with(j, "/>") {
                j += 2;
                self_closing = true;
                closed = true;
                break;
            }
            if self.c[j] == '/' {
                self.err("XML_SLASH", j, 1, "a '/' inside a tag must be followed by '>'".into(), "write /> to close an empty element");
                j += 1;
                continue;
            }
            let ae = self.name_at(j);
            if ae == j {
                self.err("XML_TAG_CHAR", j, 1, format!("unexpected character '{}' inside the tag <{name}>", self.c[j]), "attributes look like name=\"value\"; remove this character");
                j += 1;
                continue;
            }
            if j == before && !seen.is_empty() {
                // two attributes with no space between them is reported where the first value ends
            }
            let attr = self.text(j, ae);
            if !seen.insert(attr.clone()) {
                self.err("XML_ATTR_DUPLICATE", j, ae - j, format!("the attribute '{attr}' appears twice in <{name}>"), "remove one of them");
            }
            let mut k = self.skip_ws(ae);
            if self.c.get(k) != Some(&'=') {
                self.err("XML_ATTR_NO_VALUE", j, ae - j, format!("the attribute '{attr}' has no value"), &format!("write {attr}=\"...\"; every attribute needs a value in quotes"));
                j = ae;
                continue;
            }
            k = self.skip_ws(k + 1);
            match self.c.get(k) {
                Some(&q) if q == '"' || q == '\'' => {
                    let end = (k + 1..n).find(|&e| self.c[e] == q || self.c[e] == '<');
                    match end {
                        Some(e) if self.c[e] == q => {
                            self.check_entities(k + 1, e);
                            j = e + 1;
                            // the next thing must be space, '>' or '/'
                            if let Some(&nx) = self.c.get(j) {
                                if !(nx.is_whitespace() || nx == '>' || nx == '/') {
                                    self.err("XML_ATTR_SPACE", j, 1, format!("a space is needed between the attributes of <{name}>"), "put a space after the closing quote");
                                }
                            }
                        }
                        _ => {
                            self.err("XML_ATTR_QUOTE", k, 1, format!("the value of '{attr}' is missing its closing {q}"), &format!("close the value with {q} before the end of the tag"));
                            j = end.unwrap_or(n).min((k..n).find(|&e| self.c[e] == '>' || self.c[e] == '\n').unwrap_or(n));
                        }
                    }
                }
                Some(_) => {
                    let e = (k..n).find(|&e| self.c[e].is_whitespace() || self.c[e] == '>' || self.starts_with(e, "/>")).unwrap_or(n);
                    let v = self.text(k, e);
                    self.err("XML_ATTR_UNQUOTED", k, (e - k).max(1), format!("the value of '{attr}' must be in quotes"), &format!("write {attr}=\"{v}\""));
                    j = e;
                }
                None => {
                    j = n;
                }
            }
        }
        let _ = closed;
        if stack.is_empty() {
            if *root_closed {
                self.err("XML_SECOND_ROOT", ns, ne - ns, format!("<{name}> is a second root element; a file can only have one"), "put it inside the root element, or move it to its own file");
            } else {
                *roots += 1;
            }
        }
        if !self_closing {
            stack.push((name, i));
        } else if stack.is_empty() {
            *root_closed = true;
        }
        j
    }
}

// ---- layer 2: structure ---------------------------------------------------------------------------------------------------

fn names() -> &'static NameTable {
    static N: std::sync::OnceLock<NameTable> = std::sync::OnceLock::new();
    N.get_or_init(NameTable::core)
}

fn member_name(hash: u32) -> String {
    names().resolve(hash).into_owned()
}

/// What one member holds, whichever of the reference tool's two tables (Meta or PSO) defined it.
#[derive(Debug, Clone)]
pub enum Kind {
    Bool,
    /// whole number and its range
    Int(i128, i128),
    Float,
    /// 2, 3 or 4 axes (`x`, `y`, `z`, `w` attributes)
    Vector(usize),
    /// text: anything goes
    Text,
    /// a hashed name (`<name>prop_box</name>` in the XML)
    Hash,
    Enum(u32),
    Flags(u32),
    /// structure name hash (0 = any) and whether it is held by pointer (`type="..."` names the actual structure)
    Struct(u32, bool),
    Array(Box<Kind>),
    /// not checked (fixed-size arrays, maps, ...)
    Other,
}

struct Def {
    name: String,
    members: HashMap<String, Kind>,
}

pub(crate) fn meta_kind(e: &MetaEntryDef, parent: &MetaStructDef) -> Kind {
    match e.ty {
        0x01 => Kind::Bool,
        0x10 => Kind::Int(-128, 127),
        0x11 => Kind::Int(0, 255),
        0x12 => Kind::Int(-32768, 32767),
        0x13 => Kind::Int(0, 65535),
        0x14 => Kind::Int(-(1 << 31), (1 << 31) - 1),
        0x15 => Kind::Int(0, u32::MAX as i128),
        0x21 => Kind::Float,
        0x33 => Kind::Vector(3),
        0x34 => Kind::Vector(4),
        0x40 | 0x44 => Kind::Text,
        0x4A => Kind::Hash,
        0x60 | 0x62 => Kind::Enum(e.ref_key),
        0x63 | 0x64 | 0x65 => Kind::Flags(e.ref_key),
        0x05 => Kind::Struct(e.ref_key, false),
        0x07 => Kind::Struct(e.ref_key, true),
        0x52 => usize::try_from(e.ref_index).ok().and_then(|k| parent.entries.get(k)).filter(|x| x.name == 0x100).map(|x| Kind::Array(Box::new(meta_kind(x, parent)))).unwrap_or(Kind::Other),
        _ => Kind::Other,
    }
}

pub(crate) fn pso_kind(e: &PsoEntryDef, parent: &PsoStructDef) -> Kind {
    let array_info = |idx: usize| parent.entries.get(idx).filter(|x| x.name == 0x100);
    match e.ty {
        0x00 => Kind::Bool,
        0x01 => Kind::Int(-128, 127),
        0x02 => Kind::Int(0, 255),
        0x03 => Kind::Int(-32768, 32767),
        0x04 => Kind::Int(0, 65535),
        0x05 => Kind::Int(-(1 << 31), (1 << 31) - 1),
        0x06 => Kind::Int(0, u32::MAX as i128),
        0x20 => Kind::Int(i64::MIN as i128, u64::MAX as i128),
        0x07 | 0x1E => Kind::Float,
        0x08 => Kind::Vector(2),
        0x09 | 0x14 => Kind::Vector(3),
        0x0A | 0x15 => Kind::Vector(4),
        0x0B => Kind::Text,
        0x0C => Kind::Struct(e.ref_key, e.subtype != 0),
        0x0E => Kind::Enum(e.ref_key),
        0x0F => array_info((e.ref_key & 0xFFF) as usize).map(|x| Kind::Flags(x.ref_key)).unwrap_or(Kind::Other),
        0x0D => {
            let mut idx = (e.ref_key & 0xFFFF) as usize;
            if array_info(idx).is_none() {
                idx = (e.ref_key & 0xFFF) as usize;
            }
            array_info(idx).map(|x| Kind::Array(Box::new(pso_kind(x, parent)))).unwrap_or(Kind::Other)
        }
        _ => Kind::Other,
    }
}

struct Structure<'a, 'd> {
    schema: &'static Schema,
    doc: &'a Document<'d>,
    out: &'a mut Vec<Diagnostic>,
    checked: bool,
    defs: HashMap<u32, Option<std::rc::Rc<Def>>>,
}

impl<'a, 'd> Structure<'a, 'd> {
    fn new(schema: &'static Schema, doc: &'a Document<'d>, out: &'a mut Vec<Diagnostic>) -> Self {
        Self { schema, doc, out, checked: false, defs: HashMap::new() }
    }

    /// the structure called `hash`, from the Meta table or the PSO table
    fn def(&mut self, hash: u32) -> Option<std::rc::Rc<Def>> {
        if let Some(d) = self.defs.get(&hash) {
            return d.clone();
        }
        let built = if let Some(m) = self.schema.meta_structs.get(&hash) {
            Some(Def { name: member_name(m.name), members: m.entries.iter().filter(|e| e.name != 0x100).map(|e| (member_name(e.name), meta_kind(e, m))).collect() })
        } else {
            self.schema.pso_structs.get(&hash).map(|p| Def { name: member_name(p.name), members: p.entries.iter().filter(|e| e.name != 0x100).map(|e| (member_name(e.name), pso_kind(e, p))).collect() })
        };
        let rc = built.map(std::rc::Rc::new);
        self.defs.insert(hash, rc.clone());
        rc
    }

    /// name, member names and member hashes of an enum, from either table
    fn enum_members(&self, hash: u32) -> Option<(String, Vec<String>, HashSet<u32>)> {
        let (name, entries): (u32, Vec<u32>) = match (self.schema.meta_enums.get(&hash), self.schema.pso_enums.get(&hash)) {
            (Some(m), _) => (m.name, m.entries.iter().map(|(h, _)| *h).collect()),
            (_, Some(p)) => (p.name, p.entries.iter().map(|(h, _)| *h).collect()),
            _ => return None,
        };
        let hashes: HashSet<u32> = entries.iter().copied().collect();
        Some((member_name(name), entries.iter().map(|h| member_name(*h)).collect(), hashes))
    }

    fn run(&mut self) {
        let root = self.doc.root_element();
        let Some(def) = self.def(rage_joaat(root.tag_name().name())) else { return };
        self.checked = true;
        self.structure(root, &def);
        if root.tag_name().name() == "CMapTypes" {
            self.archetype_names(root);
        }
    }

    fn at(&self, node: Node<'a, 'd>) -> (usize, usize, usize) {
        let p = self.doc.text_pos_at(node.range().start);
        (p.row as usize, p.col as usize + 1, node.tag_name().name().chars().count())
    }

    fn push(&mut self, node: Node<'a, 'd>, severity: Severity, code: &'static str, message: String, fix: String) {
        if self.out.len() >= MAX_DIAGNOSTICS {
            return;
        }
        let (line, col, len) = self.at(node);
        self.out.push(Diagnostic { severity, code, line, col, len, message, fix });
    }

    fn structure(&mut self, node: Node<'a, 'd>, def: &Def) {
        let mut seen: HashSet<&str> = HashSet::new();
        for child in node.children().filter(|c| c.is_element()) {
            let name = child.tag_name().name();
            let Some(kind) = def.members.get(name) else {
                // a different case is its own, clearer mistake
                match def.members.keys().find(|k| k.eq_ignore_ascii_case(name)) {
                    Some(right) => self.push(child, Severity::Info, "META_CASE", format!("<{name}> is spelled <{right}> in the reference tool's tables (the game's own files differ in case too)"), format!("write <{right}> to match the tables")),
                    None => match nearest(name, def.members.keys().map(String::as_str)) {
                        // a near miss is probably a typo; anything else may just be newer than the reference tool's tables
                        Some(n) => self.push(child, Severity::Warning, "META_UNKNOWN_MEMBER", format!("<{name}> is not a member of {}", def.name), format!("did you mean <{n}>? The game ignores members it does not know, so a misspelled one silently does nothing")),
                        None => self.push(child, Severity::Info, "META_UNKNOWN_MEMBER", format!("<{name}> is not a member of {} in the reference tool's tables", def.name), "fine if it is new in your game version; otherwise check the spelling (the game ignores members it does not know)".into()),
                    },
                }
                continue;
            };
            if !seen.insert(name) && !matches!(kind, Kind::Array(_)) {
                self.push(child, Severity::Info, "META_DUPLICATE_MEMBER", format!("<{name}> appears more than once in this {}", def.name), "keep one; only one value is used".into());
            }
            self.member(child, kind);
        }
    }

    fn need_value(&mut self, node: Node<'a, 'd>) -> Option<&'a str> {
        let name = node.tag_name().name();
        let v = node.attribute("value");
        if v.is_none() {
            self.push(node, Severity::Error, "META_NO_VALUE", format!("<{name}> needs a value attribute"), format!("write <{name} value=\"...\"/>"));
        }
        v
    }

    fn member(&mut self, node: Node<'a, 'd>, kind: &Kind) {
        let name = node.tag_name().name();
        match kind {
            Kind::Bool => {
                if let Some(v) = self.need_value(node) {
                    if !matches!(v.trim(), "true" | "false" | "True" | "False" | "0" | "1") {
                        self.push(node, Severity::Error, "META_BAD_BOOL", format!("<{name}> is true or false, not '{v}'"), format!("write <{name} value=\"true\"/> or value=\"false\""));
                    }
                }
            }
            Kind::Int(lo, hi) => {
                if let Some(v) = self.need_value(node) {
                    match parse_int(v) {
                        None => self.push(node, Severity::Error, "META_BAD_INT", format!("<{name}> must be a whole number, not '{v}'"), "use digits (or 0x... for hex), with no decimal point or letters".into()),
                        Some(x) if x < *lo || x > *hi => self.push(node, Severity::Error, "META_INT_RANGE", format!("<{name}> is {x}, outside {lo}..{hi}"), format!("use a value from {lo} to {hi}")),
                        _ => {}
                    }
                }
            }
            Kind::Float => {
                if let Some(v) = self.need_value(node) {
                    if v.trim().parse::<f32>().is_err() {
                        let hint = if v.trim().ends_with('f') { "remove the trailing f; XML numbers are plain (1.5, not 1.5f)" } else { "use a plain number such as 1.5 (a point, not a comma)" };
                        self.push(node, float_severity(v), "META_BAD_FLOAT", format!("<{name}> must be a number, not '{v}'"), hint.into());
                    }
                }
            }
            Kind::Vector(n) => {
                let axes = &["x", "y", "z", "w"][..*n];
                for a in axes {
                    match node.attribute(*a) {
                        None => self.push(node, Severity::Error, "META_VECTOR_AXIS", format!("<{name}> is missing the '{a}' attribute"), format!("write <{name} {}/>", axes.iter().map(|a| format!("{a}=\"0\"")).collect::<Vec<_>>().join(" "))),
                        Some(v) if v.trim().parse::<f32>().is_err() => self.push(node, float_severity(v), "META_BAD_FLOAT", format!("{a}=\"{v}\" in <{name}> is not a number"), "use a plain number such as 1.5".into()),
                        _ => {}
                    }
                }
            }
            Kind::Enum(h) => {
                let text = node.text().unwrap_or("").trim();
                if let Some((ename, valid, hashes)) = self.enum_members(*h) {
                    if !text.is_empty() && parse_int(text).is_none() && !hashes.contains(&rage_joaat(text)) {
                        // a near miss is probably a typo; anything else may be a value newer than the reference tool's tables (the game's own
                        // content.xml has over a thousand)
                        match nearest(text, valid.iter().map(String::as_str)) {
                            Some(n) => self.push(node, Severity::Warning, "META_BAD_ENUM", format!("'{text}' is not a value of {ename}"), format!("did you mean {n}?")),
                            None => self.push(node, Severity::Info, "META_BAD_ENUM", format!("'{text}' is not a value of {ename} in the reference tool's tables"), format!("fine if it is new in your game version; known values include {}", valid.iter().take(6).cloned().collect::<Vec<_>>().join(", "))),
                        }
                    }
                }
            }
            Kind::Flags(h) => {
                let text = node.text().unwrap_or("").trim();
                if let Some((ename, valid, hashes)) = self.enum_members(*h) {
                    for tok in text.split(|c: char| c.is_whitespace() || c == '|' || c == ',').filter(|t| !t.is_empty()) {
                        if parse_int(tok).is_none() && !hashes.contains(&rage_joaat(tok)) {
                            let fix = nearest(tok, valid.iter().map(String::as_str)).map(|n| format!("did you mean {n}?")).unwrap_or_else(|| "check the flag name".into());
                            self.push(node, Severity::Warning, "META_BAD_FLAG", format!("'{tok}' is not a flag of {ename}"), fix);
                        }
                    }
                }
            }
            Kind::Struct(h, by_pointer) => {
                let hash = node.attribute("type").filter(|_| *by_pointer).map(rage_joaat).unwrap_or(*h);
                if let Some(def) = self.def(hash) {
                    self.structure(node, &def);
                }
            }
            Kind::Array(elem) => self.array(node, elem),
            Kind::Text | Kind::Hash | Kind::Other => {}
        }
    }

    fn array(&mut self, node: Node<'a, 'd>, elem: &Kind) {
        let name = node.tag_name().name();
        match elem {
            Kind::Struct(h, by_pointer) => {
                for child in node.children().filter(|c| c.is_element()) {
                    let cn = child.tag_name().name();
                    if cn != "Item" {
                        self.push(child, Severity::Info, "META_ARRAY_ITEM", format!("<{cn}> inside <{name}>: the entries of a list are called <Item>"), "rename it to <Item>; other names are ignored".into());
                        continue;
                    }
                    let hash = match child.attribute("type") {
                        Some(t) if self.def(rage_joaat(t)).is_none() => {
                            self.push(child, Severity::Error, "META_BAD_TYPE", format!("type=\"{t}\" is not a structure the game knows"), "check the spelling of the type name (names are case-sensitive, e.g. CBaseArchetypeDef)".into());
                            continue;
                        }
                        Some(t) => rage_joaat(t),
                        None => {
                            if *by_pointer && *h == 0 {
                                self.push(child, Severity::Warning, "META_NO_TYPE", format!("<Item> in <{name}> has no type attribute; this list can hold different structures"), "add type=\"...\" with the structure name, e.g. type=\"CBaseArchetypeDef\"".into());
                            }
                            *h
                        }
                    };
                    if let Some(def) = self.def(hash) {
                        self.structure(child, &def);
                    }
                }
            }
            Kind::Float | Kind::Int(..) => {
                if node.children().any(|c| c.is_element()) {
                    return;
                }
                for tok in node.text().unwrap_or("").split_whitespace() {
                    let bad = if matches!(elem, Kind::Float) { tok.parse::<f32>().is_err() } else { parse_int(tok).is_none() };
                    if bad {
                        self.push(node, Severity::Error, "META_BAD_ARRAY_VALUE", format!("'{tok}' in <{name}> is not a number"), "the values of this list are numbers separated by spaces".into());
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    /// `.ytyp`: two archetypes with one name make the second unreachable.
    fn archetype_names(&mut self, root: Node<'a, 'd>) {
        let Some(list) = root.children().find(|c| c.is_element() && c.tag_name().name() == "archetypes") else { return };
        let mut seen: HashMap<String, usize> = HashMap::new();
        for item in list.children().filter(|c| c.is_element() && c.tag_name().name() == "Item") {
            let name_el = item.children().find(|c| c.is_element() && c.tag_name().name() == "name");
            let Some(name) = name_el.and_then(|n| n.text()).map(|t| t.trim().to_lowercase()) else { continue };
            if name.is_empty() {
                continue;
            }
            if let Some(first) = seen.get(&name) {
                self.push(name_el.unwrap_or(item), Severity::Warning, "YTYP_DUPLICATE_ARCHETYPE", format!("another archetype is already called '{name}' (line {first}); names are not case-sensitive"), "rename one of them; the game can only find one".into());
            } else {
                seen.insert(name, self.doc.text_pos_at(item.range().start).row as usize);
            }
        }
    }
}

/// The value rules CRCK Explorer already has for specific files (handling ranges and duplicate names, siren settings), shown in the same
/// list. Their findings carry a `(line N)` in the message, which becomes the position.
fn domain_rules(root: &str, text: &str, out: &mut Vec<Diagnostic>) {
    let report = match root {
        "CHandlingDataMgr" => crate::handling::check_sources(&[crate::handling::Source { label: String::new(), text: text.to_string() }]),
        "CVehicleModelInfoVarGlobal" => crate::siren::check_texts(&[(String::new(), text.to_string())], &[]),
        "CVehicleModelInfoVariation" => crate::siren::check_texts(&[], &[(String::new(), text.to_string())]),
        _ => return,
    };
    for i in report.issues {
        let line = i.msg.rfind("(line ").and_then(|k| i.msg[k + 6..].split(')').next()).and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(1);
        let severity = match i.sev {
            'E' => Severity::Error,
            'W' => Severity::Warning,
            _ => Severity::Info,
        };
        out.push(Diagnostic { severity, code: if i.code.is_empty() { "META_RULE" } else { i.code }, line, col: 1, len: 200, message: i.msg, fix: i.fix });
    }
}

/// The game reads the leading number of a malformed value (`-3.5.000` in its own door tuning file), the reference tool refuses it: a warning
/// when there is a number to read, an error when there is none.
fn float_severity(v: &str) -> Severity {
    let t = v.trim();
    let end = t.char_indices().take_while(|(_, c)| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
    if t[..end].chars().any(|c| c.is_ascii_digit()) { Severity::Warning } else { Severity::Error }
}

fn parse_int(s: &str) -> Option<i128> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i128::from_str_radix(h, 16).ok();
    }
    s.parse::<i128>().ok()
}

/// the closest name within two edits, for "did you mean"
fn nearest<'x>(word: &str, candidates: impl Iterator<Item = &'x str>) -> Option<&'x str> {
    let w = word.to_lowercase();
    // names that differ only in their numbers (misc_11 / misc_1, seat_dside_r4 / r5) are different things, not typos
    let no_digits = |t: &str| t.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    let wn = no_digits(&w);
    if w.chars().any(|c| c.is_ascii_digit()) {
        // numbered names (misc_11, extra_9) are usually just newer than the tables
        return None;
    }
    candidates.filter(|c| no_digits(&c.to_lowercase()) != wn).map(|c| (edit_distance(&w, &c.to_lowercase()), c)).filter(|(d, _)| *d <= 2.max(w.len() / 4)).min_by_key(|(d, _)| *d).map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<&'static str> {
        check(text).diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn well_formed_text_has_no_findings() {
        let r = check("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Root a=\"1\" b='2'>\n  <!-- hi -->\n  <Item/>\n  <x>a &amp; b</x>\n</Root>\n");
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        assert_eq!(r.root.as_deref(), Some("Root"));
    }

    #[test]
    fn every_syntax_mistake_is_found_with_a_position_and_a_fix() {
        // missing closing tag
        let r = check("<a>\n  <b>\n</a>\n");
        assert!(r.diagnostics.iter().any(|d| d.code == "XML_UNCLOSED" && d.line == 2 && !d.fix.is_empty()), "{:?}", r.diagnostics);
        // mismatched close
        let r = check("<a>\n  <b></c>\n</a>\n");
        assert!(r.diagnostics.iter().any(|d| d.code == "XML_MISMATCH" && d.line == 2), "{:?}", r.diagnostics);
        // missing quote
        let r = check("<a>\n  <b x=\"1></b>\n</a>\n");
        assert!(r.diagnostics.iter().any(|d| d.code == "XML_ATTR_QUOTE" && d.line == 2), "{:?}", r.diagnostics);
        // unquoted value, no value, bare ampersand, second root
        assert!(codes("<a x=1/>").contains(&"XML_ATTR_UNQUOTED"));
        assert!(codes("<a x/>").contains(&"XML_ATTR_NO_VALUE"));
        assert!(codes("<a>fish & chips</a>").contains(&"XML_AMPERSAND"));
        assert!(codes("<a/><b/>").contains(&"XML_SECOND_ROOT"));
        assert!(codes("<a x=\"1\" x=\"2\"/>").contains(&"XML_ATTR_DUPLICATE"));
        assert!(codes("<a><!-- x -- y --></a>").contains(&"XML_COMMENT_DASHES"));
        // several mistakes are all reported, not just the first
        let r = check("<a>\n  <b x=1>\n  <c y>\n</a>\n");
        assert!(r.diagnostics.len() >= 3, "{:?}", r.diagnostics);
    }

    #[test]
    fn the_strict_parser_has_the_last_word() {
        // the scanner never reports a false positive on text a strict parser accepts
        for ok in ["<a b=\"x &lt; y\"/>", "<a><![CDATA[ a < b & c ]]></a>", "<?xml version=\"1.0\"?><a><b/></a>", "\u{feff}<a/>"] {
            assert!(check(ok).diagnostics.is_empty(), "{ok}: {:?}", check(ok).diagnostics);
        }
    }

    #[test]
    fn structure_errors_are_specific_to_the_game_format() {
        // a ytyp: a bad number, a misspelled enum value and a misspelled member are each found, with the line
        let t = "<CMapTypes>
  <archetypes>
    <Item type=\"CBaseArchetypeDef\">
      <lodDist value=\"abc\"/>
      <assetType>ASSET_TYPE_DRAWBLE</assetType>
      <textureDictionry>x</textureDictionry>
    </Item>
  </archetypes>
</CMapTypes>";
        let r = check(t);
        assert!(r.structure_checked);
        assert!(r.diagnostics.iter().any(|d| d.code == "META_BAD_FLOAT" && d.line == 4), "{:?}", r.diagnostics);
        let e = r.diagnostics.iter().find(|d| d.code == "META_BAD_ENUM").expect("a bad enum value is reported");
        assert!(e.line == 5 && e.fix.contains("ASSET_TYPE_DRAWABLE"), "{e:?}");
        let typo = r.diagnostics.iter().find(|d| d.code == "META_UNKNOWN_MEMBER").expect("a misspelled member is reported");
        assert!(typo.line == 6 && typo.fix.contains("textureDictionary"), "{typo:?}");
        // a PSO-defined file (carcols): a misspelled member
        let r = check("<CVehicleModelInfoVarGlobal>
  <Sirns/>
</CVehicleModelInfoVarGlobal>");
        assert!(r.structure_checked);
        assert!(r.diagnostics.iter().any(|d| d.code == "META_UNKNOWN_MEMBER" && d.fix.contains("Sirens")), "{:?}", r.diagnostics);
        // an unknown root is syntax-checked only
        let r = check("<Whatever><x/></Whatever>");
        assert!(!r.structure_checked && r.diagnostics.is_empty());
    }

    #[test]
    fn handling_rules_show_up_in_the_same_list() {
        let h = "<CHandlingDataMgr>
  <HandlingData>
    <Item type=\"CHandlingData\">
      <handlingName>A</handlingName>
    </Item>
    <Item type=\"CHandlingData\">
      <handlingName>a</handlingName>
    </Item>
  </HandlingData>
</CHandlingDataMgr>";
        let r = check(h);
        assert!(r.diagnostics.iter().any(|d| d.code == "HANDLING_DUPLICATE_NAME"), "{:?}", r.diagnostics);
    }

    #[test]
    fn duplicate_archetype_names_in_a_ytyp_are_flagged() {
        let t = "<CMapTypes><archetypes>\n<Item type=\"CBaseArchetypeDef\"><name>prop_a</name></Item>\n<Item type=\"CBaseArchetypeDef\"><name>PROP_A</name></Item>\n</archetypes></CMapTypes>";
        assert!(codes(t).contains(&"YTYP_DUPLICATE_ARCHETYPE"));
    }
}
