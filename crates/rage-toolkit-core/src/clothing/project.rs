//! A clothing project on disk: a small text file (`.ragetoolkitclothes`) listing the pack name and, for every drawable, its model, cloth
//! file and textures. Paths inside the project's folder are stored relative to it, so the folder can be moved; others stay absolute.
//! Saving writes a temporary file and moves it over, keeping the previous file as `.bak` the first time.
use super::{Drawable, Kind, Project, Sex, Texture, ANCHORS, COMPONENTS};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const HEADER: &str = "rage-toolkit-clothing 1";

fn kind_from_name(s: &str) -> Option<Kind> {
    if let Some(a) = s.strip_prefix("p_") {
        ANCHORS.iter().position(|x| *x == a).map(|i| Kind::Prop(i as u8))
    } else {
        COMPONENTS.iter().position(|x| *x == s).map(|i| Kind::Component(i as u8))
    }
}

fn store(p: &Path, base: &Path) -> String {
    match p.strip_prefix(base) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().to_string(),
    }
}

fn resolve(s: &str, base: &Path) -> PathBuf {
    let p = PathBuf::from(s);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

pub fn to_text(project: &Project, file: &Path) -> String {
    let base = file.parent().unwrap_or(Path::new(""));
    let mut s = format!("{HEADER}\nname {}\nlimit {}\n{}", project.name, project.limit, if project.sorted { "folders sorted\n" } else { "" });
    for (word, d) in project.drawables.iter().map(|d| ("drawable", d)).chain(project.archived.iter().map(|d| ("removed", d))) {
        s += &format!("{word} {} {} {} {}\n", d.sex.label(), d.kind.name(), d.number, d.source_number);
        s += &format!("  ydd {}\n", store(&d.ydd, base));
        s += &format!("  race {}\n", if d.race.is_empty() { "-" } else { &d.race });
        if let Some(c) = &d.cloth {
            s += &format!("  cloth {}\n", store(c, base));
        }
        for (n, a) in &d.alternates {
            s += &format!("  alternate {n} {}\n", store(a, base));
        }
        for t in &d.textures {
            s += &format!("  texture {} {} {}\n", t.letter, if t.race.is_empty() { "-" } else { &t.race }, store(&t.path, base));
        }
    }
    s
}

pub fn from_text(text: &str, file: &Path) -> Result<Project> {
    let base = file.parent().unwrap_or(Path::new(""));
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    if lines.next().map(str::trim) != Some(HEADER) {
        bail!("not a CRCK Explorer clothing project (the first line must be '{HEADER}')");
    }
    let mut p = Project { archived: vec![], sorted: false, name: String::new(), limit: 128, drawables: vec![] };
    let mut cur: Option<Drawable> = None;
    // the block being read is an archived (removed) drawable
    let mut removed = false;
    let push = |p: &mut Project, d: Drawable, removed: bool| if removed { p.archived.push(d) } else { p.drawables.push(d) };
    for (n, line) in lines.enumerate() {
        let at = || format!("line {}", n + 2);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (word, rest) = t.split_once(' ').unwrap_or((t, ""));
        match word {
            "name" => p.name = rest.trim().to_string(),
            "folders" => p.sorted = rest.trim() == "sorted",
            "limit" => p.limit = rest.trim().parse().with_context(|| format!("{}: the limit is not a number", at()))?,
            "drawable" | "removed" => {
                if let Some(d) = cur.take() {
                    push(&mut p, d, removed);
                }
                removed = word == "removed";
                let f: Vec<&str> = rest.split_whitespace().collect();
                let [sex, kind, number, source] = f[..] else { bail!("{}: expected 'drawable <sex> <kind> <number> <source number>'", at()) };
                cur = Some(Drawable {
                    sex: match sex {
                        "male" => Sex::Male,
                        "female" => Sex::Female,
                        _ => bail!("{}: the sex is male or female", at()),
                    },
                    kind: kind_from_name(kind).with_context(|| format!("{}: '{kind}' is not a component or prop", at()))?,
                    number: number.parse().with_context(|| format!("{}: bad number", at()))?,
                    source_number: source.parse().with_context(|| format!("{}: bad source number", at()))?,
                    ydd: PathBuf::new(),
                    race: String::new(),
                    cloth: None, alternates: vec![],
                    textures: vec![],
                });
            }
            "ydd" | "race" | "cloth" | "texture" | "alternate" => {
                let d = cur.as_mut().with_context(|| format!("{}: '{word}' before any drawable", at()))?;
                match word {
                    "ydd" => d.ydd = resolve(rest.trim(), base),
                    "race" => d.race = if rest.trim() == "-" { String::new() } else { rest.trim().to_string() },
                    "cloth" => d.cloth = Some(resolve(rest.trim(), base)),
                    "alternate" => {
                        let (n, path) = rest.trim().split_once(' ').with_context(|| format!("{}: expected 'alternate <number> <path>'", at()))?;
                        d.alternates.push((n.parse().with_context(|| format!("{}: bad alternate number", at()))?, resolve(path.trim(), base)));
                    }
                    _ => {
                        let mut it = rest.splitn(3, ' ');
                        let (letter, race, path) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
                        let letter = letter.chars().next().filter(|c| c.is_ascii_lowercase()).with_context(|| format!("{}: the texture letter is a..z", at()))?;
                        d.textures.push(Texture { letter, race: if race == "-" { String::new() } else { race.to_string() }, path: resolve(path.trim(), base) });
                    }
                }
            }
            other => bail!("{}: unknown line '{other}'", at()),
        }
    }
    if let Some(d) = cur.take() {
        push(&mut p, d, removed);
    }
    Ok(p)
}

/// Write the project; the first overwrite of a file keeps the old one as `<file>.bak`.
pub fn save(project: &Project, file: &Path) -> Result<()> {
    if file.exists() {
        let bak = PathBuf::from(format!("{}.bak", file.display()));
        if !bak.exists() {
            fs::copy(file, &bak).with_context(|| format!("keeping a backup of {}", file.display()))?;
        }
    }
    let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", file.display()));
    fs::write(&tmp, to_text(project, file))?;
    fs::rename(&tmp, file).with_context(|| format!("saving {}", file.display()))?;
    Ok(())
}

pub fn load(file: &Path) -> Result<Project> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    from_text(&text, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_survives_saving_and_loading_and_paths_stay_relative() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_cloth_proj_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let file = d.join("pack.ragetoolkitclothes");
        let p = Project {
            archived: vec![], sorted: false,
            name: "mypack".into(),
            limit: 256,
            drawables: vec![
                Drawable { sex: Sex::Male, kind: Kind::Component(11), number: 0, source_number: 7, ydd: d.join("src").join("jbib 007_u.ydd"), race: "u".into(), cloth: Some(d.join("src/jbib_007_u.yld")), alternates: vec![(1, d.join("src/jbib_007_u_1.ydd"))], textures: vec![Texture { path: d.join("src/a b.ytd"), letter: 'a', race: "uni".into() }, Texture { path: PathBuf::from("C:\\elsewhere\\t.ytd"), letter: 'b', race: String::new() }] },
                Drawable { sex: Sex::Female, kind: Kind::Prop(0), number: 3, source_number: 3, ydd: d.join("p_head_003.ydd"), race: String::new(), cloth: None, alternates: vec![], textures: vec![] },
            ],
        };
        save(&p, &file).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("ydd src/jbib 007_u.ydd") && text.contains("C:\\elsewhere\\t.ytd"), "{text}");
        assert_eq!(load(&file).unwrap(), p);
        // the second save keeps the first as .bak, once
        save(&p, &file).unwrap();
        assert!(PathBuf::from(format!("{}.bak", file.display())).is_file());
        assert!(from_text("nonsense", &file).is_err());
        assert!(from_text("rage-toolkit-clothing 1\ndrawable alien jbib 0 0\n", &file).is_err());
        assert!(from_text("rage-toolkit-clothing 1\n  ydd x\n", &file).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
