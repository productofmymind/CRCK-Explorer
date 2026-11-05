use rage_toolkit_core::archetype::schema_view::describe;
fn main() {
    for n in std::env::args().skip(1) {
        let h = rage_toolkit_core::rage_joaat(&n);
        match describe(h) {
            Some(fields) => println!("{n}: {}", fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>().join(", ")),
            None => println!("{n}: unknown"),
        }
    }
}
