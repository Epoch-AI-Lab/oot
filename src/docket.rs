use crate::dispute::Docket;
use std::path::Path;

pub fn save(d: &Docket, path: &Path) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(d)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn load(path: &Path) -> anyhow::Result<Docket> {
    let text = std::fs::read_to_string(path)?;
    let d: Docket = serde_json::from_str(&text)?;
    Ok(d)
}
