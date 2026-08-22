//! Serialization and persistence helpers for Oot dockets.

use crate::dispute::Docket;
use std::path::Path;

/// Serialize an adjudication [`Docket`] to a pretty-printed JSON string.
pub fn to_json(d: &Docket) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(d)?)
}

/// Deserialize an adjudication [`Docket`] from a JSON string.
pub fn from_json(s: &str) -> anyhow::Result<Docket> {
    Ok(serde_json::from_str(s)?)
}

/// Serialize an adjudication [`Docket`] to a pretty-printed TOML string.
pub fn to_toml(d: &Docket) -> anyhow::Result<String> {
    Ok(toml::to_string_pretty(d)?)
}

/// Deserialize an adjudication [`Docket`] from a TOML string.
pub fn from_toml(s: &str) -> anyhow::Result<Docket> {
    Ok(toml::from_str(s)?)
}

/// Save an adjudication [`Docket`] to disk as formatted JSON.
pub fn save(d: &Docket, path: &Path) -> anyhow::Result<()> {
    let text = to_json(d)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Save an adjudication [`Docket`] to disk as formatted JSON (explicit alias for [`save`]).
pub fn save_json(d: &Docket, path: &Path) -> anyhow::Result<()> {
    save(d, path)
}

/// Save an adjudication [`Docket`] to disk as formatted TOML.
pub fn save_toml(d: &Docket, path: &Path) -> anyhow::Result<()> {
    let text = to_toml(d)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Load an adjudication [`Docket`] from a JSON file on disk.
pub fn load_json(path: &Path) -> anyhow::Result<Docket> {
    let text = std::fs::read_to_string(path)?;
    from_json(&text)
}

/// Load an adjudication [`Docket`] from a TOML file on disk.
pub fn load_toml(path: &Path) -> anyhow::Result<Docket> {
    let text = std::fs::read_to_string(path)?;
    from_toml(&text)
}

/// Load an adjudication [`Docket`] from a file on disk (supporting both JSON and TOML formats).
pub fn load(path: &Path) -> anyhow::Result<Docket> {
    let text = std::fs::read_to_string(path)?;
    if let Ok(d) = from_json(&text) {
        Ok(d)
    } else {
        from_toml(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispute::Verdict;

    #[test]
    fn test_docket_save_and_load_roundtrip() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("oot_test_docket.json");

        let original = Docket {
            change: "feature/test-docket".into(),
            source: "jj".into(),
            base: "main".into(),
            head: "feature/test-docket".into(),
            disputes: vec![],
            intent: "testing save and load".into(),
            authors: vec!["@tester".into()],
            verdict: Verdict::Adjudicated,
            embargo: None,
        };

        save(&original, &path).expect("failed to save docket");
        let loaded = load(&path).expect("failed to load docket");

        assert_eq!(loaded.change, original.change);
        assert_eq!(loaded.source, original.source);
        assert_eq!(loaded.intent, original.intent);
        assert_eq!(loaded.verdict, original.verdict);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_docket_toml_and_json_serialization() {
        let original = Docket {
            change: "feature/toml-test".into(),
            source: "git".into(),
            base: "main".into(),
            head: "feature/toml-test".into(),
            disputes: vec![],
            intent: "toml format".into(),
            authors: vec!["@coder".into()],
            verdict: Verdict::Embargoed,
            embargo: Some("patch held for maintainers until 2026-12-31".into()),
        };

        // JSON string roundtrip
        let json_str = to_json(&original).unwrap();
        let from_json_docket = from_json(&json_str).unwrap();
        assert_eq!(from_json_docket.change, original.change);
        assert_eq!(from_json_docket.verdict, original.verdict);

        // TOML string roundtrip
        let toml_str = to_toml(&original).unwrap();
        let from_toml_docket = from_toml(&toml_str).unwrap();
        assert_eq!(from_toml_docket.change, original.change);
        assert_eq!(from_toml_docket.verdict, original.verdict);
        assert_eq!(from_toml_docket.embargo, original.embargo);

        // File save/load TOML
        let temp_path = std::env::temp_dir().join("oot_test_docket.toml");
        save_toml(&original, &temp_path).unwrap();
        let loaded = load(&temp_path).unwrap();
        assert_eq!(loaded.change, original.change);
        let _ = std::fs::remove_file(temp_path);
    }
}
