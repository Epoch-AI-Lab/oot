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
        let mut parser = Parser::new();
        let mut disputes = Vec::new();
        let mut n = 1;

        let mut paths: Vec<&String> = base.files.keys().chain(head.files.keys()).collect();
        paths.sort();
        paths.dedup();

        for path in paths {
            let Some(config) = self.config_for(path) else {
                continue;
            };
            let base_src = base.files.get(path).map(|v| as_text(v));
            let head_src = head.files.get(path).map(|v| as_text(v));

            match (base_src, head_src) {
                (Some(b), Some(h)) => {
                    let (base_fns, mut dupes) = extract_functions(
                        parse_source(&mut parser, &config.language, &b).as_ref(),
                        &b,
                        config,
                    );
                    let (head_fns, head_dupes) = extract_functions(
                        parse_source(&mut parser, &config.language, &h).as_ref(),
                        &h,
                        config,
                    );
                    dupes.extend(head_dupes);
                    dupes.sort();
                    dupes.dedup();

                    for name in &dupes {
                        disputes.push(meaning(
                            &mut n,
                            path,
                            0,
                            format!(
                                "function `{name}` is defined multiple times in this file; tracked only by first occurrence"
                            ),
                            Severity::Review,
                        ));
                    }

                    let mut added: Vec<(&String, usize)> = Vec::new();
                    let mut names: Vec<&String> = head_fns.keys().collect();
                    names.sort();
                    for name in names {
                        let (h_src, h_row, _) = &head_fns[name];
                        match base_fns.get(name) {
                            Some((b_src, _, _)) if b_src != h_src => {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    *h_row,
                                    format!("both sides changed `{}`", name),
                                    Severity::Review,
                                ));
                            }
                            None => {
                                added.push((name, *h_row));
                            }
                            _ => {}
                        }
                    }
                    let mut removed: Vec<&String> = base_fns
                        .keys()
                        .filter(|name| !head_fns.contains_key(*name))
                        .collect();
                    removed.sort();

                    // Pair removals with additions of identical source text:
                    // that is a rename, not two separate changes.
                    let mut consumed = vec![false; added.len()];
                    let mut leftover_removed: Vec<&String> = Vec::new();
                    for old in &removed {
                        let old_sig = &base_fns[*old].2;
                        let found = added
                            .iter()
                            .enumerate()
                            .find(|(i, (new, _))| !consumed[*i] && &head_fns[*new].2 == old_sig);
                        if let Some((i, (new, _))) = found {
                            consumed[i] = true;
                            disputes.push(meaning(
                                &mut n,
                                path,
                                head_fns[*new].1,
                                format!("renamed function `{}` to `{}`", old, new),
                                Severity::Review,
                            ));
                        } else {
                            leftover_removed.push(old);
                        }
                    }
                    for (i, (new, row)) in added.iter().enumerate() {
                        if !consumed[i] {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                *row,
                                format!("added function `{}`", new),
                                Severity::Review,
                            ));
                        }
                    }
                    for name in leftover_removed {
                        disputes.push(meaning(
                            &mut n,
                            path,
                            0,
                            format!("removed function `{}`", name),
                            Severity::Review,
                        ));
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
                (None, Some(h)) => {
                    let summary = file_function_summary(
                        parse_source(&mut parser, &config.language, &h).as_ref(),
                        &h,
                        config,
                    );
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        format!("file added ({})", summary),
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
        let mut parser = Parser::new();
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
            let b_file = base.files.get(path).map(|v| as_text(v));
            let o_file = ours.files.get(path).map(|v| as_text(v));
            let t_file = theirs.files.get(path).map(|v| as_text(v));

            match (b_file, o_file, t_file) {
                // File exists in all three
                (Some(b_src), Some(o_src), Some(t_src)) => {
                    let (b_fns, mut dupes) = extract_functions(
                        parse_source(&mut parser, &config.language, &b_src).as_ref(),
                        &b_src,
                        config,
                    );
                    let (o_fns, o_dupes) = extract_functions(
                        parse_source(&mut parser, &config.language, &o_src).as_ref(),
                        &o_src,
                        config,
                    );
                    let (t_fns, t_dupes) = extract_functions(
                        parse_source(&mut parser, &config.language, &t_src).as_ref(),
                        &t_src,
                        config,
                    );
                    dupes.extend(o_dupes);
                    dupes.extend(t_dupes);
                    dupes.sort();
                    dupes.dedup();

                    for name in &dupes {
                        disputes.push(meaning(
                            &mut n,
                            path,
                            0,
                            format!(
                                "function `{name}` is defined multiple times in this file; tracked only by first occurrence"
                            ),
                            Severity::Review,
                        ));
                    }

                    let mut all_fn_names: Vec<&String> = b_fns
                        .keys()
                        .chain(o_fns.keys())
                        .chain(t_fns.keys())
                        .collect();
                    all_fn_names.sort();
                    all_fn_names.dedup();

                    let mut pending_added: Vec<(String, usize)> = Vec::new();
                    let mut pending_removed: Vec<String> = Vec::new();

                    for name in all_fn_names {
                        let b_fn = b_fns.get(name);
                        let o_fn = o_fns.get(name);
                        let t_fn = t_fns.get(name);

                        let b_body = b_fn.map(|(s, _, _)| s.as_str());
                        let o_body = o_fn.map(|(s, _, _)| s.as_str());
                        let t_body = t_fn.map(|(s, _, _)| s.as_str());
                        let row = t_fn
                            .map(|(_, r, _)| *r)
                            .or_else(|| o_fn.map(|(_, r, _)| *r))
                            .unwrap_or(0);

                        // If both matches base, unchanged
                        if o_body == b_body && t_body == b_body {
                            continue;
                        }

                        // Case 1: Unilateral change by incoming (theirs)
                        if o_body == b_body && t_body != b_body {
                            match (b_body, t_body) {
                                (None, Some(_)) => {
                                    pending_added.push((name.clone(), row));
                                }
                                (Some(_), None) => {
                                    pending_removed.push(name.clone());
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

                    // Pair incoming removals with incoming additions of
                    // identical source text: a rename, not two changes.
                    pending_added.sort_by(|a, b| a.0.cmp(&b.0));
                    pending_removed.sort();
                    let mut consumed = vec![false; pending_added.len()];
                    let mut leftover_removed: Vec<String> = Vec::new();
                    for old in &pending_removed {
                        let old_sig = &b_fns[old].2;
                        let found = pending_added
                            .iter()
                            .enumerate()
                            .find(|(i, (new, _))| !consumed[*i] && &t_fns[new].2 == old_sig);
                        if let Some((i, (new, row))) = found {
                            consumed[i] = true;
                            disputes.push(meaning(
                                &mut n,
                                path,
                                *row,
                                format!("incoming branch renamed function `{}` to `{}`", old, new),
                                Severity::Review,
                            ));
                        } else {
                            leftover_removed.push(old.clone());
                        }
                    }
                    for (i, (new, row)) in pending_added.iter().enumerate() {
                        if !consumed[i] {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                *row,
                                format!("incoming branch added function `{}`", new),
                                Severity::Low,
                            ));
                        }
                    }
                    for name in leftover_removed {
                        disputes.push(meaning(
                            &mut n,
                            path,
                            0,
                            format!("incoming branch removed function `{}`", name),
                            Severity::Review,
                        ));
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
                (None, None, Some(t)) => {
                    let summary = file_function_summary(
                        parse_source(&mut parser, &config.language, &t).as_ref(),
                        &t,
                        config,
                    );
                    disputes.push(meaning(
                        &mut n,
                        path,
                        0,
                        format!("incoming branch added file ({})", summary),
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

/// Tracked functions keyed by name: source text, 1-based row, and a rename
/// signature (the body with its own name blanked out).
type FunctionMap = HashMap<String, (String, usize, String)>;

fn parse_source(parser: &mut Parser, language: &Language, source: &str) -> Option<Tree> {
    parser.set_language(language).ok()?;
    parser.parse(source, None)
}

/// Lossily convert raw snapshot bytes to text for parsing.
///
/// Only the structural engine touches this; change detection compares bytes,
/// so two distinct binary files never compare equal even if their lossy
/// text collapses.
fn as_text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
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

/// Describe what a newly added file contains: how many tracked functions and
/// up to three names, so a docket reader knows what arrived without opening
/// the file.
fn file_function_summary(tree: Option<&Tree>, source: &str, config: &LangConfig) -> String {
    let (fns, _) = extract_functions(tree, source, config);
    let mut names: Vec<&String> = fns.keys().collect();
    names.sort();
    if names.is_empty() {
        return "no functions detected".to_string();
    }
    let count = names.len();
    let preview: Vec<String> = names.iter().take(3).map(|s| s.to_string()).collect();
    let noun = if count == 1 { "function" } else { "functions" };
    if count > 3 {
        format!("{} {}: {}, …", count, noun, preview.join(", "))
    } else {
        format!("{} {}: {}", count, noun, preview.join(", "))
    }
}

/// Extract tracked functions as `name -> (source text, 1-based row, rename
/// signature with the name blanked out)`, plus the
/// list of names defined more than once (only the first occurrence is kept).
fn extract_functions(
    tree: Option<&Tree>,
    source: &str,
    config: &LangConfig,
) -> (FunctionMap, Vec<String>) {
    let mut map = HashMap::new();
    let mut duplicates = Vec::new();
    if let Some(tree) = tree {
        collect(tree.root_node(), source, &mut map, &mut duplicates, config);
    }
    (map, duplicates)
}

fn collect(
    node: Node,
    source: &str,
    map: &mut FunctionMap,
    duplicates: &mut Vec<String>,
    config: &LangConfig,
) {
    let mut matched = false;
    for kind in config.function_kinds {
        if node.kind() == kind.node_kind {
            matched = true;
            if let Some(key) = config.function_key(kind, node, source) {
                let name_node = node.child_by_field_name("name");
                insert(key, node, name_node, source, map, duplicates);
            }
        }
    }
    for wrapped in config.wrapped_functions {
        if node.kind() != wrapped.node_kind {
            continue;
        }
        matched = true;
        let (Some(name_node), Some(body)) = (
            node.child_by_field_name(wrapped.name_field),
            node.child_by_field_name(wrapped.body_field),
        ) else {
            continue;
        };
        if wrapped.name_kinds.contains(&name_node.kind())
            && wrapped.body_kinds.contains(&body.kind())
        {
            let key = name_node
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .to_string();
            insert(key, body, None, source, map, duplicates);
        }
    }
    // Do not recurse into nodes that yielded a function: nested definitions
    // are covered by the enclosing function's source span.
    if matched {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, map, duplicates, config);
    }
}

/// Record a named function under `key`, with `body` as its source. First
/// occurrence wins; later same-key definitions are reported as duplicates.
fn insert(
    key: String,
    body: Node,
    name_node: Option<Node>,
    source: &str,
    map: &mut FunctionMap,
    duplicates: &mut Vec<String>,
) {
    if key.is_empty() {
        return;
    }
    match map.entry(key.clone()) {
        std::collections::hash_map::Entry::Occupied(_) => duplicates.push(key),
        std::collections::hash_map::Entry::Vacant(slot) => {
            let src = body.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            let row = body.start_position().row + 1;
            // Signature: the body with its own name blanked, so two functions
            // that differ only by what they are called compare equal and pair
            // as a rename.
            let signature = match name_node {
                Some(n)
                    if n.start_byte() >= body.start_byte() && n.end_byte() <= body.end_byte() =>
                {
                    let rel_start = n.start_byte() - body.start_byte();
                    let rel_end = n.end_byte() - body.start_byte();
                    let mut sig = String::with_capacity(src.len());
                    sig.push_str(&src[..rel_start]);
                    sig.push('\u{0}');
                    sig.push_str(&src[rel_end..]);
                    sig
                }
                _ => src.clone(),
            };
            slot.insert((src, row, signature));
        }
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
        // `hello` changed on both sides. `old_fn` -> `new_fn` have identical
        // (empty) bodies apart from the name, so they pair as a rename.
        assert_eq!(disputes.len(), 2);

        let details: Vec<&str> = disputes.iter().map(|d| d.detail.as_str()).collect();
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `hello`")));
        assert!(details
            .iter()
            .any(|d| d.contains("renamed function `old_fn` to `new_fn`")));
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
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `greet`")));
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
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `greet`")));
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `inc`")));
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
        assert!(details
            .iter()
            .any(|d| d.contains("both sides changed `add`")));
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
    fn test_engine_js_class_field_arrow() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "index.js".into(),
            "class Counter {\n  constructor() { this.n = 0; }\n  next = () => this.n++;\n}\n"
                .into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "index.js".into(),
            "class Counter {\n  constructor() { this.n = 0; }\n  next = () => ++this.n;\n}\n"
                .into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert_eq!(disputes.len(), 1);
        assert_eq!(disputes[0].detail, "both sides changed `next`");
    }

    #[test]
    fn test_engine_js_member_assignment_ignored() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "index.js".into(),
            "const obj = {};\nobj.handle = () => 1;\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "index.js".into(),
            "const obj = {};\nobj.handle = () => 2;\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert!(
            disputes.is_empty(),
            "member-expression assignment should not be treated as a named function"
        );
    }

    #[test]
    fn test_engine_go_method_collision_flagged_ambiguous() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "server.go".into(),
            "package main\n\ntype A struct{ v int }\n\nfunc (a *A) hit() {\n\ta.v = 1\n}\n\ntype B struct{ v int }\n\nfunc (b *B) hit() {\n\tb.v = 1\n}\n".into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "server.go".into(),
            "package main\n\ntype A struct{ v int }\n\nfunc (a *A) hit() {\n\ta.v = 2\n}\n\ntype B struct{ v int }\n\nfunc (b *B) hit() {\n\tb.v = 1\n}\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert!(
            disputes
                .iter()
                .any(|d| d.detail.contains("`hit`") && d.detail.contains("multiple times")),
            "same-named methods must surface an ambiguity dispute, got {:?}",
            disputes
        );
    }

    #[test]
    fn test_engine_rust_impl_method_collision_flagged_ambiguous() {
        let eng = Engine::new().unwrap();

        let mut base = Snapshot::default();
        base.files.insert(
            "src/lib.rs".into(),
            "struct A;\nstruct B;\n\nimpl A { fn hit(&self) {} }\nimpl B { fn hit(&self) {} }\n"
                .into(),
        );

        let mut head = Snapshot::default();
        head.files.insert(
            "src/lib.rs".into(),
            "struct A;\nstruct B;\n\nimpl A { fn hit(&self) { let _ = 1; } }\nimpl B { fn hit(&self) {} }\n".into(),
        );

        let disputes = eng.diff_snapshots(&base, &head).unwrap();
        assert!(
            disputes
                .iter()
                .any(|d| d.detail.contains("`hit`") && d.detail.contains("multiple times")),
            "same-named impl methods must surface an ambiguity dispute, got {:?}",
            disputes
        );
    }

    #[test]
    fn test_engine_impl_move_is_not_a_conflict() {
        // Regression: receiver/impl-qualified keys made a pure refactor
        // (function moved between impl blocks) + an unrelated in-place edit
        // look like a High-severity 3-way conflict, flipping the verdict to
        // Blocked. Bare-name keys keep identity stable under refactoring.
        let eng = Engine::new().unwrap();

        let base_src = "struct A; struct B;\nimpl A { fn run(&self) -> i32 { 42 } }\n";
        let ours_src = "struct A; struct B;\nimpl B { fn run(&self) -> i32 { 42 } }\n";
        let theirs_src = "struct A; struct B;\nimpl A { fn run(&self) -> i32 { 43 } }\n";

        let snap = |s: &str| {
            let mut x = Snapshot::default();
            x.files.insert("src/lib.rs".into(), s.into());
            x
        };

        let disputes = eng
            .diff_3way(&snap(base_src), &snap(ours_src), &snap(theirs_src))
            .unwrap();
        assert!(
            !disputes.iter().any(|d| d.severity == Severity::High),
            "refactor + unrelated edit must not produce High conflicts, got {:?}",
            disputes
        );
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
        theirs
            .files
            .insert("lib.rs".into(), "pub fn f() -> i32 { 3 }\n".into());
        theirs
            .files
            .insert("app.py".into(), "def f():\n    return 3\n".into());

        let disputes = eng.diff_3way(&base, &ours, &theirs).unwrap();
        assert_eq!(disputes.len(), 2);
        assert!(disputes.iter().all(|d| d.severity == Severity::High));
    }
}
