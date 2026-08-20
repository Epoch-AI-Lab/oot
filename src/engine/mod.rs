//! Structural difference engine using Tree-sitter.
//!
//! Compares snapshots semantically across function definitions
//! rather than line-by-line diffs.

use crate::change::Snapshot;
use crate::dispute::{Dispute, Kind, Severity};
use crate::engine::language::{registry, LangConfig};
use std::collections::HashMap;
use tree_sitter::{Language, Node, Parser, Tree};

pub mod language;

/// Structural difference engine for code snapshots.
pub struct Engine {
    languages: Vec<LangConfig>,
}

impl Engine {
    /// Create a new structural diff engine with grammar support for every
    /// language in the [`registry`].
    pub fn new() -> anyhow::Result<Self> {
        Ok(Engine {
            languages: registry(),
        })
    }

    /// The grammar configuration for `path`, if Oot can diff that language.
    fn config_for(&self, path: &str) -> Option<&LangConfig> {
        self.languages.iter().find(|c| c.supports(path))
    }

    /// Compare two snapshots and report Meaning disputes: functions that
    /// changed, were added, or were removed between base and head.
    pub fn diff_snapshots(&self, base: &Snapshot, head: &Snapshot) -> anyhow::Result<Vec<Dispute>> {
        let mut disputes = Vec::new();
        let mut n = 1;

        let mut paths: Vec<&String> = base.files.keys().chain(head.files.keys()).collect();
        paths.sort();
        paths.dedup();

        for path in paths {
            let Some(config) = self.config_for(path) else {
                continue;
            };
            let base_src = base.files.get(path);
            let head_src = head.files.get(path);

            match (base_src, head_src) {
                (Some(b), Some(h)) => {
                    let base_fns =
                        extract_functions(parse_source(&config.language, b).as_ref(), b, config);
                    let head_fns =
                        extract_functions(parse_source(&config.language, h).as_ref(), h, config);
                    for (name, (h_src, h_row)) in &head_fns {
                        match base_fns.get(name) {
                            Some((b_src, _)) if b_src != h_src => {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    *h_row,
                                    format!("both sides changed `{}`", name),
                                    Severity::Review,
                                ));
                            }
                            None => {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    *h_row,
                                    format!("added function `{}`", name),
                                    Severity::Review,
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
                                Severity::Review,
                            ));
                        }
                    }
                }
                (Some(_), None) => {
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        "file removed".to_string(),
                        Severity::Review,
                    ));
                }
                (None, Some(_)) => {
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        "file added".to_string(),
                        Severity::Review,
                    ));
                }
                (None, None) => {}
            }
        }
        Ok(disputes)
    }

    /// Perform a 3-way semantic diff between a common merge-base ancestor,
    /// the target (ours) branch, and the incoming (theirs/head) branch.
    pub fn diff_3way(
        &self,
        base: &Snapshot,
        ours: &Snapshot,
        theirs: &Snapshot,
    ) -> anyhow::Result<Vec<Dispute>> {
        let mut disputes = Vec::new();
        let mut n = 1;

        let mut paths: Vec<&String> = base
            .files
            .keys()
            .chain(ours.files.keys())
            .chain(theirs.files.keys())
            .collect();
        paths.sort();
        paths.dedup();

        for path in paths {
            let Some(config) = self.config_for(path) else {
                continue;
            };
            let b_file = base.files.get(path);
            let o_file = ours.files.get(path);
            let t_file = theirs.files.get(path);

            match (b_file, o_file, t_file) {
                // File exists in all three
                (Some(b_src), Some(o_src), Some(t_src)) => {
                    let b_fns = extract_functions(
                        parse_source(&config.language, b_src).as_ref(),
                        b_src,
                        config,
                    );
                    let o_fns = extract_functions(
                        parse_source(&config.language, o_src).as_ref(),
                        o_src,
                        config,
                    );
                    let t_fns = extract_functions(
                        parse_source(&config.language, t_src).as_ref(),
                        t_src,
                        config,
                    );

                    let mut all_fn_names: Vec<&String> = b_fns
                        .keys()
                        .chain(o_fns.keys())
                        .chain(t_fns.keys())
                        .collect();
                    all_fn_names.sort();
                    all_fn_names.dedup();

                    for name in all_fn_names {
                        let b_fn = b_fns.get(name);
                        let o_fn = o_fns.get(name);
                        let t_fn = t_fns.get(name);

                        let b_body = b_fn.map(|(s, _)| s.as_str());
                        let o_body = o_fn.map(|(s, _)| s.as_str());
                        let t_body = t_fn.map(|(s, _)| s.as_str());
                        let row = t_fn
                            .map(|(_, r)| *r)
                            .or_else(|| o_fn.map(|(_, r)| *r))
                            .unwrap_or(0);

                        // If both matches base, unchanged
                        if o_body == b_body && t_body == b_body {
                            continue;
                        }

                        // Case 1: Unilateral change by incoming (theirs)
                        if o_body == b_body && t_body != b_body {
                            match (b_body, t_body) {
                                (None, Some(_)) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!("incoming branch added function `{}`", name),
                                        Severity::Low,
                                    ));
                                }
                                (Some(_), None) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!("incoming branch removed function `{}`", name),
                                        Severity::Review,
                                    ));
                                }
                                (Some(_), Some(_)) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!("incoming branch modified function `{}`", name),
                                        Severity::Review,
                                    ));
                                }
                                (None, None) => {}
                            }
                        }
                        // Case 2: Unilateral change by target (ours) - no dispute for incoming merge
                        else if o_body != b_body && t_body == b_body {
                            continue;
                        }
                        // Case 3: Both branches modified relative to base
                        else {
                            if o_body == t_body {
                                // Convergent clean change
                                continue;
                            }
                            // Divergent modifications -> 3-way semantic conflict
                            match (o_body, t_body) {
                                (Some(_), Some(_)) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!(
                                            "3-way conflict: both branches modified function `{}` differently",
                                            name
                                        ),
                                        Severity::High,
                                    ));
                                }
                                (None, Some(_)) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!(
                                            "3-way conflict: function `{}` modified in incoming branch but deleted in target",
                                            name
                                        ),
                                        Severity::High,
                                    ));
                                }
                                (Some(_), None) => {
                                    disputes.push(meaning(
                                        &mut n,
                                        path,
                                        row,
                                        format!(
                                            "3-way conflict: function `{}` deleted in incoming branch but modified in target",
                                            name
                                        ),
                                        Severity::High,
                                    ));
                                }
                                (None, None) => {}
                            }
                        }
                    }
                }
                // File deleted in target, modified in incoming
                (Some(_), None, Some(_)) => {
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        "3-way conflict: file deleted in target branch but modified in incoming branch".to_string(),
                        Severity::High,
                    ));
                }
                // File modified in target, deleted in incoming
                (Some(_), Some(_), None) => {
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        "3-way conflict: file modified in target branch but deleted in incoming branch".to_string(),
                        Severity::High,
                    ));
                }
                // File added only in incoming
                (None, None, Some(_)) => {
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        "incoming branch added file".to_string(),
                        Severity::Low,
                    ));
                }
                // File deleted in incoming (and base existed)
                (Some(_), None, None) => {
                    // Both deleted it, clean
                }
                _ => {}
            }
        }

        Ok(disputes)
    }
}

fn parse_source(language: &Language, source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    parser.parse(source, None)
}

fn meaning(n: &mut i32, path: &str, row: usize, detail: String, severity: Severity) -> Dispute {
    let id = format!("D{:03}", n);
    *n += 1;
    Dispute {
        id,
        location: format!("{}:{}", path, row),
        kind: Kind::Meaning,
        severity,
        detail,
    }
}

fn extract_functions(
    tree: Option<&Tree>,
    source: &str,
    config: &LangConfig,
) -> HashMap<String, (String, usize)> {
    let mut map = HashMap::new();
    let Some(tree) = tree else {
        return map;
    };
    collect(tree.root_node(), source, &mut map, config);
    map
}

fn collect(
    node: Node,
    source: &str,
    map: &mut HashMap<String, (String, usize)>,
    config: &LangConfig,
) {
    if config.function_kinds.contains(&node.kind()) {
        insert_named(node, "name", node, source, map);
    }
    for wrapped in config.wrapped_functions {
        if node.kind() == wrapped.node_kind {
            if let Some(body) = node.child_by_field_name(wrapped.body_field) {
                if wrapped.body_kinds.contains(&body.kind()) {
                    insert_named(node, wrapped.name_field, body, source, map);
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, map, config);
    }
}

/// Record a function in `map` using the identifier carried in `name_field` of
/// `name_holder` and the body text from `body`.
fn insert_named(
    name_holder: Node,
    name_field: &str,
    body: Node,
    source: &str,
    map: &mut HashMap<String, (String, usize)>,
) {
    let Some(name_node) = name_holder.child_by_field_name(name_field) else {
        return;
    };
    let name = name_node
        .utf8_text(source.as_bytes())
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return;
    }
    let src = body.utf8_text(source.as_bytes()).unwrap_or("").to_string();
    let row = body.start_position().row + 1;
    map.insert(name, (src, row));
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

    #[test]
    fn test_engine_diff_3way_conflict_and_clean() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "src/lib.rs".into(),
            "pub fn clean_fn() -> i32 { 0 }\npub fn conflict_fn() -> i32 { 10 }\n".into(),
        );

        let mut ours = Snapshot::default();
        ours.files.insert(
            "src/lib.rs".into(),
            "pub fn clean_fn() -> i32 { 0 }\npub fn conflict_fn() -> i32 { 20 }\n".into(),
        );

        let mut theirs = Snapshot::default();
        theirs.files.insert(
            "src/lib.rs".into(),
            "pub fn clean_fn() -> i32 { 99 }\npub fn conflict_fn() -> i32 { 30 }\n".into(),
        );

        let disputes = eng.diff_3way(&base, &ours, &theirs).unwrap();
        assert_eq!(disputes.len(), 2);

        let conflict = disputes
            .iter()
            .find(|d| d.detail.contains("conflict_fn"))
            .unwrap();
        assert_eq!(conflict.severity, Severity::High);
        assert!(conflict.detail.contains("3-way conflict"));

        let clean = disputes
            .iter()
            .find(|d| d.detail.contains("clean_fn"))
            .unwrap();
        assert_eq!(clean.severity, Severity::Review);
        assert!(clean.detail.contains("incoming branch modified"));
    }

    #[test]
    fn test_engine_python_function_detection() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "app.py".into(),
            "def greet(name):\n    return f\"hi {name}\"\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "app.py".into(),
            "def greet(name):\n    return f\"hello {name}\"\n\ndef bye(name):\n    return f\"bye {name}\"\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 2);

        let details: Vec<&str> = disputes.iter().map(|d| d.detail.as_str()).collect();
        assert!(details.iter().any(|d| d.contains("both sides changed `greet`")));
        assert!(details.iter().any(|d| d.contains("added function `bye`")));
    }

    #[test]
    fn test_engine_go_function_and_method_detection() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "server.go".into(),
            "package main\n\nfunc greet(name string) string {\n\treturn \"hi \" + name\n}\n\ntype counter struct{ n int }\n\nfunc (c *counter) inc() {\n\tc.n++\n}\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "server.go".into(),
            "package main\n\nfunc greet(name string) string {\n\treturn \"hello \" + name\n}\n\ntype counter struct{ n int }\n\nfunc (c *counter) inc() {\n\tc.n += 2\n}\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 2);

        let details: Vec<&str> = disputes.iter().map(|d| d.detail.as_str()).collect();
        assert!(details.iter().any(|d| d.contains("both sides changed `greet`")));
        assert!(details.iter().any(|d| d.contains("both sides changed `inc`")));
    }

    #[test]
    fn test_engine_js_arrows_and_declarations() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "index.js".into(),
            "function add(a, b) {\n  return a + b;\n}\nconst double = (x) => x * 2;\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "index.js".into(),
            "function add(a, b) {\n  return a + b + 1;\n}\nconst double = (x) => x * 3;\nconst triple = (x) => x * 3;\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 3);

        let details: Vec<&str> = disputes.iter().map(|d| d.detail.as_str()).collect();
        assert!(details.iter().any(|d| d.contains("both sides changed `add`")));
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `double`")));
        assert!(details
            .iter()
            .any(|d| d.contains("added function `triple`")));
    }

    #[test]
    fn test_engine_js_assignment_arrow() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "index.js".into(),
            "let double;\ndouble = (x) => x * 2;\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "index.js".into(),
            "let double;\ndouble = (x) => x * 3;\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 1);
        assert_eq!(disputes[0].detail, "both sides changed `double`");
    }

    #[test]
    fn test_engine_3way_mixed_languages() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files
            .insert("lib.rs".into(), "pub fn f() -> i32 { 1 }\n".into());
        base.files
            .insert("app.py".into(), "def f():\n    return 1\n".into());

        let mut ours = Snapshot::default();
        ours.files
            .insert("lib.rs".into(), "pub fn f() -> i32 { 2 }\n".into());
        ours.files
            .insert("app.py".into(), "def f():\n    return 2\n".into());

        let mut theirs = Snapshot::default();
        theirs.files
            .insert("lib.rs".into(), "pub fn f() -> i32 { 3 }\n".into());
        theirs.files
            .insert("app.py".into(), "def f():\n    return 3\n".into());

        let disputes = eng.diff_3way(&base, &ours, &theirs).unwrap();
        assert_eq!(disputes.len(), 2);
        assert!(disputes.iter().all(|d| d.severity == Severity::High));
    }
}
