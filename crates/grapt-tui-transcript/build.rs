//! Package the pinned linked grammars by dependency closure, before runtime.
//! Syntect's public serde representation preserves its opaque linked context IDs.
use serde_json::Value;
use std::{collections::BTreeSet, error::Error, fs, path::PathBuf};
use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

fn indices(
    value: &mut Value,
    map: Option<&[usize]>,
    found: &mut BTreeSet<usize>,
) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Object(fields)
            if fields.contains_key("syntax_index") && fields.contains_key("context_index") =>
        {
            let index = fields["syntax_index"]
                .as_u64()
                .ok_or("Invalid linked syntax index")? as usize;
            found.insert(index);
            if let Some(map) = map {
                let target = *map.get(index).ok_or("Linked syntax index outside bundle")?;
                if target == usize::MAX {
                    return Err("Missing grammar dependency".into());
                }
                fields.insert("syntax_index".into(), Value::from(target));
            }
        }
        Value::Object(fields) => {
            for child in fields.values_mut() {
                indices(child, map, found)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                indices(child, map, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("Missing OUT_DIR")?);
    let mut themes = syntect::highlighting::ThemeSet::load_defaults();
    for (name, file) in [
        ("InspiredGitHub", "theme-light.bin"),
        ("base16-ocean.dark", "theme-dark.bin"),
    ] {
        let theme = themes
            .themes
            .remove(name)
            .ok_or("Missing active syntax theme")?;
        let selected = syntect::highlighting::ThemeSet {
            themes: [(name.to_owned(), theme)].into_iter().collect(),
        };
        syntect::dumps::dump_to_uncompressed_file(&selected, out.join(file))?;
        if fs::metadata(out.join(file))?.len() > 2 * 1024 * 1024 {
            return Err("Theme payload exceeds bound".into());
        }
    }
    let full = two_face::syntax::extra_newlines().into_builder();
    let definitions = full.syntaxes();
    if definitions.len() > 1024 {
        return Err("Grammar count exceeds bound".into());
    }
    let plain = definitions
        .iter()
        .position(|s| s.name == "Plain Text")
        .ok_or("Missing plain grammar")?;
    let mut sources = Vec::new();
    let mut edges = Vec::new();
    for definition in definitions {
        let mut definition = definition.clone();
        // Variables have already been expanded in the pinned linked contexts.
        definition.variables.clear();
        let mut value = serde_json::to_value(definition)?;
        let mut dependencies = BTreeSet::new();
        indices(&mut value, None, &mut dependencies)?;
        sources.push(value);
        edges.push(dependencies);
    }
    let mut payload = Vec::new();
    let mut index = Vec::new();
    for (root, definition) in definitions.iter().enumerate() {
        let mut included = BTreeSet::new();
        let mut pending = vec![root, plain];
        while let Some(next) = pending.pop() {
            if included.insert(next) {
                pending.extend(edges.get(next).ok_or("Invalid grammar dependency")?);
            }
        }
        let mut map = vec![usize::MAX; definitions.len()];
        for (target, source) in included.iter().enumerate() {
            map[*source] = target;
        }
        let mut builder = SyntaxSetBuilder::new();
        for source in included {
            let mut value = sources[source].clone();
            indices(&mut value, Some(&map), &mut BTreeSet::new())?;
            let definition: SyntaxDefinition = serde_json::from_value(value)?;
            builder.add(definition);
        }
        let set = builder.build();
        let mut bytes = Vec::new();
        syntect::dumps::dump_to_uncompressed_file(&set, out.join("grammar-tmp.bin"))?;
        bytes.extend(fs::read(out.join("grammar-tmp.bin"))?);
        if bytes.len() > 2 * 1024 * 1024 || payload.len() + bytes.len() > 32 * 1024 * 1024 {
            return Err("Grammar payload exceeds bound".into());
        }
        index.push(serde_json::json!({"name":definition.name,"extensions":definition.file_extensions,"offset":payload.len(),"length":bytes.len()}));
        payload.extend(bytes);
    }
    fs::remove_file(out.join("grammar-tmp.bin"))?;
    fs::write(out.join("grammars.bin"), payload)?;
    let index = serde_json::to_vec(&index)?;
    if index.len() > 256 * 1024 {
        return Err("Grammar index exceeds bound".into());
    }
    fs::write(out.join("grammar-index.json"), index)?;
    Ok(())
}
