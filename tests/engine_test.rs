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
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/auth.rs".to_string(),
        r#"
fn authenticate(user: &str, pass: &str) -> bool {
    user == "admin" && pass == "secure_password_v2"
}
"#
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        "# Project\nUpdated README with more docs"
            .as_bytes()
            .to_vec(),
    );
    head.files.insert(
        "config.toml".to_string(),
        "title = 'New Config'".as_bytes().to_vec(),
    );
    head.files.insert(
        "scripts/run.sh".to_string(),
        "echo 'Running new script'".as_bytes().to_vec(),
    );
    head.files.insert(
        "style.css".to_string(),
        "body { color: blue; }".as_bytes().to_vec(),
    );

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
        "fn broken_syntax( { !!! %%% invalid rust code @@@ }}}"
            .as_bytes()
            .to_vec(),
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
        "fn verify_user(user: &str) -> bool { user.len() > 3 }"
            .as_bytes()
            .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/auth.rs".to_string(),
        "fn check_user(user: &str) -> bool { user.len() > 3 }"
            .as_bytes()
            .to_vec(),
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
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
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
        "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\n"
            .as_bytes()
            .to_vec(),
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
        "fn fa1() { println!(\"modified\"); }\nfn fa2() {}\nfn fa3() {}\n"
            .as_bytes()
            .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/handler.js".to_string(),
        r#"
const fetchUser = async (id) => {
  return { id, cached: false };
};
"#
        .as_bytes()
        .to_vec(),
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
    base.files.insert(
        "tools/go".to_string(),
        "func NotReally() {}".as_bytes().to_vec(),
    );

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
fn test_engine_duplicate_function_names_tracked_separately() {
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
        .as_bytes()
        .to_vec(),
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
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    // Regression: last-wins overwrite used to hide A's change entirely;
    // first-occurrence tracking then reduced it to an ambiguity note.
    assert_eq!(disputes.len(), 1, "got {:?}", disputes);
    assert_eq!(disputes[0].detail, "both sides changed `Name`");
}

#[test]
fn test_engine_3way_rename_rename_divergence_is_high_with_pinned_detail() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    let disputes = engine
        .diff_3way(
            &snap("pub fn f() -> i32 { 1 }\n"),
            &snap("pub fn g() -> i32 { 1 }\n"),
            &snap("pub fn k() -> i32 { 1 }\n"),
        )
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1, "got {:?}", disputes);
    assert_eq!(disputes[0].severity, Severity::High);
    assert_eq!(
        disputes[0].detail,
        "3-way conflict: both branches renamed function `f` differently (`f` -> `g` in target, `f` -> `k` in incoming)"
    );
    assert_eq!(
        disputes[0].location, "src/lib.rs:1",
        "located at theirs' new copy"
    );
}

#[test]
fn test_engine_3way_delete_delete_with_coincidental_adds_is_unchanged() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    // Both sides delete `f`; each happens to add an unrelated helper with a
    // different body. No rename pairing anywhere, so nothing may change:
    // no High fabricated, theirs' addition still reports as Low.
    let disputes = engine
        .diff_3way(
            &snap("pub fn f() -> i32 { 1 }\n"),
            &snap("pub fn p() -> i32 { 11 }\n"),
            &snap("pub fn q() -> i32 { 22 }\n"),
        )
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1, "got {:?}", disputes);
    assert_eq!(disputes[0].severity, Severity::Low);
    assert_eq!(disputes[0].detail, "incoming branch added function `q`");
}

#[test]
fn test_engine_3way_convergent_rename_is_clean() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    let disputes = engine
        .diff_3way(
            &snap("pub fn f() -> i32 { 1 }\n"),
            &snap("pub fn g() -> i32 { 1 }\n"),
            &snap("pub fn g() -> i32 { 1 }\n"),
        )
        .expect("Diff failed");

    assert!(disputes.is_empty(), "got {:?}", disputes);
}

#[test]
fn test_engine_3way_two_divergent_renames_emit_two_highs() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    let disputes = engine
        .diff_3way(
            &snap("pub fn f() -> i32 { 1 }\npub fn h() -> i32 { 2 }\n"),
            &snap("pub fn g() -> i32 { 1 }\npub fn h2() -> i32 { 2 }\n"),
            &snap("pub fn k() -> i32 { 1 }\npub fn h3() -> i32 { 2 }\n"),
        )
        .expect("Diff failed");

    assert_eq!(disputes.len(), 2, "got {:?}", disputes);
    assert!(disputes.iter().all(|d| d.severity == Severity::High));
    assert!(disputes.iter().any(|d| d.detail
        == "3-way conflict: both branches renamed function `f` differently (`f` -> `g` in target, `f` -> `k` in incoming)"));
    assert!(disputes.iter().any(|d| d.detail
        == "3-way conflict: both branches renamed function `h` differently (`h` -> `h2` in target, `h` -> `h3` in incoming)"));
}

#[test]
fn test_engine_3way_rename_vs_body_edit_reports_modify_delete_gap() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let snap = |s: &str| {
        let mut x = Snapshot::default();
        x.files
            .insert("src/lib.rs".to_string(), s.as_bytes().to_vec());
        x
    };

    // Known gap: ours renames f -> g while theirs edits f in place. The
    // exact-signature detector cannot pair the edited def, so this reports
    // modify/delete and stays silent about g until similarity scoring lands.
    let disputes = engine
        .diff_3way(
            &snap("pub fn f() -> i32 { 1 }\n"),
            &snap("pub fn g() -> i32 { 1 }\n"),
            &snap("pub fn f() -> i32 { 42 }\n"),
        )
        .expect("Diff failed");

    assert_eq!(disputes.len(), 1, "got {:?}", disputes);
    assert_eq!(disputes[0].severity, Severity::High);
    assert_eq!(
        disputes[0].detail,
        "3-way conflict: function `f` modified in incoming branch but deleted in target"
    );
}

#[test]
fn test_engine_typescript_types_interfaces_and_functions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/models.ts".to_string(),
        r#"
export type UserId = string;

export interface User {
    id: UserId;
    name: string;
}

export enum Role {
    Admin,
    Member,
}

export function getUser(id: UserId): User {
    return { id, name: "Alice" };
}
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/models.ts".to_string(),
        r#"
export type UserId = number;

export interface User {
    id: UserId;
    name: string;
    email: string;
}

export enum Role {
    Admin,
    Member,
    Guest,
}

export function getUser(id: UserId): User {
    return { id, name: "Alice", email: "alice@example.com" };
}
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 4, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `UserId`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `User`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `Role`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `getUser`"));
}

#[test]
fn test_engine_typescript_arrow_and_wrapped_functions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/handlers.ts".to_string(),
        r#"
export const calculateScore: (val: number) => number = (val) => {
    return val * 10;
};

export class Service {
    handle = async (req: Request): Promise<Response> => {
        return new Response("ok");
    };
}
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/handlers.ts".to_string(),
        r#"
export const calculateScore: (val: number) => number = (val) => {
    return val * 20;
};

export class Service {
    handle = async (req: Request): Promise<Response> => {
        return new Response("updated");
    };
}
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `calculateScore`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `handle`"));
}

#[test]
fn test_engine_tsx_components() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/App.tsx".to_string(),
        r#"
export function Button(props: { text: string }) {
    return <button>{props.text}</button>;
}
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/App.tsx".to_string(),
        r#"
export function Button(props: { text: string; primary?: boolean }) {
    return <button className={props.primary ? "btn-primary" : "btn"}>{props.text}</button>;
}

export const Modal = ({ isOpen }: { isOpen: boolean }) => {
    return isOpen ? <div>Modal</div> : null;
};
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `Button`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "added function `Modal`"));
}

#[test]
fn test_engine_python_decorators_and_async() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "app.py".to_string(),
        r#"
@app.route("/login")
def login():
    return "login page"

async def fetch_data():
    return await api.get()
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "app.py".to_string(),
        r#"
@app.route("/auth/login")
@require_ssl
def login():
    return "login page"

async def fetch_data():
    return await api.get_v2()
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `login`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `fetch_data`"));
}

#[test]
fn test_engine_typescript_object_literal_and_type_assertions() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "src/advanced.ts".to_string(),
        r#"
export const actions = {
    fetchUser: async (id: string) => {
        return { id, role: "user" };
    },
};

export const compute = ((x: number) => x * 2) as const;
export const validate = ((x: string) => x.length > 0) satisfies Validator;

export abstract class BaseController {
    abstract handle(): Promise<void>;
}
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "src/advanced.ts".to_string(),
        r#"
export const actions = {
    fetchUser: async (id: string) => {
        return { id, role: "admin" };
    },
};

export const compute = ((x: number) => x * 4) as const;
export const validate = ((x: string) => x.length > 5) satisfies Validator;

export abstract class BaseController {
    abstract handle(): Promise<Response>;
}
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 4, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `fetchUser`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `compute`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `validate`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `handle`"));
}

#[test]
fn test_engine_python_decorated_class_methods() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "models.py".to_string(),
        r#"
@dataclass
class UserModel:
    id: int
    name: str

    def full_name(self) -> str:
        return self.name

    @classmethod
    def create(cls, name: str):
        return cls(id=1, name=name)
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "models.py".to_string(),
        r#"
@dataclass
class UserModel:
    id: int
    name: str

    def full_name(self) -> str:
        return self.name.strip()

    @classmethod
    def create(cls, name: str):
        return cls(id=2, name=name)
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");

    assert_eq!(disputes.len(), 2, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `full_name`"));
    assert!(disputes
        .iter()
        .any(|d| d.detail == "both sides changed `create`"));
}

#[test]
fn test_engine_go_interface_methods() {
    let engine = Engine::new().expect("Failed to initialize engine");

    let mut base = Snapshot::default();
    base.files.insert(
        "service.go".to_string(),
        r#"
package service

type UserService interface {
    GetUser(id int) (*User, error)
}
"#
        .as_bytes()
        .to_vec(),
    );

    let mut head = Snapshot::default();
    head.files.insert(
        "service.go".to_string(),
        r#"
package service

type UserService interface {
    GetUser(id int) (*User, error)
    DeleteUser(id int) error
}
"#
        .as_bytes()
        .to_vec(),
    );

    let disputes = engine.diff_snapshots(&base, &head).expect("Diff failed");
    assert_eq!(disputes.len(), 1, "got {:?}", disputes);
    assert!(disputes
        .iter()
        .any(|d| d.detail == "added function `DeleteUser`"));
}
