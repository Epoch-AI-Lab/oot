use oot::change::Snapshot;
use oot::dispute::{Kind, Severity};
use oot::engine::Engine;

#[test]
fn test_engine_function_modification_detection() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
fn test_engine_unsupported_extension_filtering() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
        "Files with unsupported extensions should be filtered out from AST diffing"
    );
}

#[test]
fn test_engine_syntax_error_handling() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
    let mut engine = Engine::new().expect("Failed to initialize engine");

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
fn test_engine_go_function_and_method_detection() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "store/store.go".to_string(),
        r#"
package store

func Greet(name string) string {
	return "hello " + name
}

func (s *Store) Name() string {
	return s.title
}
"#
        .to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "store/store.go".to_string(),
        r#"
package store

func Greet(name string) string {
	return "hey " + name
}

func (s *Store) Name() string {
	return s.title
}
"#
        .to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(disputes[0].detail, "both sides changed `Greet`");
}

#[test]
fn test_engine_go_3way_method_conflict() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

    let base_src = r#"
package store

func (s *Store) Total() int {
	return s.count * s.price
}
"#;
    let ours_src = r#"
package store

func (s *Store) Total() int {
	return s.count * s.price / s.divisor
}
"#;
    let theirs_src = r#"
package store

func (s *Store) Total() int {
	return s.count * s.price + s.bonus
}
"#;

    let mut base = Snapshot::default();
    base.files
        .insert("total.go".to_string(), base_src.to_string());
    let mut ours = Snapshot::default();
    ours.files
        .insert("total.go".to_string(), ours_src.to_string());
    let mut theirs = Snapshot::default();
    theirs
        .files
        .insert("total.go".to_string(), theirs_src.to_string());

    let disputes = engine
        .diff_3way(&base, &ours, &theirs)
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(disputes[0].severity, Severity::High);
    assert!(disputes[0].detail.contains("3-way conflict"));
    assert!(disputes[0].detail.contains("`Total`"));
}

#[test]
fn test_engine_javascript_function_detection() {
    let mut engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/api.js".to_string(),
        r#"
export function fetchUser(id) {
  return { id };
}

class Client {
  connect() {
    return true;
  }
}
"#
        .to_string(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/api.js".to_string(),
        r#"
export function fetchUser(id) {
  return { id, includeProfile: true };
}

class Client {
  connect(timeoutMs) {
    return timeoutMs > 0;
  }
}
"#
        .to_string(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `fetchUser`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `connect`"));
}
