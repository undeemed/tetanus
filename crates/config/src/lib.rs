//! Layered config: defaults < file < env < CLI flags, with full
//! provenance (every resolved key remembers which layer set it).

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Layer {
    Default,
    File,
    Env,
    Flag,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Resolved {
    pub value: serde_json::Value,
    pub layer: Layer,
}

#[derive(Default, Debug)]
pub struct Config {
    entries: BTreeMap<String, Resolved>,
}

impl Config {
    pub fn set(&mut self, key: &str, value: serde_json::Value, layer: Layer) {
        match self.entries.get(key) {
            Some(existing) if layer_rank(existing.layer) > layer_rank(layer) => {}
            _ => {
                self.entries
                    .insert(key.to_string(), Resolved { value, layer });
            }
        }
    }
    pub fn get(&self, key: &str) -> Option<&Resolved> {
        self.entries.get(key)
    }
    pub fn provenance(&self) -> impl Iterator<Item = (&String, &Resolved)> {
        self.entries.iter()
    }
}

fn layer_rank(l: Layer) -> u8 {
    match l {
        Layer::Default => 0,
        Layer::File => 1,
        Layer::Env => 2,
        Layer::Flag => 3,
    }
}
