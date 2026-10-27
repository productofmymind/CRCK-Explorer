//! handling.meta (`CHandlingDataMgr`): parse, check and compare. Severity: E = the entry is unusable or ambiguous, W = likely a mistake,
//! I = worth knowing (same scale and `Report` as `validate`). Names compare case-insensitively.
//! Every element is kept in document order (known or not), so nothing in a file is lost by reading it. `strModelFlags` /
//! `strHandlingFlags` / `strDamageFlags` are carried as the raw hex text: their bits are NOT decoded (positions are unconfirmed).
//! Range warnings use `BANDS`, which were calibrated against the vanilla files of a real install (see `ranges` and
//! the calibration notes); they are not the Handling-Tools slider ranges and not engine limits.
use crate::{
    game::Game,
    validate::{Issue, Report},
};
use anyhow::{bail, Context, Result};
use roxmltree::{Document, Node};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

// ---- model -----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `<fMass value="1800.0" />`
    Num(f64),
    /// `<vecCentreOfMassOffset x= y= z= />`
    Vec3([f64; 3]),
    /// `<strModelFlags>440010</strModelFlags>`
    Text(String),
    /// a `value=` / `x= y= z=` element whose number does not parse (raw attribute text)
    Bad(String),
    /// anything else (child elements, extra attributes): the element's source text
    Raw(String),
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub value: Value,
    pub line: usize,
}

/// One `<Item type="...">` of `<SubHandlingData>` (car / bike / boat / flying / ... extras; `NULL` for empty slots).
#[derive(Debug, Clone)]
pub struct Sub {
    pub kind: String,
    pub line: usize,
    pub fields: Vec<Field>,
}

/// One `<Item type="CHandlingData">`.
#[derive(Debug, Clone)]
pub struct Handling {
    pub name: String,
    pub line: usize,
    pub item_type: String,
    /// every element except `handlingName` and `SubHandlingData`, in file order
    pub fields: Vec<Field>,
    pub sub: Vec<Sub>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Car,
    Bike,
    Boat,
    Flying,
    Submarine,
    Trailer,
}

impl Kind {
    pub fn name(self) -> &'static str {
        ["car", "bike", "boat", "flying", "submarine", "trailer"][self as usize]
    }

    /// which column of `BANDS` applies: cars, bikes, everything else (boats, aircraft, submarines, trailers)
    fn tier(self) -> usize {
        match self {
            Kind::Car => 0,
            Kind::Bike => 1,
            _ => 2,
        }
    }
}

impl Handling {
    /// first field called `name` (case-insensitive): top level first, then the sub-handling extras
    pub fn field(&self, name: &str) -> Option<&Field> {
        let by_name = |f: &&Field| f.name.eq_ignore_ascii_case(name);
        self.fields.iter().find(by_name).or_else(|| self.sub.iter().flat_map(|s| s.fields.iter()).find(by_name))
    }

    pub fn num(&self, name: &str) -> Option<f64> {
        match self.field(name)?.value {
            Value::Num(v) => Some(v),
            _ => None,
        }
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        match &self.field(name)?.value {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Class of vehicle, from the sub-handling types (the file has no other marker).
    pub fn kind(&self) -> Kind {
        let has = |t: &str| self.sub.iter().any(|s| s.kind.eq_ignore_ascii_case(t));
        if has("CBoatHandlingData") {
            Kind::Boat
        } else if has("CFlyingHandlingData") || has("CSpecialFlightHandlingData") {
            Kind::Flying
        } else if has("CSubmarineHandlingData") {
            Kind::Submarine
        } else if has("CTrailerHandlingData") {
            Kind::Trailer
        } else if has("CBikeHandlingData") {
            Kind::Bike
        } else {
            Kind::Car
        }
    }
}

pub struct Parsed {
    pub root: String,
    pub items: Vec<Handling>,
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Decimal number, finite. A trailing `f` is accepted because vanilla writes some values as `-30.0f`.
fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    s.strip_suffix(['f', 'F']).unwrap_or(s).parse::<f64>().ok().filter(|v| v.is_finite())
}

/// byte offset of the start of every line, so a line number is a binary search (roxmltree's `text_pos_at` rescans the file each time:
/// quadratic on a 700 KB handling.meta)
fn line_starts(src: &str) -> Vec<usize> {
    std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

fn line(ix: &[usize], n: Node) -> usize {
    ix.partition_point(|&s| s <= n.range().start)
}

fn field_of(ix: &[usize], src: &str, n: Node) -> Field {
    let attrs: Vec<_> = n.attributes().collect();
    let attr = |k: &str| n.attribute(k);
    let value = if n.children().any(|c| c.is_element()) {
        Value::Raw(src[n.range()].to_string())
    } else if attrs.len() == 1 && attr("value").is_some() {
        let raw = attr("value").unwrap();
        number(raw).map(Value::Num).unwrap_or_else(|| Value::Bad(raw.to_string()))
    } else if attrs.len() == 3 && attr("x").is_some() && attr("y").is_some() && attr("z").is_some() {
        let (x, y, z) = (attr("x").unwrap(), attr("y").unwrap(), attr("z").unwrap());
        match (number(x), number(y), number(z)) {
            (Some(a), Some(b), Some(c)) => Value::Vec3([a, b, c]),
            _ => Value::Bad(format!("x=\"{x}\" y=\"{y}\" z=\"{z}\"")),
        }
    } else if attrs.is_empty() {
        Value::Text(n.text().unwrap_or("").trim().to_string())
    } else {
        Value::Raw(src[n.range()].to_string())
    };
    Field { name: n.tag_name().name().to_string(), value, line: line(ix, n) }
}

fn item_of(ix: &[usize], src: &str, n: Node) -> Handling {
    let mut h = Handling { name: String::new(), line: line(ix, n), item_type: n.attribute("type").unwrap_or("").to_string(), fields: vec![], sub: vec![] };
    for c in n.children().filter(|c| c.is_element()) {
        let tag = c.tag_name().name();
        if eq(tag, "handlingName") {
            h.name = c.text().unwrap_or("").trim().to_string();
        } else if eq(tag, "SubHandlingData") {
            for s in c.children().filter(|s| s.is_element() && eq(s.tag_name().name(), "Item")) {
                let fields = s.children().filter(|f| f.is_element()).map(|f| field_of(ix, src, f)).collect();
                h.sub.push(Sub { kind: s.attribute("type").unwrap_or("").to_string(), line: line(ix, s), fields });
            }
        } else {
            h.fields.push(field_of(ix, src, c));
        }
    }
    h
}

/// Err = not well-formed XML. A wrong root element is not an error here (see `Parsed::root`).
pub fn parse(src: &str) -> std::result::Result<Parsed, String> {
    let src = src.trim_start_matches('\u{feff}');
    let doc = Document::parse(src).map_err(|e| e.to_string())?;
    let ix = line_starts(src);
    let root = doc.root_element();
    let items = root
        .children()
        .filter(|c| c.is_element() && eq(c.tag_name().name(), "HandlingData"))
        .flat_map(|h| h.children().filter(|c| c.is_element() && eq(c.tag_name().name(), "Item")).collect::<Vec<_>>())
        .map(|n| item_of(&ix, src, n))
        .collect();
    Ok(Parsed { root: root.tag_name().name().to_string(), items })
}

// ---- field lists and calibrated bands ---------------------------------------------------------------------------

/// numeric, present in every vanilla entry and needed for the vehicle to drive: missing or bad = E
const CORE: &[&str] = &[
    "fMass", "fInitialDragCoeff", "fPercentSubmerged", "fDriveBiasFront", "nInitialDriveGears", "fInitialDriveForce", "fInitialDriveMaxFlatVel", "fBrakeForce", "fBrakeBiasFront", "fHandBrakeForce",
    "fSteeringLock", "fTractionCurveMax", "fTractionCurveMin", "fTractionCurveLateral", "fTractionBiasFront", "fTractionLossMult", "fSuspensionForce", "fSuspensionCompDamp", "fSuspensionReboundDamp",
    "fSuspensionUpperLimit", "fSuspensionLowerLimit", "fSuspensionBiasFront", "fAntiRollBarForce", "fAntiRollBarBiasFront",
];const CORE_VEC: &[&str] = &["vecCentreOfMassOffset", "vecInertiaMultiplier"];
const FLAGS: &[&str] = &["strModelFlags", "strHandlingFlags", "strDamageFlags"];
/// numeric, present in every vanilla entry but not essential: missing = W (the spelling `fCamberStiffnesss` is vanilla's)
const EXPECTED: &[&str] = &[
    "fDriveInertia", "fClutchChangeRateScaleUpShift", "fClutchChangeRateScaleDownShift", "fTractionSpringDeltaMax", "fLowSpeedTractionLossMult", "fCamberStiffnesss", "fSuspensionRaise", "fRollCentreHeightFront",
    "fRollCentreHeightRear", "fCollisionDamageMult", "fWeaponDamageMult", "fDeformationDamageMult", "fEngineDamageMult", "fPetrolTankVolume", "fOilVolume", "nMonetaryValue",
];
/// numeric, present in some vanilla entries only
const OPTIONAL: &[&str] = &["fSeatOffsetDistX", "fSeatOffsetDistY", "fSeatOffsetDistZ", "fDownforceModifier", "fBoostMaxSpeed", "fRocketBoostCapacity", "fPopUpLightRotation", "fWeaponDamageScaledToVehHealthMult"];

fn is_known(name: &str) -> bool {
    eq(name, "AIHandling") || [CORE, CORE_VEC, FLAGS, EXPECTED, OPTIONAL].iter().any(|l| l.iter().any(|k| eq(k, name)))
}

type Band = (f64, f64);

/// Warning thresholds per field: (field, cars, bikes, everything else). Each is the min..max found in the vanilla files of the
/// author's install for that class, widened by 25% and rounded outward to 1/1.2/1.5/2/2.5/3/4/5/6/8 x 10^n; 0..1 for the
/// front-bias fractions; 0 where the vanilla minimum is under 1% of the maximum (except fMass). "Everything else" is also the
/// union of the other two. `ranges` prints the evidence; each widening is explained in the code.
const BANDS: &[(&str, Band, Band, Band)] = &[
    ("fMass", (60.0, 80000.0), (80.0, 1500.0), (60.0, 80000.0)),
    ("fInitialDragCoeff", (0.0, 400.0), (0.6, 100.0), (0.0, 400.0)),
    ("fPercentSubmerged", (60.0, 250.0), (40.0, 250.0), (30.0, 250.0)),
    ("fDriveBiasFront", (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)),
    ("nInitialDriveGears", (1.0, 10.0), (1.0, 8.0), (1.0, 10.0)),
    ("fInitialDriveForce", (0.04, 1.0), (0.0, 20.0), (0.0, 25.0)),
    ("fInitialDriveMaxFlatVel", (12.0, 250.0), (30.0, 200.0), (8.0, 500.0)),
    ("fBrakeForce", (0.08, 8.0), (0.2, 4.0), (0.0, 8.0)),
    ("fBrakeBiasFront", (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)),
    ("fHandBrakeForce", (0.08, 2.0), (0.4, 8.0), (0.0, 8.0)),
    ("fSteeringLock", (15.0, 120.0), (15.0, 80.0), (15.0, 120.0)),
    ("fTractionCurveMax", (0.8, 4.0), (0.0, 4.0), (0.0, 5.0)),
    ("fTractionCurveMin", (0.6, 4.0), (0.0, 4.0), (0.0, 5.0)),
    ("fTractionCurveLateral", (6.0, 50.0), (15.0, 150.0), (0.0, 150.0)),
    ("fTractionBiasFront", (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)),
    ("fTractionLossMult", (0.0, 2.0), (0.0, 1.5), (0.0, 2.0)),
    ("fSuspensionForce", (0.3, 8.0), (0.0, 12.0), (0.0, 12.0)),
    ("fSuspensionCompDamp", (0.4, 8.0), (0.0, 10.0), (0.0, 10.0)),
    ("fSuspensionReboundDamp", (0.5, 10.0), (0.0, 15.0), (0.0, 15.0)),
    ("fSuspensionUpperLimit", (0.015, 0.5), (0.04, 0.3), (0.0, 1.0)),
    ("fSuspensionLowerLimit", (-0.5, -0.02), (-0.4, 0.15), (-0.5, 0.15)),
    ("fSuspensionBiasFront", (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)),
    ("fAntiRollBarForce", (0.0, 4.0), (0.0, 1.5), (0.0, 4.0)),
    ("fAntiRollBarBiasFront", (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)),
    ("fDownforceModifier", (0.0, 600.0), (0.0, 600.0), (0.0, 600.0)),
];

/// The Handling-Tools editor's slider ranges as given to us (UI ranges, not engine limits). Only used by `ranges`, to show how
/// many vanilla vehicles a slider range would have flagged.
const UI_RANGES: &[(&str, Band)] = &[
    ("fMass", (0.0, 15000.0)),
    ("fSteeringLock", (0.0, 50.0)),
    ("fTractionCurveMax", (0.0, 3.0)),
    ("fTractionCurveMin", (0.0, 3.0)),
    ("fTractionCurveLateral", (10.0, 30.0)),
    ("fDriveBiasFront", (0.0, 1.0)),
    ("fBrakeBiasFront", (0.0, 1.0)),
    ("nInitialDriveGears", (0.0, 6.0)),
    ("fInitialDriveForce", (0.0, 0.5)),
    ("fInitialDriveMaxFlatVel", (0.0, 200.0)),
    ("fBrakeForce", (0.0, 1.0)),
    ("fDownforceModifier", (0.0, 5.0)),
    ("fInitialDragCoeff", (0.0, 20.0)),
    ("fSuspensionForce", (0.0, 5.0)),
];

const TIERS: [&str; 3] = ["car", "bike", "other"];

fn band_of(field: &str, tier: usize) -> Option<Band> {
    BANDS.iter().find(|b| eq(b.0, field)).map(|b| [b.1, b.2, b.3][tier])
}

/// The range a value is expected to stay within for this class of vehicle (the warning band `check` uses), if the field has one.
pub fn band(field: &str, kind: Kind) -> Option<(f64, f64)> {
    band_of(field, kind.tier())
}

// ---- check -----------------------------------------------------------------------------------------------------

pub struct Source {
    pub label: String,
    pub text: String,
}

fn fmt(v: f64) -> String {
    format!("{v}")
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Checks every source together (a handlingName used twice anywhere in them is an error).
pub fn check_sources(sources: &[Source]) -> Report {
    let mut issues: Vec<Issue> = vec![];
    let mut add = |sev: char, code: &'static str, file: &str, msg: String, fix: &str| issues.push(Issue { sev, code, file: file.into(), msg, fix: fix.into() });
    let mut seen: HashMap<String, (String, usize)> = HashMap::new();
    let mut total = 0;
    for s in sources {
        let p = match parse(&s.text) {
            Ok(p) => p,
            Err(e) => {
                add('E', "HANDLING_BAD_XML", &s.label, format!("not valid XML: {e}"), "fix the XML; the game ignores or crashes on broken metas");
                continue;
            }
        };
        if p.root != "CHandlingDataMgr" {
            add('E', "HANDLING_BAD_ROOT", &s.label, format!("root element is <{}>, expected <CHandlingDataMgr>", p.root), "wrong template or wrong file type");
            continue;
        }
        if p.items.is_empty() {
            add('W', "HANDLING_EMPTY", &s.label, "no <HandlingData><Item> entries".into(), "a handling file without entries does nothing");
        }
        for h in &p.items {
            total += 1;
            check_item(h, &s.label, &mut add);
            let who = if h.name.is_empty() { "<unnamed>" } else { h.name.as_str() };
            if h.name.is_empty() {
                add('E', "HANDLING_MISSING_FIELD", &s.label, format!("{who}: handlingName missing or empty (line {})", h.line), "vehicles.meta <handlingId> finds the entry by this name");
            } else if let Some((f0, l0)) = seen.get(&h.name.to_lowercase()) {
                add('E', "HANDLING_DUPLICATE_NAME", &s.label, format!("{}: handlingName already defined in {f0} (line {l0}); again here (line {})", h.name, h.line), "names are case-insensitive and only one entry can win: rename one and update its <handlingId>");
            } else {
                seen.insert(h.name.to_lowercase(), (s.label.clone(), h.line));
            }
        }
    }
    let rank = |c: char| "EWI".find(c).unwrap_or(3);
    issues.sort_by_key(|i| rank(i.sev));
    let n = |c: char| issues.iter().filter(|i| i.sev == c).count();
    let (e, w, i) = (n('E'), n('W'), n('I'));
    Report { summary: format!("{total} handling entr{} in {} file(s): {e} error(s), {w} warning(s), {i} info", if total == 1 { "y" } else { "ies" }, sources.len()), errors: e, issues }
}

fn check_item(h: &Handling, file: &str, add: &mut impl FnMut(char, &'static str, &str, String, &str)) {
    let who = if h.name.is_empty() { "<unnamed>" } else { h.name.as_str() };
    if h.item_type != "CHandlingData" {
        add('W', "HANDLING_ITEM_TYPE", file, format!("{who}: <Item type=\"{}\"> (line {}), expected type=\"CHandlingData\"", h.item_type, h.line), "the game reads the item by its type attribute");
    }
    let tier = h.kind().tier();
    for (list, missing) in [(CORE, Some('E')), (EXPECTED, Some('W')), (OPTIONAL, None)] {
        for &name in list {
            let Some(f) = h.fields.iter().find(|f| eq(&f.name, name)) else {
                if let Some(sev) = missing {
                    let fix = if sev == 'E' { "add it; copy the element from a vanilla vehicle of the same kind" } else { "every vanilla entry has it; copy it from a similar vehicle" };
                    add(sev, "HANDLING_MISSING_FIELD", file, format!("{who}: {name} missing (line {})", h.line), fix);
                }
                continue;
            };
            match &f.value {
                Value::Num(v) => {
                    if name.starts_with('n') && v.fract() != 0.0 {
                        add('W', "HANDLING_BAD_VALUE", file, format!("{who}: {name} is {}, a whole number is expected (line {})", fmt(*v), f.line), "the game reads it as an integer");
                    }
                    if let Some((lo, hi)) = band_of(name, tier) {
                        if *v < lo || *v > hi {
                            add(
                                'W',
                                "HANDLING_OUT_OF_RANGE",
                                file,
                                format!("{who}: {name} is {}, outside {}..{} for a {} (line {})", fmt(*v), fmt(lo), fmt(hi), TIERS[tier], f.line),
                                "every vanilla vehicle of this kind is inside that range: typo or wrong units? fine if intended",
                            );
                        }
                    }
                }
                Value::Bad(raw) => add('E', "HANDLING_BAD_VALUE", file, format!("{who}: {name} value \"{raw}\" is not a number (line {})", f.line), "use a plain decimal number, e.g. value=\"1.500000\""),
                _ => add('E', "HANDLING_BAD_VALUE", file, format!("{who}: {name} is not a <{name} value=\"...\" /> element (line {})", f.line), "write it as <name value=\"number\" />"),
            }
        }
    }
    for &name in CORE_VEC {
        match h.fields.iter().find(|f| eq(&f.name, name)).map(|f| (&f.value, f.line)) {
            None => add('E', "HANDLING_MISSING_FIELD", file, format!("{who}: {name} missing (line {})", h.line), "add <name x=\"0.0\" y=\"0.0\" z=\"0.0\" />"),
            Some((Value::Vec3(_), _)) => {}
            Some((_, l)) => add('E', "HANDLING_BAD_VALUE", file, format!("{who}: {name} needs numeric x, y and z attributes (line {l})"), "write it as <name x=\"0.0\" y=\"0.0\" z=\"0.0\" />"),
        }
    }
    for &name in FLAGS {
        match h.fields.iter().find(|f| eq(&f.name, name)).map(|f| (&f.value, f.line)) {
            None => add('E', "HANDLING_MISSING_FIELD", file, format!("{who}: {name} missing (line {})", h.line), "add it as hex digits, e.g. <name>0</name>"),
            Some((Value::Text(t), l)) if !is_hex(t) => {
                let sev = if t.is_empty() { 'W' } else { 'E' };
                add(sev, "HANDLING_BAD_FLAGS", file, format!("{who}: {name} is \"{t}\", expected hex digits (line {l})"), "flags are plain hex digits, as in vanilla (e.g. 440010)");
            }
            Some((Value::Text(_), _)) => {}
            Some((_, l)) => add('E', "HANDLING_BAD_FLAGS", file, format!("{who}: {name} must be text holding hex digits (line {l})"), "write it as <name>440010</name>"),
        }
    }
    if h.fields.iter().all(|f| !eq(&f.name, "AIHandling")) {
        add('W', "HANDLING_MISSING_FIELD", file, format!("{who}: AIHandling missing (line {})", h.line), "every vanilla entry has it (e.g. AVERAGE)");
    }
    let mut count: HashMap<String, usize> = HashMap::new();
    for f in &h.fields {
        *count.entry(f.name.to_lowercase()).or_default() += 1;
    }
    let mut reported: Vec<String> = vec![];
    for f in &h.fields {
        let key = f.name.to_lowercase();
        if count[&key] > 1 && !reported.contains(&key) {
            reported.push(key);
            add('W', "HANDLING_DUPLICATE_FIELD", file, format!("{who}: {} appears {} times (line {})", f.name, count[&f.name.to_lowercase()], f.line), "keep one; which one the game uses is not documented");
        }
        if !is_known(&f.name) {
            add('I', "HANDLING_UNKNOWN_FIELD", file, format!("{who}: {} is not in any vanilla handling entry (line {})", f.name, f.line), "typo? unknown elements are ignored by these tools");
        }
    }
    for s in &h.sub {
        for f in s.fields.iter().filter(|f| matches!(f.value, Value::Bad(_))) {
            let Value::Bad(raw) = &f.value else { continue };
            add('E', "HANDLING_BAD_VALUE", file, format!("{who}: {} in {} value \"{raw}\" is not a number (line {})", f.name, s.kind, f.line), "use a plain decimal number");
        }
    }
}

/// `*.meta` files with `handling` in the name below a folder, or the file itself. Labels are relative to the folder.
pub fn read_sources(path: &Path) -> Result<Vec<Source>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut es: Vec<_> = fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).collect()).unwrap_or_default();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            let n = e.file_name().to_string_lossy().to_lowercase();
            if p.is_dir() {
                walk(&p, out);
            } else if n.contains("handling") && n.ends_with(".meta") {
                out.push(p);
            }
        }
    }
    let read = |p: &Path, label: String| -> Result<Source> {
        let bytes = fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        Ok(Source { label, text: String::from_utf8_lossy(&bytes).into_owned() })
    };
    if path.is_dir() {
        let mut files = vec![];
        walk(path, &mut files);
        if files.is_empty() {
            bail!("no *handling*.meta file under {}", path.display());
        }
        files.iter().map(|p| read(p, p.strip_prefix(path).unwrap_or(p).to_string_lossy().replace('\\', "/"))).collect()
    } else {
        Ok(vec![read(path, path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?])
    }
}

/// Convenience: files and folders in, one report out.
pub fn check(paths: &[PathBuf]) -> Result<Report> {
    let mut src = vec![];
    for p in paths {
        src.extend(read_sources(p)?);
    }
    Ok(check_sources(&src))
}

// ---- compare / ranges ------------------------------------------------------------------------------------------

/// A parsed entry with where it came from.
pub struct Entry {
    pub h: Handling,
    pub source: String,
    pub vanilla: bool,
}

/// `common.rpf/data/handling.meta` -> `game:base`, `update/x64/dlcpacks/mp2023_01/dlc.rpf/...` -> `game:mp2023_01`, ...
fn game_label(path: &str) -> String {
    let seg: Vec<&str> = path.split('/').collect();
    let after = |k: &str| seg.iter().position(|s| s.eq_ignore_ascii_case(k)).and_then(|i| seg.get(i + 1)).map(|s| s.to_string());
    let name = after("dlcpacks").or_else(|| after("dlc_patch").map(|s| format!("patch:{s}"))).unwrap_or_else(|| if path.starts_with("update/update.rpf") { "update".into() } else { "base".into() });
    format!("game:{name}")
}

/// Every handling file in the user's own game, in load order (base, then updates and DLC; later overrides earlier).
pub fn game_sources(dir: Option<&Path>) -> Result<Vec<Source>> {
    let g = Game::open(dir)?;
    let mut out = vec![];
    for f in g.find_where(&|leaf| leaf.starts_with("handling") && leaf.ends_with(".meta")) {
        let bytes = g.read(&f.path)?;
        out.push(Source { label: game_label(&f.path), text: String::from_utf8_lossy(&bytes).into_owned() });
    }
    Ok(out)
}

fn entries_of(sources: &[Source], vanilla: bool) -> Result<Vec<Entry>> {
    let mut out = vec![];
    for s in sources {
        let p = parse(&s.text).map_err(|e| anyhow::anyhow!("{}: not valid XML: {e}", s.label))?;
        if p.root != "CHandlingDataMgr" {
            bail!("{}: root element is <{}>, expected <CHandlingDataMgr>", s.label, p.root);
        }
        out.extend(p.items.into_iter().map(|h| Entry { h, source: s.label.clone(), vanilla }));
    }
    Ok(out)
}

/// The user's files first (every entry), then the game's (one entry per handlingName, the last definition wins).
pub fn load(paths: &[PathBuf], game: bool, game_dir: Option<&Path>) -> Result<Vec<Entry>> {
    let mut src = vec![];
    for p in paths {
        src.extend(read_sources(p)?);
    }
    let mut out = entries_of(&src, false)?;
    if game {
        let mut idx: HashMap<String, usize> = HashMap::new();
        let mut vanilla: Vec<Entry> = vec![];
        for e in entries_of(&game_sources(game_dir)?, true)? {
            match idx.get(&e.h.name.to_lowercase()) {
                Some(&i) => vanilla[i] = e,
                None => {
                    idx.insert(e.h.name.to_lowercase(), vanilla.len());
                    vanilla.push(e);
                }
            }
        }
        out.extend(vanilla);
    }
    if out.is_empty() {
        bail!("nothing to read: give a handling.meta, a folder, or --game");
    }
    Ok(out)
}

pub const DEFAULT_COLUMNS: &[&str] = &["mass", "drive-force", "top-speed", "brake-force", "traction-max", "drive-bias", "gears", "steering-lock"];

/// short names for the columns people ask for most; any raw field name (`fMass`, `strModelFlags`, ...) works too
const ALIASES: &[(&str, &str)] = &[
    ("mass", "fMass"),
    ("drag", "fInitialDragCoeff"),
    ("driveforce", "fInitialDriveForce"),
    ("topspeed", "fInitialDriveMaxFlatVel"),
    ("brakeforce", "fBrakeForce"),
    ("handbrake", "fHandBrakeForce"),
    ("tractionmax", "fTractionCurveMax"),
    ("tractionmin", "fTractionCurveMin"),
    ("tractionlateral", "fTractionCurveLateral"),
    ("drivebias", "fDriveBiasFront"),
    ("brakebias", "fBrakeBiasFront"),
    ("gears", "nInitialDriveGears"),
    ("steeringlock", "fSteeringLock"),
    ("downforce", "fDownforceModifier"),
    ("modelflags", "strModelFlags"),
    ("handlingflags", "strHandlingFlags"),
    ("damageflags", "strDamageFlags"),
];

fn canonical(col: &str) -> String {
    let key: String = col.chars().filter(|c| *c != '-' && *c != '_').collect::<String>().to_lowercase();
    if let Some(a) = ALIASES.iter().find(|a| a.0 == key) {
        return a.1.to_string();
    }
    if ["name", "kind", "source"].contains(&key.as_str()) {
        return key;
    }
    // a raw field name keeps vanilla's spelling when we know it
    [CORE, CORE_VEC, FLAGS, EXPECTED, OPTIONAL].iter().flat_map(|l| l.iter()).find(|k| eq(k, col)).map(|k| k.to_string()).unwrap_or_else(|| col.to_string())
}

#[derive(Clone, Default, Debug)]
pub struct Cell {
    pub text: String,
    pub num: Option<f64>,
}

fn cell(e: &Entry, col: &str) -> Cell {
    let text = |t: String| Cell { text: t, num: None };
    match col {
        "name" => text(e.h.name.clone()),
        "kind" => text(e.h.kind().name().into()),
        "source" => text(e.source.clone()),
        _ => match e.h.field(col).map(|f| &f.value) {
            Some(Value::Num(v)) => Cell { text: fmt(*v), num: Some(*v) },
            Some(Value::Vec3(v)) => text(format!("{}/{}/{}", fmt(v[0]), fmt(v[1]), fmt(v[2]))),
            Some(Value::Text(s)) | Some(Value::Bad(s)) => text(s.clone()),
            Some(Value::Raw(_)) => text("(complex)".into()),
            None => Cell::default(),
        },
    }
}

pub struct CompareOptions {
    pub paths: Vec<PathBuf>,
    pub game: bool,
    pub game_dir: Option<PathBuf>,
    /// aliases or raw field names; empty = `DEFAULT_COLUMNS`
    pub fields: Vec<String>,
    /// any column (alias, raw name, `name`, `kind`, `source`)
    pub sort: Option<String>,
    pub desc: bool,
    /// keep rows whose name contains any of these (case-insensitive)
    pub filter: Vec<String>,
    pub limit: Option<usize>,
}

#[derive(Debug)]
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

pub fn compare(o: &CompareOptions) -> Result<Table> {
    let mut entries = load(&o.paths, o.game, o.game_dir.as_deref())?;
    if !o.filter.is_empty() {
        let f: Vec<String> = o.filter.iter().map(|s| s.to_lowercase()).collect();
        entries.retain(|e| f.iter().any(|s| e.h.name.to_lowercase().contains(s)));
        if entries.is_empty() {
            bail!("no vehicle name contains {}", o.filter.join(" or "));
        }
    }
    let mut cols: Vec<String> = vec!["name".into(), "kind".into()];
    for c in if o.fields.is_empty() { DEFAULT_COLUMNS.iter().map(|s| s.to_string()).collect() } else { o.fields.clone() } {
        let c = canonical(&c);
        if c != "name" && c != "source" && !cols.contains(&c) {
            cols.push(c);
        }
    }
    cols.push("source".into());
    for c in &cols {
        if !["name", "kind", "source"].contains(&c.as_str()) && entries.iter().all(|e| e.h.field(c).is_none()) {
            bail!("no vehicle has a field called '{c}' (short names: {})", ALIASES.iter().map(|a| a.0).collect::<Vec<_>>().join(", "));
        }
    }
    let mut rows: Vec<Vec<Cell>> = entries.iter().map(|e| cols.iter().map(|c| cell(e, c)).collect()).collect();
    if let Some(s) = &o.sort {
        let s = canonical(s);
        let Some(i) = cols.iter().position(|c| eq(c, &s)) else { bail!("cannot sort by '{s}': it is not one of the columns ({})", cols.join(", ")) };
        // empty cells always last; numbers by value, text case-insensitively
        rows.sort_by(|a, b| {
            let (x, y) = (&a[i], &b[i]);
            let ord = match (x.num, y.num) {
                (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
                _ => x.text.to_lowercase().cmp(&y.text.to_lowercase()),
            };
            match (x.text.is_empty(), y.text.is_empty()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ if o.desc => ord.reverse(),
                _ => ord,
            }
        });
    }
    if let Some(n) = o.limit {
        rows.truncate(n);
    }
    Ok(Table { header: cols, rows })
}

impl Table {
    /// aligned text; number columns right-aligned, empty cells as `-`
    pub fn render(&self) -> String {
        let n = self.header.len();
        let show = |c: &Cell| if c.text.is_empty() { "-".to_string() } else { c.text.clone() };
        let w: Vec<usize> = (0..n).map(|i| self.rows.iter().map(|r| show(&r[i]).chars().count()).chain([self.header[i].chars().count()]).max().unwrap_or(0)).collect();
        let numeric: Vec<bool> = (0..n).map(|i| self.rows.iter().any(|r| r[i].num.is_some()) && self.rows.iter().all(|r| r[i].num.is_some() || r[i].text.is_empty())).collect();
        let pad = |s: &str, i: usize| if numeric[i] { format!("{s:>w$}", w = w[i]) } else { format!("{s:<w$}", w = w[i]) };
        let mut out = self.header.iter().enumerate().map(|(i, h)| pad(h, i)).collect::<Vec<_>>().join("  ").trim_end().to_string() + "\n";
        for r in &self.rows {
            out += &(r.iter().enumerate().map(|(i, c)| pad(&show(c), i)).collect::<Vec<_>>().join("  ").trim_end().to_string() + "\n");
        }
        out
    }

    pub fn csv(&self) -> String {
        let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
        let mut out = self.header.iter().map(|h| q(h)).collect::<Vec<_>>().join(",") + "\n";
        for r in &self.rows {
            out += &(r.iter().map(|c| q(&c.text)).collect::<Vec<_>>().join(",") + "\n");
        }
        out
    }
}

pub struct RangeRow {
    pub field: &'static str,
    pub tier: &'static str,
    pub n: usize,
    pub min: f64,
    pub max: f64,
    pub ui: Option<Band>,
    pub outside_ui: usize,
    pub band: Band,
    pub outside_band: usize,
}

/// How the vanilla entries sit against the editor's slider ranges and our bands: one row per field and vehicle class (counts are
/// entries, so pass the game's de-duplicated entries to count vehicles).
pub fn ranges(entries: &[Entry]) -> Vec<RangeRow> {
    let mut out = vec![];
    for &(field, ..) in BANDS {
        for (t, tier) in TIERS.iter().enumerate() {
            let vals: Vec<f64> = entries.iter().filter(|e| e.h.kind().tier() == t).filter_map(|e| e.h.num(field)).collect();
            if vals.is_empty() {
                continue;
            }
            let ui = UI_RANGES.iter().find(|u| eq(u.0, field)).map(|u| u.1);
            let band = band_of(field, t).unwrap();
            let outside = |b: Band| vals.iter().filter(|v| **v < b.0 || **v > b.1).count();
            out.push(RangeRow {
                field,
                tier,
                n: vals.len(),
                min: vals.iter().cloned().fold(f64::INFINITY, f64::min),
                max: vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                ui,
                outside_ui: ui.map(outside).unwrap_or(0),
                band,
                outside_band: outside(band),
            });
        }
    }
    out
}

pub fn render_ranges(rows: &[RangeRow]) -> String {
    let w = rows.iter().map(|r| r.field.len()).max().unwrap_or(5);
    let mut out = format!("{:<w$}  {:<5} {:>4}  {:>9} {:>9}  {:<15} {:>10}  {:<15} {:>12}\n", "field", "class", "n", "min", "max", "editor range", "outside it", "our band", "outside it");
    let range = |b: Band| format!("{}..{}", fmt(b.0), fmt(b.1));
    for r in rows {
        let (ui, ui_n) = match r.ui {
            Some(u) => (range(u), r.outside_ui.to_string()),
            None => ("-".into(), "-".into()),
        };
        out += &format!("{:<w$}  {:<5} {:>4}  {:>9} {:>9}  {:<15} {:>10}  {:<15} {:>12}\n", r.field, r.tier, r.n, fmt(r.min), fmt(r.max), ui, ui_n, range(r.band), r.outside_band);
    }
    out
}

// ---- tests -----------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const OK_ITEM: &str = r#"<Item type="CHandlingData">
      <handlingName>TESTCAR</handlingName>
      <fMass value="1500.000000" />
      <fInitialDragCoeff value="8.000000" />
      <fPercentSubmerged value="85.000000" />
      <vecCentreOfMassOffset x="0.000000" y="0.000000" z="0.000000" />
      <vecInertiaMultiplier x="1.000000" y="1.300000" z="1.500000" />
      <fDriveBiasFront value="0.300000" />
      <nInitialDriveGears value="6" />
      <fInitialDriveForce value="0.300000" />
      <fDriveInertia value="1.000000" />
      <fClutchChangeRateScaleUpShift value="3.000000" />
      <fClutchChangeRateScaleDownShift value="3.000000" />
      <fInitialDriveMaxFlatVel value="150.000000" />
      <fBrakeForce value="0.800000" />
      <fBrakeBiasFront value="0.450000" />
      <fHandBrakeForce value="0.700000" />
      <fSteeringLock value="40.000000" />
      <fTractionCurveMax value="2.200000" />
      <fTractionCurveMin value="1.900000" />
      <fTractionCurveLateral value="22.000000" />
      <fTractionSpringDeltaMax value="0.150000" />
      <fLowSpeedTractionLossMult value="1.000000" />
      <fCamberStiffnesss value="0.000000" />
      <fTractionBiasFront value="0.480000" />
      <fTractionLossMult value="1.000000" />
      <fSuspensionForce value="2.000000" />
      <fSuspensionCompDamp value="1.400000" />
      <fSuspensionReboundDamp value="2.200000" />
      <fSuspensionUpperLimit value="0.120000" />
      <fSuspensionLowerLimit value="-0.120000" />
      <fSuspensionRaise value="0.000000" />
      <fSuspensionBiasFront value="0.500000" />
      <fAntiRollBarForce value="0.800000" />
      <fAntiRollBarBiasFront value="0.600000" />
      <fRollCentreHeightFront value="0.300000" />
      <fRollCentreHeightRear value="0.300000" />
      <fCollisionDamageMult value="1.000000" />
      <fWeaponDamageMult value="1.000000" />
      <fDeformationDamageMult value="0.800000" />
      <fEngineDamageMult value="1.500000" />
      <fPetrolTankVolume value="65.000000" />
      <fOilVolume value="5.000000" />
      <nMonetaryValue value="30000" />
      <strModelFlags>440010</strModelFlags>
      <strHandlingFlags>0</strHandlingFlags>
      <strDamageFlags>0</strDamageFlags>
      <AIHandling>AVERAGE</AIHandling>
      <SubHandlingData>
        <Item type="CCarHandlingData">
          <fBackEndPopUpCarImpulseMult value="0.100000" />
          <AdvancedData><Item><Slot value="1" /></Item></AdvancedData>
        </Item>
        <Item type="NULL" />
        <Item type="NULL" />
      </SubHandlingData>
    </Item>"#;

    fn file(items: &str) -> String {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CHandlingDataMgr>\n  <HandlingData>\n    {items}\n  </HandlingData>\n</CHandlingDataMgr>\n")
    }

    fn one(label: &str, items: &str) -> Report {
        check_sources(&[Source { label: label.into(), text: file(items) }])
    }

    fn codes(r: &Report) -> Vec<&'static str> {
        r.issues.iter().map(|i| i.code).collect()
    }

    #[test]
    fn parses_known_and_unknown_fields_in_order() {
        let p = parse(&file(&OK_ITEM.replace("<fOilVolume", "<fMadeUp value=\"1\" />\n<fOilVolume"))).unwrap();
        assert_eq!(p.root, "CHandlingDataMgr");
        let h = &p.items[0];
        assert_eq!((h.name.as_str(), h.item_type.as_str(), h.kind()), ("TESTCAR", "CHandlingData", Kind::Car));
        assert_eq!(h.num("FMASS"), Some(1500.0));
        assert_eq!(h.text("strModelFlags"), Some("440010"));
        assert_eq!(h.field("vecInertiaMultiplier").unwrap().value, Value::Vec3([1.0, 1.3, 1.5]));
        assert_eq!(h.num("fMadeUp"), Some(1.0));
        assert!(h.fields.iter().position(|f| f.name == "fMadeUp") < h.fields.iter().position(|f| f.name == "fOilVolume"));
        // sub-handling extras are searchable; complex elements are kept as text
        assert_eq!(h.num("fBackEndPopUpCarImpulseMult"), Some(0.1));
        assert!(matches!(&h.field("AdvancedData").unwrap().value, Value::Raw(s) if s.contains("<Slot value=\"1\"")));
        assert_eq!(h.sub.len(), 3);
        assert!(h.line > 1);
    }

    #[test]
    fn a_complete_entry_is_clean() {
        let r = one("a.meta", OK_ITEM);
        assert!(r.issues.is_empty(), "{:?}", r.issues.iter().map(|i| &i.msg).collect::<Vec<_>>());
    }

    #[test]
    fn duplicate_names_are_errors_case_insensitively_and_across_files() {
        let r = check_sources(&[Source { label: "a.meta".into(), text: file(OK_ITEM) }, Source { label: "b.meta".into(), text: file(&OK_ITEM.replace("TESTCAR", "testcar")) }]);
        assert_eq!(codes(&r), vec!["HANDLING_DUPLICATE_NAME"]);
        assert_eq!((r.errors, r.issues[0].file.as_str()), (1, "b.meta"));
        assert!(r.issues[0].msg.contains("a.meta (line "));
    }

    #[test]
    fn missing_and_non_numeric_fields_are_errors() {
        let bad = OK_ITEM.replace("<fMass value=\"1500.000000\" />", "").replace("value=\"40.000000\"", "value=\"forty\"").replace("<fBrakeForce value=\"0.800000\" />", "<fBrakeForce>0.8</fBrakeForce>");
        let r = one("a.meta", &bad);
        assert_eq!(r.errors, 3, "{:?}", r.issues.iter().map(|i| &i.msg).collect::<Vec<_>>());
        assert!(r.issues.iter().any(|i| i.msg.contains("fMass missing")));
        assert!(r.issues.iter().any(|i| i.msg.contains("\"forty\" is not a number") && i.msg.contains("(line ")));
        assert!(r.issues.iter().any(|i| i.msg.contains("fBrakeForce is not a")));
        // a missing non-essential field is only a warning, an infinite value counts as not numeric
        let r = one("a.meta", &OK_ITEM.replace("<fOilVolume value=\"5.000000\" />", "").replace("value=\"0.300000\" />\n      <fDriveInertia", "value=\"inf\" />\n      <fDriveInertia"));
        assert_eq!((r.errors, r.issues.len()), (1, 2));
        assert!(r.issues.iter().any(|i| i.sev == 'W' && i.msg.contains("fOilVolume missing")));
    }

    #[test]
    fn flags_must_be_hex_text_and_are_not_decoded() {
        let r = one("a.meta", &OK_ITEM.replace(">440010<", ">44001G<"));
        assert_eq!(codes(&r), vec!["HANDLING_BAD_FLAGS"]);
        assert_eq!(r.errors, 1);
        // any hex digits are accepted unjudged, including bits we do not know
        assert!(one("a.meta", &OK_ITEM.replace(">440010<", ">FFFFFFFF<")).issues.is_empty());
    }

    #[test]
    fn ranges_depend_on_the_vehicle_kind_and_vanilla_extremes_pass() {
        let heavy = OK_ITEM.replace("value=\"1500.000000\"", "value=\"250000.000000\"");
        let r = one("a.meta", &heavy);
        assert_eq!(codes(&r), vec!["HANDLING_OUT_OF_RANGE"]);
        assert!(r.issues[0].msg.contains("fMass is 250000, outside 60..80000 for a car (line "));
        // a bike's mass band is tighter than a car's; the same 5000 kg is fine for a car and not for a bike
        let bike = |m: &str| OK_ITEM.replace("value=\"1500.000000\"", &format!("value=\"{m}\"")).replace("<Item type=\"CCarHandlingData\">", "<Item type=\"CBikeHandlingData\">");
        assert_eq!(one("a.meta", &bike("5000")).warnings(), 1);
        assert_eq!(one("a.meta", &bike("300")).warnings(), 0);
        assert_eq!(one("a.meta", &OK_ITEM.replace("value=\"1500.000000\"", "value=\"5000\"")).warnings(), 0);
        // vanilla's Rhino tank (60000 kg) stays silent
        assert_eq!(one("a.meta", &OK_ITEM.replace("value=\"1500.000000\"", "value=\"60000\"")).warnings(), 0);
    }

    #[test]
    fn warns_on_fractional_gears_duplicates_unknowns_and_wrong_item_type() {
        let t = OK_ITEM.replace("<nInitialDriveGears value=\"6\" />", "<nInitialDriveGears value=\"6.5\" /><fMadeUp value=\"1\" /><fMass value=\"1600\" />").replace("type=\"CHandlingData\"", "type=\"CHandlingDat\"");
        let r = one("a.meta", &t);
        let mut c = codes(&r);
        c.sort();
        assert_eq!(c, vec!["HANDLING_BAD_VALUE", "HANDLING_DUPLICATE_FIELD", "HANDLING_ITEM_TYPE", "HANDLING_UNKNOWN_FIELD"]);
        assert_eq!(r.errors, 0);
    }

    #[test]
    fn broken_files_are_reported_not_fatal() {
        let r = check_sources(&[Source { label: "x.meta".into(), text: "<CHandlingDataMgr><oops>".into() }, Source { label: "y.meta".into(), text: "<Other />".into() }, Source { label: "z.meta".into(), text: file(OK_ITEM) }]);
        assert_eq!(codes(&r), vec!["HANDLING_BAD_XML", "HANDLING_BAD_ROOT"]);
        assert!(r.summary.starts_with("1 handling entry in 3 file(s): 2 error(s)"), "{}", r.summary);
    }

    #[test]
    fn compare_sorts_filters_aligns_and_quotes() {
        let heavy = OK_ITEM.replace("TESTCAR", "Heavy,One").replace("value=\"1500.000000\"", "value=\"9000\"");
        let light = OK_ITEM.replace("TESTCAR", "light").replace("value=\"1500.000000\"", "value=\"900\"");
        let dir = std::env::temp_dir().join(format!("rage_toolkit_handling_unit_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("handling.meta"), file(&format!("{heavy}\n{light}"))).unwrap();
        fs::write(dir.join("sub/Handling_extra.META"), file(&OK_ITEM)).unwrap();
        fs::write(dir.join("vehicles.meta"), "<not handling>").unwrap();
        let mut o = CompareOptions { paths: vec![dir.clone()], game: false, game_dir: None, fields: vec![], sort: Some("mass".into()), desc: false, filter: vec![], limit: None };
        let t = compare(&o).unwrap();
        assert_eq!(t.header, ["name", "kind", "fMass", "fInitialDriveForce", "fInitialDriveMaxFlatVel", "fBrakeForce", "fTractionCurveMax", "fDriveBiasFront", "nInitialDriveGears", "fSteeringLock", "source"]);
        assert_eq!(t.rows.iter().map(|r| r[0].text.as_str()).collect::<Vec<_>>(), ["light", "TESTCAR", "Heavy,One"]);
        o.desc = true;
        o.limit = Some(1);
        assert_eq!(compare(&o).unwrap().rows[0][0].text, "Heavy,One");
        o.limit = None;
        o.filter = vec!["LIGHT".into(), "heavy".into()];
        o.fields = vec!["strModelFlags".into(), "gears".into(), "kind".into()];
        o.sort = Some("name".into());
        o.desc = false;
        let t = compare(&o).unwrap();
        assert_eq!(t.header, ["name", "kind", "strModelFlags", "nInitialDriveGears", "source"]);
        let csv = t.csv();
        assert!(csv.starts_with("name,kind,strModelFlags,nInitialDriveGears,source\n\"Heavy,One\",car,440010,6,handling.meta\n"), "{csv}");
        assert!(t.render().lines().all(|l| !l.ends_with(' ')));
        o.fields = vec!["nosuchfield".into()];
        assert!(compare(&o).unwrap_err().to_string().contains("nosuchfield"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn numbers_accept_vanillas_f_suffix_and_nothing_non_finite() {
        assert_eq!((number("-30.0f"), number(" 7.5F "), number("4.00000"), number("1e3")), (Some(-30.0), Some(7.5), Some(4.0), Some(1000.0)));
        for bad in ["", "f", "inf", "-inf", "nan", "1,5", "forty", "1.5ff"] {
            assert_eq!(number(bad), None, "{bad}");
        }
    }

    #[test]
    fn game_labels() {
        assert_eq!(game_label("common.rpf/data/handling.meta"), "game:base");
        assert_eq!(game_label("update/update.rpf/common/data/handling.meta"), "game:update");
        assert_eq!(game_label("update/x64/dlcpacks/mp2023_01/dlc.rpf/common/data/handling.meta"), "game:mp2023_01");
        assert_eq!(game_label("update/update.rpf/dlc_patch/mpbiker/common/data/handling.meta"), "game:patch:mpbiker");
    }

    impl Report {
        fn warnings(&self) -> usize {
            self.issues.iter().filter(|i| i.sev == 'W').count()
        }
    }
}
