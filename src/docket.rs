//! Serialization and persistence helpers for Oot dockets.

use crate::dispute::Docket;
use std::io::Write;
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

/// Atomically persist `contents` to `path` via a temp file in the same
/// directory followed by rename, so readers see either the old complete
/// file or the new complete file, never a truncated one.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "docket".to_string());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{file_name}.tmp-{}-{nanos}", std::process::id()));

    let write_result = (|| -> anyhow::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Save an adjudication [`Docket`] to disk as formatted JSON.
pub fn save(d: &Docket, path: &Path) -> anyhow::Result<()> {
    let text = to_json(d)?;
    atomic_write(path, text.as_bytes())
}

/// Save an adjudication [`Docket`] to disk as formatted JSON (explicit alias for [`save`]).
pub fn save_json(d: &Docket, path: &Path) -> anyhow::Result<()> {
    save(d, path)
}

/// Save an adjudication [`Docket`] to disk as formatted TOML.
pub fn save_toml(d: &Docket, path: &Path) -> anyhow::Result<()> {
    let text = to_toml(d)?;
    atomic_write(path, text.as_bytes())
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
///
/// Format is selected by extension when present (`\.json` -> JSON only,
/// `\.toml` -> TOML only); otherwise the trimmed content decides
/// (leading `{` -> JSON, anything else -> TOML). A file that looks like
/// JSON is never retried as TOML, so real JSON syntax/schema errors
/// surface as JSON errors instead of being masked by a TOML error.
pub fn load(path: &Path) -> anyhow::Result<Docket> {
    let text = std::fs::read_to_string(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("json") => from_json(&text),
        Some("toml") => from_toml(&text),
        _ => {
            if text.trim_start().starts_with('{') {
                from_json(&text)
            } else {
                from_toml(&text)
            }
        }
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

    #[test]
    fn test_load_json_error_is_not_masked_by_toml() {
        // Truncated JSON must surface the JSON parse error, not a TOML one.
        let path = std::env::temp_dir().join(format!(
            "oot_test_docket_badjson-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "{ \"change\": \"x\",").unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(
            !err.contains("TOML parse error"),
            "JSON error was masked as TOML: {err}"
        );
        // The error must equal the direct JSON parse error.
        let direct = from_json("{ \"change\": \"x\",").unwrap_err().to_string();
        assert_eq!(err, direct);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_load_rejects_toml_content_with_json_extension() {
        // TOML content under a .json name must fail as JSON, not fall back.
        let path = std::env::temp_dir().join(format!(
            "oot_test_docket_mismatch-{}.json",
            std::process::id()
        ));
        let original = Docket {
            change: "feature/mismatch".into(),
            source: "git".into(),
            base: "main".into(),
            head: "feature/mismatch".into(),
            disputes: vec![],
            intent: "mismatch".into(),
            authors: vec!["@coder".into()],
            verdict: Verdict::Adjudicated,
            embargo: None,
        };
        save_toml(&original, &path).unwrap();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_save_overwrite_leaves_no_tmp_and_complete_file() {
        let dir =
            std::env::temp_dir().join(format!("oot_test_docket_atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("docket.json");

        let mk = |intent: &str| Docket {
            change: "feature/atomic".into(),
            source: "git".into(),
            base: "main".into(),
            head: "feature/atomic".into(),
            disputes: vec![],
            intent: intent.into(),
            authors: vec!["@tester".into()],
            verdict: Verdict::Adjudicated,
            embargo: None,
        };

        save(&mk("first"), &path).unwrap();
        assert_eq!(load(&path).unwrap().intent, "first");
        save(&mk("second-longer-intent-here"), &path).unwrap();
        assert_eq!(load(&path).unwrap().intent, "second-longer-intent-here");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
