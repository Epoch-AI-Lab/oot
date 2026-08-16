//! Structural difference engine using Tree-sitter.
//!
//! Compares snapshots semantically across function definitions
//! rather than line-by-line diffs.

use crate::change::Snapshot;
use crate::dispute::{Dispute, Kind, Severity};
use std::collections::HashMap;
use tree_sitter::{Node, Parser, Tree};

/// Structural difference engine for code snapshots.
pub struct Engine {
    language: tree_sitter::Language,
}

impl Engine {
    /// Create a new structural diff engine initialized with Rust grammar support.
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_rust::LANGUAGE.into();
        Ok(Engine { language })
    }

    /// Compare two snapshots and report Meaning disputes: functions that
    /// changed, were added, or were removed between base and head.
    pub fn diff_snapshots(&self, base: &Snapshot, head: &Snapshot) -> anyhow::Result<Vec<Dispute>> {
        let mut parser = Parser::new();
        parser.set_language(&self.language)?;

        let mut disputes = Vec::new();
        let mut n = 1;

        let mut paths: Vec<&String> = base.files.keys().chain(head.files.keys()).collect();
        paths.sort();
        paths.dedup();

        for path in paths {
            if !path.ends_with(".rs") {
                continue;
            }
            let base_src = base.files.get(path);
            let head_src = head.files.get(path);

            match (base_src, head_src) {
                (Some(b), Some(h)) => {
                    let base_fns = extract_functions(parse(&mut parser, b).as_ref(), b);
                    let head_fns = extract_functions(parse(&mut parser, h).as_ref(), h);
                    for (name, (h_src, h_row)) in &head_fns {
                        match base_fns.get(name) {
                            Some((b_src, _)) if b_src != h_src => {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    *h_row,
                                    format!("both sides changed `{}`", name),
                                ));
                            }
                            None => {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    *h_row,
                                    format!("added function `{}`", name),
                                ));
                            }
                            _ => {}
                        }
                    }
                    for name in base_fns.keys() {
                        if !head_fns.contains_key(name) {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                0,
                                format!("removed function `{}`", name),
                            ));
                        }
                    }
                }
                (Some(_), None) => {
                    disputes.push(meaning(&mut n, path, 0, "file removed".to_string()));
                }
                (None, Some(_)) => {
                    disputes.push(meaning(&mut n, path, 0, "file added".to_string()));
                }
                (None, None) => {}
            }
        }
        Ok(disputes)
    }
}

fn parse(parser: &mut Parser, source: &str) -> Option<Tree> {
    parser.parse(source, None)
}

fn meaning(n: &mut i32, path: &str, row: usize, detail: String) -> Dispute {
    let id = format!("D{:03}", n);
    *n += 1;
    Dispute {
        id,
        location: format!("{}:{}", path, row),
        kind: Kind::Meaning,
        severity: Severity::Review,
        detail,
    }
}

fn extract_functions(tree: Option<&Tree>, source: &str) -> HashMap<String, (String, usize)> {
    let mut map = HashMap::new();
    let Some(tree) = tree else {
        return map;
    };
    collect(tree.root_node(), source, &mut map);
    map
}

fn collect(node: Node, source: &str, map: &mut HashMap<String, (String, usize)>) {
    if node.kind() == "function_item" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = name_node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_string();
            let src = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            let row = node.start_position().row + 1;
            map.insert(name, (src, row));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_diff_functions() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "src/lib.rs".into(),
            "pub fn hello() -> &'static str { \"hello\" }\npub fn old_fn() {}\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "src/lib.rs".into(),
            "pub fn hello() -> &'static str { \"hello world\" }\npub fn new_fn() {}\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 3);

        let details: Vec<&str> = disputes.iter().map(|d| d.detail.as_str()).collect();
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `hello`")));
        assert!(details
            .iter()
            .any(|d| d.contains("added function `new_fn`")));
        assert!(details
            .iter()
            .any(|d| d.contains("removed function `old_fn`")));
    }
}
