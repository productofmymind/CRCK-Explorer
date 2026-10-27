"""Extracts the reference tool's Meta and PSO structure/enum tables into the compact
text files `src/schema/reference_meta.txt` and `src/schema/reference_pso.txt`
that `schema.rs` loads for writing files from XML/JSON.

Usage: python tools/extract_reference_schema.py <the reference library/GameFiles/MetaTypes dir>

Line formats (hashes in hex, one record per line):
  M  <struct> <key> <unk8> <size>   <name>,<offset>,<type>,<unk9>,<refidx>,<refkey> ...
  ME <enum> <key>                   <name>,<value> ...
  P  <struct> <type> <unk> <size>   <name>,<type>,<offset>,<subtype>,<refkey> ...
  PE <enum> <type>                  <name>,<value> ...
"""
import re
import sys
from pathlib import Path


def joaat(s: str) -> int:
    h = 0
    for b in s.encode():
        h = (h + b) & 0xFFFFFFFF
        h = (h + (h << 10)) & 0xFFFFFFFF
        h ^= h >> 6
    h = (h + (h << 3)) & 0xFFFFFFFF
    h ^= h >> 11
    h = (h + (h << 15)) & 0xFFFFFFFF
    return h


def strip_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


def load_enum(text: str, name: str) -> dict:
    m = re.search(r"enum\s+%s\s*:\s*\w+\s*\{(.*?)\n\s*\}" % name, text, re.S)
    body = m.group(1)
    out = {}
    for k, v in re.findall(r"^\s*(\w+)\s*=\s*(0x[0-9A-Fa-f]+|\d+)", body, re.M):
        out[k] = int(v, 0)
    return out


class Names:
    def __init__(self, meta_names, type_names, meta_types, pso_types):
        self.meta_names = meta_names
        self.type_names = type_names
        self.meta_types = meta_types
        self.pso_types = pso_types
        self.unresolved = set()

    def name(self, tok: str) -> int:
        tok = tok.strip()
        if tok in ("0", "0u"):
            return 0
        if tok.startswith("(MetaName)"):
            rest = tok[len("(MetaName)"):]
            if rest.startswith("MetaTypeName."):
                return self.type_names[rest.split(".", 1)[1]]
            if rest.startswith("MetaName."):
                return self.name(rest)
            return int(rest, 0) & 0xFFFFFFFF
        if tok.startswith("MetaName."):
            key = tok.split(".", 1)[1]
            if key in self.meta_names:
                return self.meta_names[key]
            self.unresolved.add(key)
            return joaat(key)
        if tok.startswith("MetaTypeName."):
            return self.type_names[tok.split(".", 1)[1]]
        return int(tok, 0) & 0xFFFFFFFF

    def meta_type(self, tok: str) -> int:
        return self.meta_types[tok.strip().split(".", 1)[1]]

    def pso_type(self, tok: str) -> int:
        return self.pso_types[tok.strip().split(".", 1)[1]]


def split_args(s: str):
    """Top-level comma split of a C# argument list."""
    out, depth, cur = [], 0, []
    for ch in s:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "," and depth == 0:
            out.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    if "".join(cur).strip():
        out.append("".join(cur))
    return [a.strip() for a in out]


def records(text: str, ctor: str):
    """Every `new <ctor>( ... );` statement body, as a list of top-level args."""
    for m in re.finditer(r"new %s\((.*?)\);" % ctor, text, re.S):
        args = split_args(m.group(1))
        if args and not args[0].startswith("{"):  # skip the code-generator templates
            yield args


def entries_of(args, inner_ctor):
    for a in args:
        m = re.match(r"new %s\((.*)\)$" % inner_ctor, a.strip(), re.S)
        if m:
            yield split_args(m.group(1))


def hx(v: int) -> str:
    return "%x" % (v & 0xFFFFFFFF)


def main():
    src = Path(sys.argv[1])
    out_dir = Path(__file__).resolve().parent.parent / "src" / "schema"
    out_dir.mkdir(exist_ok=True)

    meta_names = load_enum(strip_comments((src / "MetaNames.cs").read_text(encoding="utf-8", errors="replace")), "MetaName")
    meta_types_src = strip_comments((src / "MetaTypes.cs").read_text(encoding="utf-8", errors="replace"))
    meta_src = strip_comments((src / "Meta.cs").read_text(encoding="utf-8", errors="replace"))
    pso_src = strip_comments((src / "Pso.cs").read_text(encoding="utf-8", errors="replace"))
    pso_types_src = strip_comments((src / "PsoTypes.cs").read_text(encoding="utf-8", errors="replace"))
    names = Names(
        meta_names,
        load_enum(meta_types_src, "MetaTypeName"),
        load_enum(meta_src, "MetaStructureEntryDataType"),
        load_enum(pso_src, "PsoDataType"),
    )

    meta_lines, seen = [], set()
    for args in records(meta_types_src, "MetaStructureInfo"):
        name, key, unk8, size = names.name(args[0]), int(args[1], 0), int(args[2], 0), int(args[3], 0)
        if name in seen:
            continue
        seen.add(name)
        ents = []
        for e in entries_of(args[4:], "MetaStructureEntryInfo_s"):
            ents.append("%s,%d,%x,%d,%d,%s" % (hx(names.name(e[0])), int(e[1], 0), names.meta_type(e[2]), int(e[3], 0), int(e[4], 0), hx(names.name(e[5]))))
        meta_lines.append("M %s %s %d %d %s" % (hx(name), hx(key), unk8, size, " ".join(ents)))
    seen = set()
    for args in records(meta_types_src, "MetaEnumInfo"):
        name, key = names.name(args[0]), int(args[1], 0)
        if name in seen:
            continue
        seen.add(name)
        ents = ["%s,%d" % (hx(names.name(e[0])), int(e[1], 0)) for e in entries_of(args[2:], "MetaEnumEntryInfo_s")]
        meta_lines.append("ME %s %s %s" % (hx(name), hx(key), " ".join(ents)))

    pso_lines, seen = [], set()
    for args in records(pso_types_src, "PsoStructureInfo"):
        name, ty, unk, size = names.name(args[0]), int(args[1], 0), int(args[2], 0), int(args[3], 0)
        if name in seen:
            continue
        seen.add(name)
        ents = []
        for e in entries_of(args[4:], "PsoStructureEntryInfo"):
            ents.append("%s,%x,%d,%d,%s" % (hx(names.name(e[0])), names.pso_type(e[1]), int(e[2], 0), int(e[3], 0), hx(names.name(e[4]))))
        pso_lines.append("P %s %d %d %d %s" % (hx(name), ty, unk, size, " ".join(ents)))
    seen = set()
    for args in records(pso_types_src, "PsoEnumInfo"):
        name, ty = names.name(args[0]), int(args[1], 0)
        if name in seen:
            continue
        seen.add(name)
        ents = ["%s,%d" % (hx(names.name(e[0])), int(e[1], 0)) for e in entries_of(args[2:], "PsoEnumEntryInfo")]
        pso_lines.append("PE %s %d %s" % (hx(name), ty, " ".join(ents)))

    header = "# source: the reference tool MetaTypes.cs / PsoTypes.cs, extracted by tools/extract_reference_schema.py\n"
    (out_dir / "reference_meta.txt").write_text(header + "\n".join(meta_lines) + "\n", encoding="utf-8", newline="\n")
    (out_dir / "reference_pso.txt").write_text(header + "\n".join(pso_lines) + "\n", encoding="utf-8", newline="\n")
    ms = sum(1 for l in meta_lines if l.startswith("M "))
    ps = sum(1 for l in pso_lines if l.startswith("P "))
    print("meta: %d structs, %d enums; pso: %d structs, %d enums" % (ms, len(meta_lines) - ms, ps, len(pso_lines) - ps))
    if names.unresolved:
        print("unresolved MetaName members (hashed by joaat):", sorted(names.unresolved)[:20], len(names.unresolved))


if __name__ == "__main__":
    main()
