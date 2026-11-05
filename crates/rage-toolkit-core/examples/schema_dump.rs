use rage_toolkit_core::archetype::schema_view::describe;
use rage_formats::rage_joaat;
fn main() {
    for t in std::env::args().skip(1) {
        println!("== {t}");
        for f in describe(rage_joaat(&t)).unwrap_or_default() { println!("  {} {:?}", f.name, f.kind); }
    }
}
