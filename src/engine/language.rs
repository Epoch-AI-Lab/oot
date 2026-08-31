//! Per-language grammar configuration for the structural engine.
//!
//! The engine extracts named functions from a parse tree. Most languages
//! expose them as a single node kind carrying a `name` field. JavaScript
//! is the odd one out: a named arrow function (`const f = () => {}`) keeps
//! its callable on the `value` field of a `variable_declarator` and its name
//! on the declarator itself. That relationship is captured by
//! [`LangConfig::wrapped_functions`].
//!
//! Function map keys are always bare names, but every same-key definition
//! is kept and diffing aligns each group by content (exact source match
//! first, then positional pairing of leftovers — see `align_defs`).
//! Receiver- or impl-qualified keys (`(*T).name`, `(A).name`) were tried and
//! rejected: they make keys unstable under refactoring, so moving a function
//! between impl blocks fabricates High-severity 3-way conflicts (and false
//! Blocked verdicts). Content-first alignment keeps identity stable under
//! those moves while still tracking each definition separately.

use tree_sitter::{Language, Node};

/// Callable node kinds that can sit on the right of a name.
const CALLABLE_KINDS: &[&str] = &[
    "arrow_function",
    "function_expression",
    "generator_function",
];

/// A directly named function or method node kind.
#[derive(Debug, Clone, Copy)]
pub struct FunctionKind {
    /// The node kind, e.g. `"function_item"` or `"method_declaration"`.
    pub node_kind: &'static str,
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

    /// The map key for a directly named function node: its `name` field text.
    pub fn function_key(&self, _kind: &FunctionKind, node: Node, source: &str) -> Option<String> {
        let name_node = self.name_node(node)?;
        name_node
            .utf8_text(source.as_bytes())
            .ok()
            .map(str::to_string)
    }

    /// Extract the identifier node carrying the definition's name.
    pub fn name_node<'a>(&self, node: Node<'a>) -> Option<Node<'a>> {
        if node.kind() == "decorated_definition" {
            if let Some(def) = node.child_by_field_name("definition") {
                // If it decorates a class, do not treat the whole class as a function.
                if def.kind() == "class_definition" {
                    return None;
                }
                if let Some(name) = def.child_by_field_name("name") {
                    return Some(name);
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "class_definition" {
                    return None;
                }
                if child.kind() == "function_definition" || child.kind() == "method_definition" {
                    if let Some(name) = child.child_by_field_name("name") {
                        return Some(name);
                    }
                }
            }
            None
        } else {
            node.child_by_field_name("name")
        }
    }
}

/// Recursively unwrap expressions (`as const`, `satisfies`, type assertions, parentheses)
/// down to the underlying callable node.
pub fn unwrap_callable<'a>(mut node: Node<'a>) -> Node<'a> {
    while node.kind() == "as_expression"
        || node.kind() == "satisfies_expression"
        || node.kind() == "type_assertion"
        || node.kind() == "parenthesized_expression"
        || node.kind() == "non_null_expression"
        || node.kind() == "call_expression"
    {
        if node.kind() == "call_expression" {
            if let Some(args) = node.child_by_field_name("arguments") {
                let mut found = None;
                let mut cursor = args.walk();
                for child in args.children(&mut cursor) {
                    if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                        found = Some(child);
                        break;
                    }
                }
                if let Some(c) = found {
                    node = c;
                    continue;
                }
            }
            break;
        }
        if let Some(child) = node.child_by_field_name("expression") {
            node = child;
        } else if let Some(child) = node.child_by_field_name("value") {
            node = child;
        } else if let Some(child) = node.named_child(0) {
            node = child;
        } else {
            break;
        }
    }
    node
}

const TS_FUNCTION_KINDS: &[FunctionKind] = &[
    FunctionKind {
        node_kind: "function_declaration",
    },
    FunctionKind {
        node_kind: "generator_function_declaration",
    },
    FunctionKind {
        node_kind: "method_definition",
    },
    FunctionKind {
        node_kind: "method_signature",
    },
    FunctionKind {
        node_kind: "abstract_method_signature",
    },
    FunctionKind {
        node_kind: "function_signature",
    },
    FunctionKind {
        node_kind: "type_alias_declaration",
    },
    FunctionKind {
        node_kind: "interface_declaration",
    },
    FunctionKind {
        node_kind: "enum_declaration",
    },
];

const JS_TS_WRAPPED_FUNCTIONS: &[WrappedFunction] = &[
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
        node_kind: "pair",
        name_field: "key",
        name_kinds: &["property_identifier", "identifier", "string"],
        body_field: "value",
        body_kinds: CALLABLE_KINDS,
    },
    WrappedFunction {
        node_kind: "field_definition",
        name_field: "property",
        name_kinds: &["property_identifier", "private_property_identifier"],
        body_field: "value",
        body_kinds: CALLABLE_KINDS,
    },
    WrappedFunction {
        node_kind: "field_definition",
        name_field: "name",
        name_kinds: &[
            "property_identifier",
            "private_property_identifier",
            "identifier",
        ],
        body_field: "value",
        body_kinds: CALLABLE_KINDS,
    },
    WrappedFunction {
        node_kind: "public_field_definition",
        name_field: "name",
        name_kinds: &[
            "property_identifier",
            "private_property_identifier",
            "identifier",
        ],
        body_field: "value",
        body_kinds: CALLABLE_KINDS,
    },
    WrappedFunction {
        node_kind: "property_definition",
        name_field: "name",
        name_kinds: &[
            "property_identifier",
            "private_property_identifier",
            "identifier",
        ],
        body_field: "value",
        body_kinds: CALLABLE_KINDS,
    },
];

/// Registry of every language the structural engine understands.
pub fn registry() -> Vec<LangConfig> {
    vec![
        LangConfig {
            name: "rust",
            extensions: &["rs"],
            language: tree_sitter_rust::LANGUAGE.into(),
            function_kinds: &[FunctionKind {
                node_kind: "function_item",
            }],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "python",
            extensions: &["py", "pyi"],
            language: tree_sitter_python::LANGUAGE.into(),
            function_kinds: &[
                FunctionKind {
                    node_kind: "function_definition",
                },
                FunctionKind {
                    node_kind: "decorated_definition",
                },
            ],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "go",
            extensions: &["go"],
            language: tree_sitter_go::LANGUAGE.into(),
            function_kinds: &[
                FunctionKind {
                    node_kind: "function_declaration",
                },
                FunctionKind {
                    node_kind: "method_declaration",
                },
                FunctionKind {
                    node_kind: "method_elem",
                },
                FunctionKind {
                    node_kind: "method_spec",
                },
            ],
            wrapped_functions: &[],
        },
        LangConfig {
            name: "javascript",
            extensions: &["js", "mjs", "cjs"],
            language: tree_sitter_javascript::LANGUAGE.into(),
            function_kinds: &[
                FunctionKind {
                    node_kind: "function_declaration",
                },
                FunctionKind {
                    node_kind: "generator_function_declaration",
                },
                FunctionKind {
                    node_kind: "method_definition",
                },
            ],
            wrapped_functions: JS_TS_WRAPPED_FUNCTIONS,
        },
        LangConfig {
            name: "typescript",
            extensions: &["ts", "mts", "cts"],
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            function_kinds: TS_FUNCTION_KINDS,
            wrapped_functions: JS_TS_WRAPPED_FUNCTIONS,
        },
        LangConfig {
            name: "tsx",
            extensions: &["tsx", "jsx"],
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            function_kinds: TS_FUNCTION_KINDS,
            wrapped_functions: JS_TS_WRAPPED_FUNCTIONS,
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
        assert!(langs.iter().any(|l| l.supports("types.pyi")));
        assert!(langs.iter().any(|l| l.supports("server.go")));
        assert!(langs.iter().any(|l| l.supports("index.js")));
        assert!(langs.iter().any(|l| l.supports("index.mjs")));
        assert!(langs.iter().any(|l| l.supports("index.cjs")));
        assert!(langs.iter().any(|l| l.supports("index.ts")));
        assert!(langs.iter().any(|l| l.supports("index.mts")));
        assert!(langs.iter().any(|l| l.supports("index.cts")));
        assert!(langs.iter().any(|l| l.supports("component.tsx")));
        assert!(langs.iter().any(|l| l.supports("component.jsx")));

        let supported = |path: &str| langs.iter().any(|l| l.supports(path));
        assert!(!supported("README.md"));
        assert!(!supported("config.toml"));
        assert!(!supported("style.css"));
        assert!(!supported("run.sh"));
    }
}
