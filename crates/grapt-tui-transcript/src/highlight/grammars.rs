//! Load one grammar dependency closure; unrelated languages stay in the image.
use serde::Deserialize;
use syntect::parsing::SyntaxSet;

const INDEX: &str = include_str!(concat!(env!("OUT_DIR"), "/grammar-index.json"));
const DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grammars.bin"));

#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(borrow)]
    name: &'a str,
    #[serde(borrow)]
    extensions: Vec<&'a str>,
    offset: usize,
    length: usize,
}

pub(super) struct Grammars {
    entries: Vec<Entry<'static>>,
    current: Option<(usize, SyntaxSet)>,
}

impl Grammars {
    pub fn new() -> Result<Self, String> {
        let entries: Vec<Entry<'static>> =
            serde_json::from_str(INDEX).map_err(|e| e.to_string())?;
        if entries.len() > 1024 {
            return Err("Grammar index exceeds bound".into());
        }
        Ok(Self {
            entries,
            current: None,
        })
    }

    pub fn get(&mut self, token: &str) -> Result<&SyntaxSet, String> {
        let index = self
            .entries
            .iter()
            .rposition(|e| {
                e.extensions
                    .iter()
                    .any(|ext| ext.eq_ignore_ascii_case(token))
            })
            .or_else(|| {
                self.entries
                    .iter()
                    .rposition(|e| e.name.eq_ignore_ascii_case(token))
            })
            .or_else(|| self.entries.iter().position(|e| e.name == "Plain Text"))
            .ok_or("Missing plain grammar")?;
        if self.current.as_ref().is_none_or(|(old, _)| *old != index) {
            let entry = &self.entries[index];
            let end = entry
                .offset
                .checked_add(entry.length)
                .ok_or("Invalid grammar range")?;
            let bytes = DATA.get(entry.offset..end).ok_or("Invalid grammar range")?;
            let syntax =
                syntect::dumps::from_uncompressed_data(bytes).map_err(|e| e.to_string())?;
            self.current = Some((index, syntax));
        }
        self.current
            .as_ref()
            .map(|(_, syntax)| syntax)
            .ok_or_else(|| "Missing grammar".into())
    }
}
