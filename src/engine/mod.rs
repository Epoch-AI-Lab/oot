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
                    let base_fns = extract_functions(
                        parse_source(&mut parser, &config.language, &b).as_ref(),
                        &b,
                        config,
                    );
                    let head_fns = extract_functions(
                        parse_source(&mut parser, &config.language, &h).as_ref(),
                        &h,
                        config,
                    );

                    let mut names: Vec<&String> = base_fns.keys().chain(head_fns.keys()).collect();
                    names.sort();
                    names.dedup();

                    let empty: Vec<FnDef> = Vec::new();
                    let mut pending_added: Vec<(&str, FnDef)> = Vec::new();
                    let mut pending_removed: Vec<(&str, FnDef)> = Vec::new();

                    for name in names {
                        let b_list = base_fns.get(name).unwrap_or(&empty);
                        let h_list = head_fns.get(name).unwrap_or(&empty);
                        let (changed, gone, fresh) = align_defs(b_list, h_list);
                        for (_, hi) in &changed {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                h_list[*hi].row,
                                format!("both sides changed `{}`", name),
                                Severity::Review,
                            ));
                        }
                        for bi in gone {
                            pending_removed.push((name, b_list[bi].clone()));
                        }
                        for hi in fresh {
                            pending_added.push((name, h_list[hi].clone()));
                        }
                    }

                    // Pair removals with additions of identical blanked-name
                    // source text: that is a rename, not two separate changes.
                    pending_added.sort_by(|a, b| a.0.cmp(b.0));
                    pending_removed.sort_by(|a, b| a.0.cmp(b.0));
                    let mut consumed = vec![false; pending_added.len()];
                    let mut leftover_removed: Vec<&str> = Vec::new();
                    for (old_name, old_def) in &pending_removed {
                        let found = pending_added.iter().enumerate().find(|(i, (_, new_def))| {
                            !consumed[*i] && new_def.signature == old_def.signature
                        });
                        if let Some((i, (new_name, new_def))) = found {
                            consumed[i] = true;
                            disputes.push(meaning(
                                &mut n,
                                path,
                                new_def.row,
                                format!("renamed function `{}` to `{}`", old_name, new_name),
                                Severity::Review,
                            ));
                        } else {
                            leftover_removed.push(old_name);
                        }
                    }
                    for (i, (new_name, new_def)) in pending_added.iter().enumerate() {
                        if !consumed[i] {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                new_def.row,
                                format!("added function `{}`", new_name),
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
                    let b_fns = extract_functions(
                        parse_source(&mut parser, &config.language, &b_src).as_ref(),
                        &b_src,
                        config,
                    );
                    let o_fns = extract_functions(
                        parse_source(&mut parser, &config.language, &o_src).as_ref(),
                        &o_src,
                        config,
                    );
                    let t_fns = extract_functions(
                        parse_source(&mut parser, &config.language, &t_src).as_ref(),
                        &t_src,
                        config,
                    );

                    let mut all_fn_names: Vec<&String> = b_fns
                        .keys()
                        .chain(o_fns.keys())
                        .chain(t_fns.keys())
                        .collect();
                    all_fn_names.sort();
                    all_fn_names.dedup();

                    let mut pending_added: Vec<(String, FnDef)> = Vec::new();
                    let mut pending_removed: Vec<(String, FnDef)> = Vec::new();
                    let empty: Vec<FnDef> = Vec::new();

                    for name in all_fn_names {
                        let b_list = b_fns.get(name).unwrap_or(&empty);
                        let o_list = o_fns.get(name).unwrap_or(&empty);
                        let t_list = t_fns.get(name).unwrap_or(&empty);

                        let (o_changed, o_gone, o_fresh) = align_defs(b_list, o_list);
                        let (t_changed, t_gone, t_fresh) = align_defs(b_list, t_list);

                        let ours_touched =
                            !(o_changed.is_empty() && o_gone.is_empty() && o_fresh.is_empty());
                        let theirs_touched =
                            !(t_changed.is_empty() && t_gone.is_empty() && t_fresh.is_empty());

                        // If both match base, unchanged
                        if !ours_touched && !theirs_touched {
                            continue;
                        }

                        // Case 1: Unilateral change by incoming (theirs)
                        if !ours_touched {
                            for (_, ti) in &t_changed {
                                disputes.push(meaning(
                                    &mut n,
                                    path,
                                    t_list[*ti].row,
                                    format!("incoming branch modified function `{}`", name),
                                    Severity::Review,
                                ));
                            }
                            for bi in &t_gone {
                                pending_removed.push((name.clone(), b_list[*bi].clone()));
                            }
                            for ti in &t_fresh {
                                pending_added.push((name.clone(), t_list[*ti].clone()));
                            }
                            continue;
                        }
                        // Case 2: Unilateral change by target (ours) - no dispute for incoming merge
                        if !theirs_touched {
                            continue;
                        }
                        // Case 3: Both branches modified relative to base
                        if same_defs(o_list, t_list) {
                            // Convergent clean change
                            continue;
                        }
                        // Divergent modifications -> 3-way semantic conflict
                        let row = t_list
                            .first()
                            .map(|d| d.row)
                            .or_else(|| o_list.first().map(|d| d.row))
                            .unwrap_or(0);
                        let detail = if !t_changed.is_empty() && !o_changed.is_empty() {
                            format!(
                                "3-way conflict: both branches modified function `{}` differently",
                                name
                            )
                        } else if !t_changed.is_empty() && !o_gone.is_empty() {
                            format!(
                                "3-way conflict: function `{}` modified in incoming branch but deleted in target",
                                name
                            )
                        } else if !t_gone.is_empty() && !o_changed.is_empty() {
                            format!(
                                "3-way conflict: function `{}` deleted in incoming branch but modified in target",
                                name
                            )
                        } else {
                            format!(
                                "3-way conflict: both branches changed function `{}` differently",
                                name
                            )
                        };
                        disputes.push(meaning(&mut n, path, row, detail, Severity::High));
                    }

                    // Pair incoming removals with incoming additions of
                    // identical blanked-name source text: a rename, not two
                    // changes.
                    pending_added.sort_by(|a, b| a.0.cmp(&b.0));
                    pending_removed.sort_by(|a, b| a.0.cmp(&b.0));
                    let mut consumed = vec![false; pending_added.len()];
                    let mut leftover_removed: Vec<String> = Vec::new();
                    for (old_name, old_def) in &pending_removed {
                        let found = pending_added.iter().enumerate().find(|(i, (_, new_def))| {
                            !consumed[*i] && new_def.signature == old_def.signature
                        });
                        if let Some((i, (new_name, new_def))) = found {
                            consumed[i] = true;
                            disputes.push(meaning(
                                &mut n,
                                path,
                                new_def.row,
                                format!(
                                    "incoming branch renamed function `{}` to `{}`",
                                    old_name, new_name
                                ),
                                Severity::Review,
                            ));
                        } else {
                            leftover_removed.push(old_name.clone());
                        }
                    }
                    for (i, (new, row)) in pending_added.iter().enumerate() {
                        if !consumed[i] {
                            disputes.push(meaning(
                                &mut n,
                                path,
                                row.row,
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

/// One extracted definition: source text, 1-based row, and a rename
/// signature (the body with its own name blanked out).
#[derive(Debug, Clone)]
struct FnDef {
    src: String,
    row: usize,
    signature: String,
}

/// Tracked functions grouped by bare name. A name may hold several
/// definitions in one file (same-named methods on different receivers);
/// each keeps its own entry and diffing runs a matching pass per group
/// instead of collapsing to the first occurrence.
type FunctionMap = HashMap<String, Vec<FnDef>>;

/// Align two def lists for one name: identical source text pairs first
/// (each def consumed once, silently — those are unchanged), then remaining
/// leftovers pair by relative order and count as changed. Returns matched
/// `(base_idx, head_idx)` pairs plus the base and head leftovers.
fn align_defs(base: &[FnDef], head: &[FnDef]) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut used_base = vec![false; base.len()];
    let mut used_head = vec![false; head.len()];
    for (hi, h) in head.iter().enumerate() {
        for (bi, b) in base.iter().enumerate() {
            if !used_base[bi] && b.src == h.src {
                used_base[bi] = true;
                used_head[hi] = true;
                break;
            }
        }
    }
    let rest_base: Vec<usize> = (0..base.len()).filter(|&i| !used_base[i]).collect();
    let rest_head: Vec<usize> = (0..head.len()).filter(|&i| !used_head[i]).collect();
    let pairs = rest_base.len().min(rest_head.len());
    let mut changed = Vec::new();
    for i in 0..pairs {
        changed.push((rest_base[i], rest_head[i]));
    }
    let gone = rest_base[pairs..].to_vec();
    let fresh = rest_head[pairs..].to_vec();
    (changed, gone, fresh)
}

/// Whether two def lists hold the same source texts, ignoring order.
fn same_defs(a: &[FnDef], b: &[FnDef]) -> bool {
    let mut sa: Vec<&str> = a.iter().map(|d| d.src.as_str()).collect();
    let mut sb: Vec<&str> = b.iter().map(|d| d.src.as_str()).collect();
    sa.sort_unstable();
    sb.sort_unstable();
    sa == sb
}

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
    let fns = extract_functions(tree, source, config);
    let mut names: Vec<&String> = fns.keys().collect();
    names.sort();
    if names.is_empty() {
        return "no functions detected".to_string();
    }
    let count: usize = fns.values().map(Vec::len).sum();
    let preview: Vec<String> = names.iter().take(3).map(|s| s.to_string()).collect();
    let noun = if count == 1 { "function" } else { "functions" };
    if count > 3 {
        format!("{} {}: {}, …", count, noun, preview.join(", "))
    } else {
        format!("{} {}: {}", count, noun, preview.join(", "))
    }
}

/// Extract tracked functions as `name -> [(source text, 1-based row, rename
/// signature with the name blanked out)]`, in document order.
fn extract_functions(tree: Option<&Tree>, source: &str, config: &LangConfig) -> FunctionMap {
    let mut map = HashMap::new();
    if let Some(tree) = tree {
        collect(tree.root_node(), source, &mut map, config);
    }
    map
}

fn collect(node: Node, source: &str, map: &mut FunctionMap, config: &LangConfig) {
    let mut matched = false;
    for kind in config.function_kinds {
        if node.kind() == kind.node_kind {
            matched = true;
            if let Some(key) = config.function_key(kind, node, source) {
                let name_node = node.child_by_field_name("name");
                insert(key, node, name_node, source, map);
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
            insert(key, body, None, source, map);
        }
    }
    // Do not recurse into nodes that yielded a function: nested definitions
    // are covered by the enclosing function's source span.
    if matched {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, map, config);
    }
}

/// Record a named function under `key`. Every definition is kept; diffing
/// aligns same-key groups by content (see [`align_defs`]).
fn insert(key: String, body: Node, name_node: Option<Node>, source: &str, map: &mut FunctionMap) {
    if key.is_empty() {
        return;
    }
    let src = body.utf8_text(source.as_bytes()).unwrap_or("").to_string();
    let row = body.start_position().row + 1;
    // Signature: the body with its own name blanked, so two functions
    // that differ only by what they are called compare equal and pair
    // as a rename.
    let signature = match name_node {
        Some(n) if n.start_byte() >= body.start_byte() && n.end_byte() <= body.end_byte() => {
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
    map.entry(key).or_default().push(FnDef {
        src,
        row,
        signature,
    });
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
    fn test_engine_go_same_name_methods_tracked_separately() {
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
        // Only A's `hit` changed; B's identical `hit` must not be dragged in.
        assert_eq!(disputes.len(), 1);
        assert_eq!(disputes[0].detail, "both sides changed `hit`");
    }

    #[test]
    fn test_engine_rust_impl_method_collision_tracked_separately() {
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
        assert_eq!(disputes.len(), 1);
        assert_eq!(
            disputes[0].detail, "both sides changed `hit`",
            "only the touched impl method must be reported, got {:?}",
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
