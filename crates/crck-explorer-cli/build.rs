//! Puts the logo and a version block into crck-explorer-cli.exe (what Explorer shows). Windows only.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // assets/crck-explorer.ico, written without `..` and with forward slashes: rc.exe is fussy about both
    let ico = manifest.parent().and_then(|p| p.parent()).unwrap().join("assets/crck-explorer.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut n = version.split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u16>().unwrap_or(0)).chain(std::iter::repeat(0));
    let v = (n.next().unwrap(), n.next().unwrap(), n.next().unwrap());
    let rc = format!(
        r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x4
FILETYPE 0x1
{{
  BLOCK "StringFileInfo" {{
    BLOCK "040904b0" {{
      VALUE "FileDescription", "CRCK Explorer command line"
      VALUE "ProductName", "CRCK Explorer"
      VALUE "InternalName", "crck-explorer-cli"
      VALUE "OriginalFilename", "crck-explorer-cli.exe"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
    }}
  }}
  BLOCK "VarFileInfo" {{
    VALUE "Translation", 0x409, 1200
  }}
}}
"#,
        ico = ico.display().to_string().replace('\\', "/"),
        a = v.0,
        b = v.1,
        c = v.2,
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("crck-explorer-cli.rc");
    fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
