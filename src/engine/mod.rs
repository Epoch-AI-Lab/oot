//! Structural difference engine using Tree-sitter.
//!
//! Compares snapshots semantically across function definitions
//! rather than line-by-line diffs.

use crate::change::Snapshot;
use crate::dispute::{Dispute, Kind, Severity};
use anyhow::Context;
use std::collections::HashMap;
use tree_sitter::{Node, Parser, Tree};

/// Languages the structural engine can parse.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Lang {
    Rust,
    Go,
    JavaScript,
}

impl Lang {
    /// Detect the language of a file from its extension.
    fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Lang::Rust),
            "go" => Some(Lang::Go),
            "js" | "mjs" | "cjs" => Some(Lang::JavaScript),
            _ => None,
        }
    }

    fn language(self) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }

    /// Node kinds that declare a function in this language. Kinds without a
    /// `name` field fall back to the enclosing variable binding's name.
    fn function_kinds(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &["function_item"],
            Lang::Go => &["function_declaration", "method_declaration"],
            Lang::JavaScript => &[
                "function_declaration",
                "function_expression",
                "generator_function_declaration",
                "generator_function",
                "method_definition",
                "arrow_function",
            ],
        }
    }
}

/// Structural difference engine for code snapshots.
pub struct Engine {
    languages: HashMap<Lang, tree_sitter::Language>,
}

impl Engine {
    /// Create a new structural diff engine with one validated grammar per
    /// supported language.
    ///
    /// Fails loudly if any grammar's ABI is incompatible with the runtime — this
    /// doubles as a version-drift tripwire for CI.
    pub fn new() -> anyhow::Result<Self> {
        let mut languages = HashMap::new();
        for lang in [Lang::Rust, Lang::Go, Lang::JavaScript] {
            let language = lang.language();
            let mut probe = Parser::new();
            probe
                .set_language(&language)
                .with_context(|| format!("loading grammar for {lang:?}"))?;
            languages.insert(lang, language);
        }
        Ok(Engine { languages })
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
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let language = &self.languages[&lang];
            let base_src = base.files.get(path);
            let head_src = head.files.get(path);

            match (base_src, head_src) {
                (Some(b), Some(h)) => {
                    let (base_fns, mut dupes) =
                        extract_functions(parse_source(language, b)?.as_ref(), b, lang);
                    let (head_fns, head_dupes) =
                        extract_functions(parse_source(language, h)?.as_ref(), h, lang);
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

                    let mut names: Vec<&String> = head_fns.keys().collect();
                    names.sort();
                    for name in names {
                        let (h_src, h_row) = &head_fns[name];
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
                    let mut removed: Vec<&String> = base_fns
                        .keys()
                        .filter(|name| !head_fns.contains_key(*name))
                        .collect();
                    removed.sort();
                    for name in removed {
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
            let Some(lang) = Lang::from_path(path) else {
                continue;
            };
            let language = &self.languages[&lang];
            let b_file = base.files.get(path);
            let o_file = ours.files.get(path);
            let t_file = theirs.files.get(path);

            match (b_file, o_file, t_file) {
                // File exists in all three
                (Some(b_src), Some(o_src), Some(t_src)) => {
                    let (b_fns, mut dupes) =
                        extract_functions(parse_source(language, b_src)?.as_ref(), b_src, lang);
                    let (o_fns, o_dupes) =
                        extract_functions(parse_source(language, o_src)?.as_ref(), o_src, lang);
                    let (t_fns, t_dupes) =
                        extract_functions(parse_source(language, t_src)?.as_ref(), t_src, lang);
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

fn parse_source(language: &tree_sitter::Language, source: &str) -> anyhow::Result<Option<Tree>> {
    let mut parser = Parser::new();
    parser.set_language(language)?;
    Ok(parser.parse(source, None))
}

/// Extract tracked functions as `name -> (source text, 1-based row)`, plus the
/// list of names defined more than once (only the first occurrence is kept).
fn extract_functions(
    tree: Option<&Tree>,
    source: &str,
    lang: Lang,
) -> (HashMap<String, (String, usize)>, Vec<String>) {
    let mut map = HashMap::new();
    let mut duplicates = Vec::new();
    if let Some(tree) = tree {
        collect(tree.root_node(), source, lang, &mut map, &mut duplicates);
    }
    (map, duplicates)
}

fn collect(
    node: Node,
    source: &str,
    lang: Lang,
    map: &mut HashMap<String, (String, usize)>,
    duplicates: &mut Vec<String>,
) {
    if lang.function_kinds().contains(&node.kind()) {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(str::to_string)
            .or_else(|| inherited_name(node, source));
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            match map.entry(name.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    duplicates.push(name);
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let src = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                    let row = node.start_position().row + 1;
                    slot.insert((src, row));
                }
            }
        }
        // Do not recurse into matched functions: nested definitions are covered
        // by the enclosing function's source span.
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, source, lang, map, duplicates);
    }
}

/// Name for anonymous functions bound to a variable: `const f = () => {}`.
fn inherited_name(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    if parent.kind() != "variable_declarator" {
        return None;
    }
    parent
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_grammars_abi_compatible() {
        // Tripwire: if a grammar crate is bumped past the runtime's supported
        // ABI range, set_language fails here instead of at some user's build.
        for lang in [Lang::Rust, Lang::Go, Lang::JavaScript] {
            let mut parser = Parser::new();
            parser.set_language(&lang.language()).unwrap_or_else(|e| {
                panic!(
                    "grammar for {:?} is ABI-incompatible with runtime: {}",
                    lang, e
                )
            });
        }
    }

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
}
