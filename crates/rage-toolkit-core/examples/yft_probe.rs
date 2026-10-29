//! Dumps a .yft as XML: `cargo run -p rage-toolkit-core --example yft_probe -- FILE [--rt]`; with `--rt` it builds the
//! XML back into a .yft, checks it and reports sizes instead of printing the XML.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: yft_probe FILE [--rt]");
    let data = std::fs::read(&path)?;
    let xml = rage_formats::dump_yft_xml(&data, &rage_formats::NameTable::core(), None)?;
    if std::env::args().any(|a| a == "--rt") {
        let t = std::time::Instant::now();
        let built = rage_formats::build_yft_from_xml_checked(&xml, None)?;
        if let Some(p) = std::env::args().skip_while(|a| a != "--write").nth(1) { std::fs::write(p, &built.bytes)?; }
        let (chunks, drawables, bytes) = rage_formats::blocks::yft_xml::chunk_summary(&data)?;
        println!("{path}: {} -> {} bytes, {chunks} chunks ({bytes} bytes), {drawables} drawables, xml {} bytes, {:?}, warnings {:?}", data.len(), built.bytes.len(), xml.len(), t.elapsed(), built.warnings.len());
    } else { print!("{xml}"); }
    Ok(())
}
