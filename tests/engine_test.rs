use oot::change::Snapshot;
use oot::dispute::{Dispute, Kind, Severity};
use oot::engine::Engine;

#[test]
fn test_engine_function_modification_detection() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/auth.rs".to_string(),
        r#"
fn authenticate(user: &str, pass: &str) -> bool {
    user == "admin" && pass == "secret"
}
"#
        .to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/auth.rs".to_string(),
        r#"
fn authenticate(user: &str, pass: &str) -> bool {
    user == "admin" && pass == "secure_password_v2"
}
"#
        .to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    let d = &disputes[0];
    assert_eq!(d.kind, Kind::Meaning);
    assert_eq!(d.severity, Severity::Review);
    assert_eq!(d.id, "D001");
    assert!(d.location.starts_with("src/auth.rs:"));
    assert_eq!(d.detail, "both sides changed `authenticate`");
}

#[test]
fn test_engine_function_addition_and_deletion_detection() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/math.rs".to_string(),
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn legacy_multiply(a: i32, b: i32) -> i32 {
    a * b
}
"#
        .to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/math.rs".to_string(),
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn subtract(a: i32, b: i32) -> i32 {
    a - b
}
"#
        .to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2);
    let added = disputes
        .iter()
        .find(|d| d.detail == "added function `subtract`")
        .expect("Should have detected added function");
    assert_eq!(added.kind, Kind::Meaning);
    assert_eq!(added.severity, Severity::Review);

    let removed = disputes
        .iter()
        .find(|d| d.detail == "removed function `legacy_multiply`")
        .expect("Should have detected removed function");
    assert_eq!(removed.kind, Kind::Meaning);
    assert_eq!(removed.severity, Severity::Review);
}

#[test]
fn test_engine_identical_unchanged_files() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let source = r#"
pub fn calculate_hash(data: &[u8]) -> u64 {
    let mut hash = 0u64;
    for b in data {
        hash = hash.wrapping_add(*b as u64);
    }
    hash
}

pub fn verify_signature() -> bool {
    true
}
"#;

    let mut base = Snapshot::default();
    base.files
        .insert("src/crypto.rs".to_string(), source.to_string());

    let mut head = Snapshot::default();
    head.files
        .insert("src/crypto.rs".to_string(), source.to_string());

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert!(
        disputes.is_empty(),
        "Expected zero disputes for identical files, got {:?}",
        disputes
    );
}

#[test]
fn test_engine_non_rust_file_filtering() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "README.md".to_string(),
        "# Project\nInitial README".to_string(),
    );
    base.files.insert(
        "config.toml".to_string(),
        "title = 'Old Config'".to_string(),
    );
    base.files.insert(
        "scripts/run.sh".to_string(),
        "echo 'Running old script'".to_string(),
    );
    base.files.insert(
        "style.css".to_string(),
        "body { color: black; }".to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "README.md".to_string(),
        "# Project\nUpdated README with more docs".to_string(),
    );
    head.files.insert(
        "config.toml".to_string(),
        "title = 'New Config'".to_string(),
    );
    head.files.insert(
        "scripts/run.sh".to_string(),
        "echo 'Running new script'".to_string(),
    );
    head.files
        .insert("style.css".to_string(), "body { color: blue; }".to_string());

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert!(
        disputes.is_empty(),
        "Non-Rust files should be filtered out from AST diffing"
    );
}

#[test]
fn test_engine_syntax_error_handling() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/broken.rs".to_string(),
        "fn valid_base() -> i32 { 42 }".to_string(),
    );

    let mut head = Snapshot::default();
    // Incomplete / invalid Rust syntax
    head.files.insert(
        "src/broken.rs".to_string(),
        "fn broken_syntax( { !!! %%% invalid rust code @@@ }}}".to_string(),
    );

    // Engine should handle syntax errors gracefully without panicking
    let result = engine.diff_snapshots(&base, &head);
    assert!(result.is_ok());
    let disputes = result.unwrap();
    // Since tree-sitter recovers and base function is missing in head, it flags removed function
    assert!(disputes
        .iter()
        .any(|d| d.detail.contains("removed function `valid_base`")));
}

#[test]
fn test_engine_file_added_and_removed() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/old_module.rs".to_string(),
        "fn old_util() {}".to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/new_module.rs".to_string(),
        "fn new_util() {}".to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2);
    let added = disputes
        .iter()
        .find(|d| d.detail == "file added")
        .expect("File added dispute");
    assert_eq!(added.location, "src/new_module.rs:0");

    let removed = disputes
        .iter()
        .find(|d| d.detail == "file removed")
        .expect("File removed dispute");
    assert_eq!(removed.location, "src/old_module.rs:0");
}

#[test]
fn test_engine_multiple_files_and_functions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/a.rs".to_string(),
        "fn fa1() {}\nfn fa2() {}\n".to_string(),
    );
    base.files
        .insert("src/b.rs".to_string(), "fn fb1() {}\n".to_string());

    let mut head = Snapshot::default();
    head.files.insert(
        "src/a.rs".to_string(),
        "fn fa1() { println!(\"modified\"); }\nfn fa2() {}\nfn fa3() {}\n".to_string(),
    );
    head.files
        .insert("src/b.rs".to_string(), "fn fb1() {}\n".to_string());

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    // fa1 modified, fa3 added in src/a.rs. src/b.rs is unchanged.
    assert_eq!(disputes.len(), 2);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `fa1`"));
    assert!(disputes.iter().any(|d| d.detail == "added function `fa3`"));
}

#[test]
fn test_engine_mixed_language_snapshot() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "app.py".to_string(),
        "def greet(name):\n    return f\"hi {name}\"\n".to_string(),
    );
    base.files.insert(
        "index.js".to_string(),
        "const double = (x) => x * 2;\n".to_string(),
    );
    base.files.insert(
        "server.go".to_string(),
        "package main\n\nfunc greet(name string) string {\n\treturn \"hi \" + name\n}\n".to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "app.py".to_string(),
        "def greet(name):\n    return f\"hello {name}\"\n".to_string(),
    );
    head.files.insert(
        "index.js".to_string(),
        "const double = (x) => x * 3;\n".to_string(),
    );
    head.files.insert(
        "server.go".to_string(),
        "package main\n\nfunc greet(name string) string {\n\treturn \"hello \" + name\n}\n".to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 3);

    let greets: Vec<&Dispute> = disputes
        .iter()
        .filter(|d| d.detail == "both sides changed `greet`")
        .collect();
    assert_eq!(greets.len(), 2, "one greet change per language file");
    assert!(
        greets.iter().any(|d| d.location.starts_with("app.py:")),
        "python greet dispute should point into app.py"
    );
    assert!(
        greets.iter().any(|d| d.location.starts_with("server.go:")),
        "go greet dispute should point into server.go"
    );

    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `double`"));
}
