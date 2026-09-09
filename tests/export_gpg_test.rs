//! Tests GPG signature preservation during visibility-filtered exports.
//!
//! Commits that do not touch private paths and whose parents are untouched
//! retain their original signed commit objects and GPG signatures verbatim.
//! Commits that touch private paths are rewritten and exported unsigned.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_oot")
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Kriday")
        .env("GIT_AUTHOR_EMAIL", "k@oot.dev")
        .env("GIT_COMMITTER_NAME", "Kriday")
        .env("GIT_COMMITTER_EMAIL", "k@oot.dev")
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn oot(args: &[&str], cwd: &Path) -> (bool, String) {
    let o = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("oot binary should run");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
fn test_filtered_export_preserves_gpg_signatures_on_clean_commits() {
    let tmp = std::env::temp_dir().join(format!("oot-gpg-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);

    // 1. Create a signed root commit on public file README.md
    std::fs::write(src.join("README.md"), "# Public Project\n").unwrap();
    git(&src, &["add", "."]);
    let root_tree = git(&src, &["write-tree"]);

    let raw_root = format!(
        "tree {root_tree}\n\
         author Kriday <k@oot.dev> 1700000000 +0530\n\
         committer Kriday <k@oot.dev> 1700000000 +0530\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \
         iQEcBAABCgAGBQJlrootAAoJEDummyRoot\n \
         =root\n \
         -----END PGP SIGNATURE-----\n\
         \n\
         signed root commit\n"
    );
    let raw_path = src.join("root.commit");
    std::fs::write(&raw_path, &raw_root).unwrap();
    let forged_root = Command::new("git")
        .arg("-C")
        .arg(&src)
        .args(["hash-object", "-t", "commit", "-w", "root.commit"])
        .output()
        .unwrap();
    assert!(forged_root.status.success());
    let signed_root_sha = String::from_utf8_lossy(&forged_root.stdout)
        .trim()
        .to_string();
    std::fs::remove_file(&raw_path).unwrap();
    git(&src, &["update-ref", "refs/heads/main", &signed_root_sha]);

    // 2. Create a signed second commit on src/lib.rs (clean, public)
    std::fs::create_dir_all(src.join("src")).unwrap();
    std::fs::write(
        src.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();
    git(&src, &["add", "."]);
    let second_tree = git(&src, &["write-tree"]);

    let raw_second = format!(
        "tree {second_tree}\n\
         parent {signed_root_sha}\n\
         author Kriday <k@oot.dev> 1700000100 +0530\n\
         committer Kriday <k@oot.dev> 1700000100 +0530\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \
         iQEcBAABCgAGBQJlsecondAAoJEDummySecond\n \
         =second\n \
         -----END PGP SIGNATURE-----\n\
         \n\
         signed second commit\n"
    );
    let raw_path = src.join("second.commit");
    std::fs::write(&raw_path, &raw_second).unwrap();
    let forged_second = Command::new("git")
        .arg("-C")
        .arg(&src)
        .args(["hash-object", "-t", "commit", "-w", "second.commit"])
        .output()
        .unwrap();
    assert!(forged_second.status.success());
    let signed_second_sha = String::from_utf8_lossy(&forged_second.stdout)
        .trim()
        .to_string();
    std::fs::remove_file(&raw_path).unwrap();
    git(&src, &["update-ref", "refs/heads/main", &signed_second_sha]);

    // 3. Create a third commit that introduces secrets/.env (private path)
    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/.env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // 4. Create a fourth commit that updates src/lib.rs (downstream of secret)
    std::fs::write(
        src.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b + 1 }\n",
    )
    .unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "update lib"]);

    // Configure visibility policy to filter secrets/
    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    // Verify 1: The root commit was preserved byte-exact with its GPG signature
    let root_cat = git(&out, &["cat-file", "commit", &signed_root_sha]);
    assert!(
        root_cat.contains("BEGIN PGP SIGNATURE"),
        "root GPG signature lost"
    );
    assert!(root_cat.contains("DummyRoot"), "root GPG payload altered");

    // Verify 2: The second commit was preserved byte-exact with its GPG signature
    let second_cat = git(&out, &["cat-file", "commit", &signed_second_sha]);
    assert!(
        second_cat.contains("BEGIN PGP SIGNATURE"),
        "second commit GPG signature lost"
    );
    assert!(
        second_cat.contains("DummySecond"),
        "second commit GPG payload altered"
    );

    // Verify 3: Secrets are stripped from the exported history
    let exported_head = git(&out, &["rev-parse", "main"]);
    assert_ne!(exported_head, signed_second_sha);
    let log = git(&out, &["log", "--oneline", "main"]);
    assert!(
        !log.contains("add secret"),
        "secret commit should be withheld"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

fn forge_signed(
    repo: &Path,
    tree: &str,
    parent: Option<&str>,
    stamp: &str,
    message: &str,
) -> String {
    let mut raw = format!("tree {tree}\n");
    if let Some(p) = parent {
        raw.push_str(&format!("parent {p}\n"));
    }
    raw.push_str(&format!(
        "author Kriday <k@oot.dev> 1700000200 +0530\n\
         committer Kriday <k@oot.dev> 1700000200 +0530\n\
         gpgsig -----BEGIN PGP SIGNATURE-----\n \
         {stamp}\n \
         =sig\n \
         -----END PGP SIGNATURE-----\n\
         \n\
         {message}\n"
    ));
    let raw_path = repo.join("forged.commit");
    std::fs::write(&raw_path, &raw).unwrap();
    let forged = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "-t", "commit", "-w", "forged.commit"])
        .output()
        .unwrap();
    assert!(forged.status.success());
    std::fs::remove_file(&raw_path).unwrap();
    String::from_utf8_lossy(&forged.stdout).trim().to_string()
}

#[test]
fn test_rebuilt_commit_drops_sig_and_logs_it() {
    let tmp = std::env::temp_dir().join(format!("oot-gpg-sigdrop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);

    // Signed root and second commits on public files.
    std::fs::write(src.join("README.md"), "# Public Project\n").unwrap();
    git(&src, &["add", "."]);
    let root_tree = git(&src, &["write-tree"]);
    let signed_root = forge_signed(&src, &root_tree, None, "iQDummyRoot", "signed root");
    git(&src, &["update-ref", "refs/heads/main", &signed_root]);

    std::fs::create_dir_all(src.join("src")).unwrap();
    std::fs::write(src.join("src/lib.rs"), "pub fn add() -> i32 { 1 }\n").unwrap();
    git(&src, &["add", "."]);
    let second_tree = git(&src, &["write-tree"]);
    let signed_second = forge_signed(
        &src,
        &second_tree,
        Some(&signed_root),
        "iQDummySecond",
        "signed second",
    );
    git(&src, &["update-ref", "refs/heads/main", &signed_second]);

    // Third commit introduces the secret and is withheld whole.
    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/.env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // Fourth commit is signed but lands past the secret, so export rebuilds it.
    std::fs::write(src.join("src/lib.rs"), "pub fn add() -> i32 { 2 }\n").unwrap();
    git(&src, &["add", "."]);
    let fourth_tree = git(&src, &["write-tree"]);
    let third_sha = git(&src, &["rev-parse", "HEAD"]);
    let signed_fourth = forge_signed(
        &src,
        &fourth_tree,
        Some(&third_sha),
        "iQDummyFourth",
        "signed fourth",
    );
    git(&src, &["update-ref", "refs/heads/main", &signed_fourth]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "export failed: {msg}");

    // The rebuilt head must not carry the old sig: it covers old bytes.
    let exported_head = git(&out, &["rev-parse", "main"]);
    assert_ne!(exported_head, signed_fourth);
    let head_cat = git(&out, &["cat-file", "commit", &exported_head]);
    assert!(
        !head_cat.contains("gpgsig"),
        "rebuilt commit must ship unsigned"
    );

    // The audit log names the dropped sig and the original sha.
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(log.contains("sig-dropped"), "missing drop event: {log}");
    assert!(
        log.contains(&signed_fourth),
        "drop event must name the original sha: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_resign_key_leaves_unsigned_rebuilds_alone() {
    let tmp = std::env::temp_dir().join(format!("oot-gpg-nosign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "# Public Project\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "root"]);

    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/.env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // Unsigned commit past the secret: rebuild must stay unsigned even
    // with a key set. Keys replace old sigs, they never add new ones.
    std::fs::write(src.join("README.md"), "# Public Project v2\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "update readme"]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\nresign_key_id = \"no-such-key-oot-test\"\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "unsigned rebuilds must not touch the key: {msg}");

    // Every exported commit stays unsigned: the key replaces old sigs only.
    let shas = git(&out, &["log", "--format=%H", "main"]);
    assert!(!shas.is_empty(), "export must produce commits");
    for sha in shas.lines() {
        let cat = git(&out, &["cat-file", "commit", sha]);
        assert!(
            !cat.contains("gpgsig"),
            "unsigned original {sha} must stay unsigned"
        );
    }
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(
        !log.contains("resigned"),
        "nothing was signed, nothing to log: {log}"
    );
    assert!(
        !log.contains("sig-dropped"),
        "nothing had a sig, nothing dropped: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_whitespace_resign_key_behaves_as_unset() {
    let tmp = std::env::temp_dir().join(format!("oot-gpg-blankkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);
    std::fs::write(src.join("README.md"), "# Public Project\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "root"]);

    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/.env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // Signed commit past the secret: rebuild drops the sig, blank key
    // behaves as unset instead of failing on a garbage key id.
    std::fs::write(src.join("README.md"), "# Public Project v2\n").unwrap();
    git(&src, &["add", "."]);
    let third_tree = git(&src, &["write-tree"]);
    let third_sha = git(&src, &["rev-parse", "HEAD"]);
    let signed_third = forge_signed(
        &src,
        &third_tree,
        Some(&third_sha),
        "iQDummyThird",
        "signed third",
    );
    git(&src, &["update-ref", "refs/heads/main", &signed_third]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\nresign_key_id = \"   \"\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(ok, "blank key must behave as unset: {msg}");

    let exported_head = git(&out, &["rev-parse", "main"]);
    let head_cat = git(&out, &["cat-file", "commit", &exported_head]);
    assert!(
        !head_cat.contains("gpgsig"),
        "rebuilt commit must ship unsigned"
    );
    let log = std::fs::read_to_string(proj.join(".oot/export-log.jsonl")).unwrap();
    assert!(log.contains("sig-dropped"), "missing drop event: {log}");
    assert!(
        log.contains(&signed_third),
        "drop event must name the original sha: {log}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_resign_with_bad_key_fails_loudly() {
    let tmp = std::env::temp_dir().join(format!("oot-gpg-badkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let src = tmp.join("src");
    let proj = tmp.join("proj");
    let out = tmp.join("out");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    git(&src, &["init", "--quiet", "-b", "main"]);

    // Root is signed so the rebuild path actually tries the key.
    // A bad key must fail on the signed rebuild, not silently pass.
    std::fs::write(src.join("README.md"), "# Public Project\n").unwrap();
    git(&src, &["add", "."]);
    let root_tree = git(&src, &["write-tree"]);
    let signed_root = forge_signed(&src, &root_tree, None, "iQDummyRoot", "signed root");
    git(&src, &["update-ref", "refs/heads/main", &signed_root]);

    std::fs::create_dir_all(src.join("secrets")).unwrap();
    std::fs::write(src.join("secrets/.env"), "API_KEY=supersecret\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-m", "add secret"]);

    // Third commit is signed but lands past the secret, so export rebuilds
    // it and must attempt the re-sign.
    std::fs::write(src.join("README.md"), "# Public Project v2\n").unwrap();
    git(&src, &["add", "."]);
    let third_tree = git(&src, &["write-tree"]);
    let third_sha = git(&src, &["rev-parse", "HEAD"]);
    let signed_third = forge_signed(
        &src,
        &third_tree,
        Some(&third_sha),
        "iQDummyThird",
        "signed third",
    );
    git(&src, &["update-ref", "refs/heads/main", &signed_third]);

    std::fs::write(
        proj.join("visibility.toml"),
        "private_paths = [\"secrets/\"]\nprivate_branches = []\nresign_key_id = \"no-such-key-oot-test\"\n",
    )
    .unwrap();

    assert!(oot(&["init"], &proj).0);
    let (ok, msg) = oot(&["import", "--repo", src.to_str().unwrap()], &proj);
    assert!(ok, "import failed: {msg}");
    let (ok, msg) = oot(&["export", "--out", out.to_str().unwrap()], &proj);
    assert!(!ok, "export with a bad key must fail");
    assert!(
        msg.contains("no-such-key-oot-test"),
        "error must name the bad key: {msg}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
