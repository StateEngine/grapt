use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Marketplace {
    pub name: Name,
    pub entries: BTreeMap<Name, Value>,
    pub refused: Vec<String>,
}
impl Marketplace {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value = document(bytes)?;
        let name = Name::new(value["name"].as_str().ok_or("marketplace requires name")?)?;
        if !value["owner"]["name"]
            .as_str()
            .is_some_and(|v| !v.trim().is_empty())
        {
            return Err("marketplace requires owner.name".into());
        }
        let entries = value["plugins"]
            .as_array()
            .ok_or("marketplace requires plugins[]")?;
        if entries.len() > 1024 {
            return Err("marketplace exceeds 1024 entries".into());
        }
        let mut marketplace = Self {
            name,
            entries: BTreeMap::new(),
            refused: Vec::new(),
        };
        for (index, entry) in entries.iter().enumerate() {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "entry has no name".to_owned())
                .and_then(Name::new);
            match name {
                Ok(name) if entry.get("source").is_some() => {
                    if marketplace.entries.insert(name, entry.clone()).is_some() {
                        return Err("marketplace contains a duplicate plugin identity".into());
                    }
                }
                _ => marketplace
                    .refused
                    .push(format!("entry {index} requires a valid name and source")),
            }
        }
        Ok(marketplace)
    }
    pub fn local_source(&self, name: &Name) -> Result<PathBuf, String> {
        match self.source(name)? {
            PluginSource::Relative(path) => Ok(path),
            PluginSource::Git(_) => {
                Err("this slice supports relative directory plugin sources only".into())
            }
        }
    }
    pub fn source(&self, name: &Name) -> Result<PluginSource, String> {
        let entry = self
            .entries
            .get(name)
            .ok_or("plugin is not listed in this marketplace")?;
        PluginSource::parse(&entry["source"])
    }
}
