//! Per-language grammar configuration for the structural engine.
//!
//! The engine extracts named functions from a parse tree. Most languages
//! expose them as a single node kind carrying a `name` field. JavaScript
//! is the odd one out: a named arrow function (`const f = () => {}`) keeps
//! its callable on the `value` field of a `variable_declarator` and its name
//! on the declarator itself. That relationship is captured by
//! [`LangConfig::wrapped_functions`].

use tree_sitter::Language;

/// Callable node kinds that can sit on the right of a name.
const CALLABLE_KINDS: &[&str] = &["arrow_function", "function_expression", "generator_function"];

/// A wrapper node that carries a callable in one field and the function's
/// name in another. JavaScript's `const f = () => {}` keeps the callable on
/// the `value` field of a `variable_declarator`; `f = () => {}` keeps it on
/// the `right` field of an `assignment_expression`.
#[derive(Debug, Clone, Copy)]
pub struct WrappedFunction {
    /// The wrapper node kind (e.g. `"variable_declarator"`).
    pub node_kind: &'static str,
    /// Field on the wrapper that holds the function's name.
    pub name_field: &'static str,
    /// Field on the wrapper that holds the callable.
    pub body_field: &'static str,
    /// Callable node kinds that count as a named function.
    pub body_kinds: &'static [&'static str],
}

/// Static description of one language the engine can diff.
#[derive(Debug, Clone)]
pub struct LangConfig {
    /// Canonical language name (e.g. `"rust"`, `"python"`).
    pub name: &'static str,
    /// File extensions routed to this grammar, without the leading dot.
    pub extensions: &'static [&'static str],
    /// The tree-sitter grammar.
    pub language: Language,
    /// Node kinds that are directly named functions or methods. Each carries
    /// the function's identifier in a `name` field.
    pub function_kinds: &'static [&'static str],
    /// Wrapper node kinds that carry a callable in a field while the
    /// function's name sits on the wrapper itself.
    pub wrapped_functions: &'static [WrappedFunction],
}

impl LangConfig {
    /// Whether `path` should be parsed with this grammar.
    pub fn supports(&self, path: &str) -> bool {
        path.rsplit('.')
            .next()
            .is_some_and(|ext| self.extensions.contains(&ext.to_ascii_lowercase().as_str()))
    }
}

/// Registry of every language the structural engine understands.
pub fn registry() -> Vec<LangConfig> {
    vec![
        LangConfig {
            name: "rust",
            extensions: &["rs"],
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &["function_item"],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "python",
            extensions: &["py"],
            language: tree_sitter_python::LANGUAGE.into(),
            function_kinds: &["function_definition"],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "go",
            extensions: &["go"],
            language: tree_sitter_go::LANGUAGE.into(),
            function_kinds: &["function_declaration", "method_declaration"],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "javascript",
            extensions: &["js", "mjs", "cjs", "jsx"],
            language: tree_sitter_javascript::LANGUAGE.into(),
            function_kinds: &[
                "function_declaration",
                "generator_function_declaration",
                "method_definition",
            ],
            wrapped_functions: &[
                WrappedFunction {
                    node_kind: "variable_declarator",
                    name_field: "name",
                    body_field: "value",
                    body_kinds: CALLABLE_KINDS,
                },
                WrappedFunction {
                    node_kind: "assignment_expression",
                    name_field: "left",
                    body_field: "right",
                    body_kinds: CALLABLE_KINDS,
                },
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_routing() {
        let langs = registry();
        assert!(langs.iter().any(|l| l.supports("src/lib.rs")));
        assert!(langs.iter().any(|l| l.supports("app.py")));
        assert!(langs.iter().any(|l| l.supports("server.go")));
        assert!(langs.iter().any(|l| l.supports("index.js")));
        assert!(langs.iter().any(|l| l.supports("index.mjs")));
        assert!(langs.iter().any(|l| l.supports("index.cjs")));
        assert!(langs.iter().any(|l| l.supports("component.jsx")));

        let supported = |path: &str| langs.iter().any(|l| l.supports(path));
        assert!(!supported("README.md"));
        assert!(!supported("config.toml"));
        assert!(!supported("style.css"));
        assert!(!supported("run.sh"));
    }
}