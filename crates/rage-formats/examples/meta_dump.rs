//! Dumps a Meta, PSO, RBF or XML metadata file as XML (default) or JSON:
//! `cargo run --example meta_dump -- FILE [--json]`.
use rage_formats::{dump_meta, dump_ymf, is_pso, to_json, to_xml, NameTable, RSC7_MAGIC};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: meta_dump FILE [--json]");
    let data = std::fs::read(path)?;
    let is_rsc7 = data.len() >= 4 && u32::from_le_bytes(data[0..4].try_into()?) == RSC7_MAGIC;
    let dump = if is_rsc7 && !is_pso(&data) { dump_meta(&data)? } else { dump_ymf(&data)?.1 };
    for w in &dump.warnings {
        eprintln!("warning: {w}");
    }
    let names = NameTable::core();
    if args.iter().any(|a| a == "--json") {
        println!("{}", to_json(&dump.root, &names).pretty(2));
    } else {
        print!("{}", to_xml(&dump.root, &names));
    }
    Ok(())
}
