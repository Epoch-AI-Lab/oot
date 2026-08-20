//! Per-language grammar configuration for the structural engine.
//!
//! The engine extracts named functions from a parse tree. Most languages
//! expose them as a single node kind carrying a `name` field. JavaScript
//! is the odd one out: a named arrow function (`const f = () => {}`) keeps
//! its callable on the `value` field of a `variable_declarator` and its name
//! on the declarator itself. That relationship is captured by
//! [`LangConfig::wrapped_functions`].

use tree_sitter::{Language, Node};

/// Callable node kinds that can sit on the right of a name.
const CALLABLE_KINDS: &[&str] = &[
    "arrow_function",
    "function_expression",
    "generator_function",
];

/// How to disambiguate a function's map key when bare names can collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    /// Key is the bare function name.
    None,
    /// Prefix with the receiver type, e.g. Go methods become `(*T).name`.
    Receiver,
}

/// A directly named function or method node kind.
#[derive(Debug, Clone, Copy)]
pub struct FunctionKind {
    /// The node kind, e.g. `"function_item"` or `"method_declaration"`.
    pub node_kind: &'static str,
    /// How to qualify the key when names could collide.
    pub qualifier: Qualifier,
}

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
    /// Node kinds the name field may hold. Anything else (member access,
    /// destructuring patterns) is not a named function.
    pub name_kinds: &'static [&'static str],
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
    pub function_kinds: &'static [FunctionKind],
    /// Wrapper node kinds that carry a callable in a field while the
    /// function's name sits on the wrapper itself.
    pub wrapped_functions: &'static [WrappedFunction],
}

impl LangConfig {
    /// Whether `path` should be parsed with this grammar.
    pub fn supports(&self, path: &str) -> bool {
        path.rsplit_once('.')
            .is_some_and(|(_, ext)| self.extensions.contains(&ext.to_ascii_lowercase().as_str()))
    }

    /// The map key for a directly named function node, qualified per
    /// `kind.qualifier` so same-named functions stay distinct.
    pub fn function_key(&self, kind: &FunctionKind, node: Node, source: &str) -> Option<String> {
        let name_node = node.child_by_field_name("name")?;
        let name = name_node.utf8_text(source.as_bytes()).ok()?.to_string();
        match kind.qualifier {
            Qualifier::None => Some(name),
            Qualifier::Receiver => {
                let receiver = node.child_by_field_name("receiver")?;
                let mut cursor = receiver.walk();
                let decl = receiver
                    .children(&mut cursor)
                    .find(|c| c.kind() == "parameter_declaration")?;
                let ty = decl.child_by_field_name("type")?;
                let ty_text = ty.utf8_text(source.as_bytes()).ok()?;
                Some(format!("({}).{}", ty_text, name))
            }
        }
    }
}

/// Registry of every language the structural engine understands.
pub fn registry() -> Vec<LangConfig> {
    vec![
        LangConfig {
            name: "rust",
            extensions: &["rs"],
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &[FunctionKind {
                node_kind: "function_item",
                qualifier: Qualifier::None,
            }],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "python",
            extensions: &["py"],
            language: tree_sitter_python::LANGUAGE.into(),
            function_kinds: &[FunctionKind {
                node_kind: "function_definition",
                qualifier: Qualifier::None,
            }],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "go",
            extensions: &["go"],
            language: tree_sitter_go::LANGUAGE.into(),
            function_kinds: &[
                FunctionKind {
                    node_kind: "function_declaration",
                    qualifier: Qualifier::None,
                },
                FunctionKind {
                    node_kind: "method_declaration",
                    qualifier: Qualifier::Receiver,
                },
            ],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "javascript",
            extensions: &["js", "mjs", "cjs", "jsx"],
            language: tree_sitter_javascript::LANGUAGE.into(),
            function_kinds: &[
                FunctionKind {
                    node_kind: "function_declaration",
                    qualifier: Qualifier::None,
                },
                FunctionKind {
                    node_kind: "generator_function_declaration",
                    qualifier: Qualifier::None,
                },
                FunctionKind {
                    node_kind: "method_definition",
                    qualifier: Qualifier::None,
                },
            ],
            wrapped_functions: &[
                WrappedFunction {
                    node_kind: "variable_declarator",
                    name_field: "name",
                    name_kinds: &["identifier"],
                    body_field: "value",
                    body_kinds: CALLABLE_KINDS,
                },
                WrappedFunction {
                    node_kind: "assignment_expression",
                    name_field: "left",
                    name_kinds: &["identifier"],
                    body_field: "right",
                    body_kinds: CALLABLE_KINDS,
                },
                WrappedFunction {
                    node_kind: "field_definition",
                    name_field: "property",
                    name_kinds: &["property_identifier", "private_property_identifier"],
                    body_field: "value",
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
