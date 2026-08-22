use oot::change::Snapshot;
use oot::dispute::{Kind, Severity};
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
        .as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/auth.rs".to_string(),
        r#"
fn authenticate(user: &str, pass: &str) -> bool {
    user == "admin" && pass == "secure_password_v2"
}
"#
        .as_bytes().to_vec(),
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
        .as_bytes().to_vec(),
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
        .as_bytes().to_vec(),
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
        .insert("src/crypto.rs".to_string(), source.as_bytes().to_vec());

    let mut head = Snapshot::default();
    head.files
        .insert("src/crypto.rs".to_string(), source.as_bytes().to_vec());

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert!(
        disputes.is_empty(),
        "Expected zero disputes for identical files, got {:?}",
        disputes
    );
}

#[test]
fn test_engine_unsupported_extension_filtering() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "README.md".to_string(),
        "# Project\nInitial README".as_bytes().to_vec(),
    );
    base.files.insert(
        "config.toml".to_string(),
        "title = 'Old Config'".as_bytes().to_vec(),
    );
    base.files.insert(
        "scripts/run.sh".to_string(),
        "echo 'Running old script'".as_bytes().to_vec(),
    );
    base.files.insert(
        "style.css".to_string(),
        "body { color: black; }".as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "README.md".to_string(),
        "# Project\nUpdated README with more docs".as_bytes().to_vec(),
    );
    head.files.insert(
        "config.toml".to_string(),
        "title = 'New Config'".as_bytes().to_vec(),
    );
    head.files.insert(
        "scripts/run.sh".to_string(),
        "echo 'Running new script'".as_bytes().to_vec(),
    );
    head.files
        .insert("style.css".to_string(), "body { color: blue; }".as_bytes().to_vec());

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert!(
        disputes.is_empty(),
        "Files with unsupported extensions should be filtered out from AST diffing"
    );
}

#[test]
fn test_engine_syntax_error_handling() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/broken.rs".to_string(),
        "fn valid_base() -> i32 { 42 }".as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    // Incomplete / invalid Rust syntax
    head.files.insert(
        "src/broken.rs".to_string(),
        "fn broken_syntax( { !!! %%% invalid rust code @@@ }}}".as_bytes().to_vec(),
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
        "fn old_util() {}".as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/new_module.rs".to_string(),
        "fn new_util() {}".as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2);
    let added = disputes
        .iter()
        .find(|d| d.detail == "file added (1 function: new_util)")
        .expect("File added dispute with content summary");
    assert_eq!(added.location, "src/new_module.rs:0");

    let removed = disputes
        .iter()
        .find(|d| d.detail == "file removed")
        .expect("File removed dispute");
    assert_eq!(removed.location, "src/old_module.rs:0");
}

#[test]
fn test_engine_rename_is_not_remove_add() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/auth.rs".to_string(),
        "fn verify_user(user: &str) -> bool { user.len() > 3 }".as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/auth.rs".to_string(),
        "fn check_user(user: &str) -> bool { user.len() > 3 }".as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(
        disputes.len(),
        1,
        "identical body under a new name is one rename, got {:?}",
        disputes
    );
    assert_eq!(
        disputes[0].detail,
        "renamed function `verify_user` to `check_user`"
    );
}

#[test]
fn test_engine_3way_rename_is_not_conflict() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let base_src = "fn handle(req: i32) -> i32 { req + 1 }";
    let ours_src = base_src; // target untouched
    let theirs_src = "fn process(req: i32) -> i32 { req + 1 }"; // incoming renamed it

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files.insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    let disputes = engine
        .diff_3way(&snap(base_src), &snap(ours_src), &snap(theirs_src))
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(
        disputes[0].detail,
        "incoming branch renamed function `handle` to `process`"
    );
}

#[test]
fn test_engine_added_file_summary_lists_functions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let base = Snapshot::default();
    let mut head = Snapshot::default();
    head.files.insert(
        "src/newstuff.rs".to_string(),
        "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\n".as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(
        disputes[0].detail,
        "file added (4 functions: alpha, beta, delta, …)"
    );
}

#[test]
fn test_engine_multiple_files_and_functions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/a.rs".to_string(),
        "fn fa1() {}\nfn fa2() {}\n".as_bytes().to_vec(),
    );
    base.files
        .insert("src/b.rs".to_string(), "fn fb1() {}\n".as_bytes().to_vec());

    let mut head = Snapshot::default();
    head.files.insert(
        "src/a.rs".to_string(),
        "fn fa1() { println!(\"modified\"); }\nfn fa2() {}\nfn fa3() {}\n".as_bytes().to_vec(),
    );
    head.files
        .insert("src/b.rs".to_string(), "fn fb1() {}\n".as_bytes().to_vec());

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
    let engine = Engine::new().expect("Failed to initialize engine");

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
        .as_bytes().to_vec(),
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
        .as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(disputes[0].detail, "both sides changed `Greet`");
}

#[test]
fn test_engine_go_3way_method_conflict() {
    let engine = Engine::new().expect("Failed to initialize engine");

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
        .insert("total.go".to_string(), base_src.as_bytes().to_vec());
    let mut ours = Snapshot::default();
    ours.files
        .insert("total.go".to_string(), ours_src.as_bytes().to_vec());
    let mut theirs = Snapshot::default();
    theirs
        .files
        .insert("total.go".to_string(), theirs_src.as_bytes().to_vec());

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
    let engine = Engine::new().expect("Failed to initialize engine");

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
        .as_bytes().to_vec(),
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
        .as_bytes().to_vec(),
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

#[test]
fn test_engine_javascript_const_arrow_detection() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/handler.js".to_string(),
        r#"
const fetchUser = async (id) => {
  return { id };
};
"#
        .as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/handler.js".to_string(),
        r#"
const fetchUser = async (id) => {
  return { id, cached: false };
};
"#
        .as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(
        disputes[0].detail, "both sides changed `fetchUser`",
        "arrow function bound to a const must be tracked under the binding name"
    );
}

#[test]
fn test_engine_javascript_3way_conflict() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let base_src = r#"
export const formatPrice = (cents) => {
  return `$${cents / 100}`;
};
"#;
    let ours_src = r#"
export const formatPrice = (cents) => {
  return (cents / 100).toFixed(2);
};
"#;
    let theirs_src = r#"
export const formatPrice = (cents) => {
  return `${cents / 100} EUR`;
};
"#;

    let mut base = Snapshot::default();
    base.files
        .insert("src/price.mjs".to_string(), base_src.as_bytes().to_vec());
    let mut ours = Snapshot::default();
    ours.files
        .insert("src/price.mjs".to_string(), ours_src.as_bytes().to_vec());
    let mut theirs = Snapshot::default();
    theirs
        .files
        .insert("src/price.mjs".to_string(), theirs_src.as_bytes().to_vec());

    // Covers both the 3-way path for JavaScript and .mjs extension routing.
    let disputes = engine
        .diff_3way(&base, &ours, &theirs)
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1);
    assert_eq!(disputes[0].severity, Severity::High);
    assert!(disputes[0].detail.contains("3-way conflict"));
    assert!(disputes[0].detail.contains("`formatPrice`"));
}

#[test]
fn test_engine_dotless_filename_is_not_source() {
    let engine = Engine::new().expect("Failed to initialize engine");

    // A file literally named `go` with no extension must not be parsed as Go.
    let mut base = Snapshot::default();
    base.files
        .insert("tools/go".to_string(), "func NotReally() {}".as_bytes().to_vec());

    let mut head = Snapshot::default();
    head.files.insert(
        "tools/go".to_string(),
        "func DefinitelyChanged() {}".as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert!(
        disputes.is_empty(),
        "extension-less files must be skipped, got {:?}",
        disputes
    );
}

#[test]
fn test_engine_duplicate_function_names_flagged() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "types.go".to_string(),
        r#"
package types

func (a A) Name() string {
	return "A"
}

func (b B) Name() string {
	return "B"
}
"#
        .as_bytes().to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "types.go".to_string(),
        r#"
package types

func (a A) Name() string {
	return "A-changed"
}

func (b B) Name() string {
	return "B"
}
"#
        .as_bytes().to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    // Regression: last-wins overwrite used to hide A's change entirely.
    assert!(
        disputes
            .iter()
            .any(|d| d.detail.contains("`Name`") && d.detail.contains("multiple times")),
        "duplicate name must surface an ambiguity dispute, got {:?}",
        disputes
    );
}
