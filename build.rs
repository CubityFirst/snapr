//! Lists the open-source crates built into snapr, each with its licence and
//! the licence files it ships, for the About tab. Read from `cargo metadata`
//! for the platform being built, so it's always the crates actually used,
//! and written deflated to `$OUT_DIR/third-party.json.deflate`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let list = match metadata() {
        Ok(meta) => crates(&meta),
        Err(e) => {
            // The About tab says the list is missing rather than the build
            // failing over it.
            println!("cargo:warning=no third-party crate list for the About tab: {e}");
            json!({ "crates": [], "texts": [] })
        }
    };
    let bytes = serde_json::to_vec(&list).unwrap();
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("third-party.json.deflate");
    std::fs::write(out, miniz_oxide::deflate::compress_to_vec(&bytes, 10)).unwrap();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/snapr.iconset");
        if let Err(e) = windows_icon() {
            println!("cargo:warning=snapr.exe built without its icon: {e}");
        }
    }
}

/// Puts snapr's icon in snapr.exe, from `assets/snapr.iconset`: it's what
/// Explorer, the Start menu shortcut and taskbar pins show, which can't use
/// the one snapr draws at runtime for its windows and tray.
fn windows_icon() -> Result<(), String> {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/snapr.iconset");
    let files = ["icon_16x16.png", "icon_32x32.png", "icon_32x32@2x.png", "icon_128x128.png", "icon_256x256.png"];
    let pngs = files
        .iter()
        .map(|f| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).to_path_buf();
    let ico = out.join("snapr.ico");
    std::fs::write(&ico, ico_of(&pngs)).map_err(|e| e.to_string())?;
    let rc = out.join("snapr.rc");
    let path = ico.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).map_err(|e| e.to_string())?;
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .map_err(|e| e.to_string())
}

/// An .ico holding PNGs as they are (which Windows has read since Vista).
fn ico_of(pngs: &[Vec<u8>]) -> Vec<u8> {
    let mut ico = vec![0, 0, 1, 0];
    ico.extend((pngs.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * pngs.len() as u32;
    for png in pngs {
        // The PNG's IHDR has its width and height; 0 means 256 here.
        let side = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let side = if side >= 256 { 0 } else { side as u8 };
        ico.extend([side, side, 0, 0, 1, 0, 32, 0]);
        ico.extend((png.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in pngs {
        ico.extend(png);
    }
    ico
}

/// `cargo metadata` for the target, offline if every crate is already
/// downloaded (it usually is: they're being built).
fn metadata() -> Result<Value, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("Cargo.toml");
    let target = std::env::var("TARGET").unwrap();
    let run = |offline: bool| {
        let mut cmd = Command::new(&cargo);
        cmd.args(["metadata", "--format-version", "1", "--filter-platform", &target])
            .arg("--manifest-path")
            .arg(&manifest);
        if offline {
            cmd.arg("--offline");
        }
        cmd.output()
    };
    let output = match run(true) {
        Ok(o) if o.status.success() => o,
        _ => run(false).map_err(|e| e.to_string())?,
    };
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

/// The crates compiled into snapr: its normal dependencies, all the way
/// down, but not proc macros (or what only they use), which run at build
/// time. Licence texts are listed once and referred to by index.
fn crates(meta: &Value) -> Value {
    let packages: HashMap<&str, &Value> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let is_proc_macro = |p: &Value| {
        p["targets"].as_array().into_iter().flatten().any(|t| {
            t["kind"].as_array().into_iter().flatten().any(|k| k == "proc-macro")
        })
    };
    let root = meta["resolve"]["root"].as_str().unwrap_or_default();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            let Some(pkg) = dep["pkg"].as_str() else { continue };
            if normal && packages.get(pkg).is_some_and(|p| !is_proc_macro(p)) {
                stack.push(pkg);
            }
        }
    }
    seen.remove(root);

    let mut texts: Vec<String> = Vec::new();
    let mut text_index: HashMap<String, usize> = HashMap::new();
    let mut list: Vec<Value> = seen
        .iter()
        .filter_map(|id| packages.get(id))
        .map(|p| {
            let dir = Path::new(p["manifest_path"].as_str().unwrap_or_default())
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf();
            let mut files: Vec<_> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_file())
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().to_uppercase();
                    ["LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE"]
                        .iter()
                        .any(|s| name.starts_with(s))
                })
                .map(|e| e.path())
                .collect();
            files.sort();
            let licence_texts: Vec<usize> = files
                .iter()
                .filter_map(|f| std::fs::read_to_string(f).ok())
                .map(|t| t.replace("\r\n", "\n").trim().to_string())
                .filter(|t| !t.is_empty())
                .map(|t| {
                    *text_index.entry(t.clone()).or_insert_with(|| {
                        texts.push(t);
                        texts.len() - 1
                    })
                })
                .collect();
            json!({
                "name": p["name"],
                "version": p["version"],
                "license": p["license"],
                "authors": p["authors"],
                "repository": p["repository"],
                "texts": licence_texts,
            })
        })
        .collect();
    list.sort_by(|a, b| {
        let key = |v: &Value| (v["name"].as_str().unwrap_or_default().to_lowercase(), v["version"].to_string());
        key(a).cmp(&key(b))
    });
    json!({ "crates": list, "texts": texts })
}
