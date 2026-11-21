use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spacelizard"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

type Counts = BTreeMap<(String, usize, String), usize>;

fn expected(dir: &Path, extra: bool) -> Counts {
    let mut counts = Counts::new();
    for entry in dir.read_dir().unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "dm") {
            continue;
        }
        let file = path.file_name().unwrap().to_str().unwrap();
        for (index, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
            let Some(items) = line
                .split_once("// EXPECT:")
                .map(|(_, items)| items)
                .or_else(|| {
                    extra
                        .then(|| line.split_once("// EXPECT(extra):").map(|(_, items)| items))
                        .flatten()
                })
            else {
                continue;
            };
            for item in items.trim().split(',') {
                let item = item.trim();
                let (rule, count) = item
                    .split_once('*')
                    .map_or((item, 1), |(r, n)| (r, n.parse().unwrap()));
                *counts
                    .entry((file.to_owned(), index + 1, rule.to_owned()))
                    .or_default() += count;
            }
        }
    }
    counts
}

fn check(dir: &Path, flags: &[&str]) -> Output {
    cli()
        .args(["check", "--config"])
        .arg(dir.join("spacelizard.toml"))
        .args(flags)
        .output()
        .unwrap()
}

fn assert_fixture(name: &str, extra: bool) -> Vec<Value> {
    let dir = fixture(name);
    let mut flags = vec!["--format=json", "--verify-header"];
    if extra {
        flags.push("--ci");
    }
    let output = check(&dir, &flags);
    let diagnostics: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let mut actual = Counts::new();
    for d in &diagnostics {
        let file = d["file"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        let line = d["line"].as_u64().unwrap() as usize;
        let rule = d["rule"].as_str().unwrap().to_owned();
        assert!(d["column"].as_u64().unwrap() > 0, "{d}");
        *actual.entry((file, line, rule)).or_default() += 1;
    }
    let expected = expected(&dir, extra);
    let missing: Vec<_> = expected
        .iter()
        .filter(|(key, count)| actual.get(*key) != Some(*count))
        .collect();
    let unexpected: Vec<_> = actual
        .iter()
        .filter(|(key, count)| expected.get(*key) != Some(*count))
        .collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "missing: {missing:?}\nunexpected: {unexpected:?}"
    );
    let expected_exit = i32::from(diagnostics.iter().any(|d| d["severity"] == "error"));
    assert_eq!(output.status.code(), Some(expected_exit));
    diagnostics
}

#[test]
fn pr34_contracts() {
    assert_fixture("pr34", false);
    assert_fixture("pr34", true);
}

#[test]
fn pr34_clean() {
    assert!(assert_fixture("clean", false).is_empty());
}

#[test]
fn duplicate_cli_defines_do_not_warn() {
    let dir = fixture("clean");
    let output = check(&dir, &["--format=json", "-D", "DUP", "-D", "DUP=2"]);
    assert!(output.status.success());
    let diagnostics: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn representative_images_policy() {
    let diagnostics = assert_fixture("representative", false);
    assert_eq!(diagnostics[0]["column"], 2);
}

#[test]
fn new_contracts_and_variable_rules() {
    assert_fixture("mvp", false);
    let diagnostics = assert_fixture("mvp", true);
    let alias = diagnostics
        .iter()
        .find(|d| d["rule"] == "var_alias_escape")
        .unwrap();
    assert_eq!(alias["severity"], "warning");
    assert_eq!(alias["column"], 2);
}

#[test]
fn editor_and_github_output() {
    let dir = fixture("mvp");
    let text = check(&dir, &[]);
    let github = check(&dir, &["--format=github"]);
    assert_eq!(text.status.code(), Some(1));
    assert_eq!(github.status.code(), Some(1));
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .lines()
            .any(|line| line.contains(":2: error[var_assign_forbidden]:"))
    );
    assert!(
        String::from_utf8(github.stdout)
            .unwrap()
            .lines()
            .any(|line| line.starts_with("::warning file=variables.dm,")
                && line.contains("title=spacelizard [var_alias_escape]::"))
    );
}

struct TemporaryDir(PathBuf);

impl Drop for TemporaryDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn header_sync_and_mismatch() {
    let dir = TemporaryDir(
        std::env::temp_dir().join(format!("spacelizard-header-test-{}", std::process::id())),
    );
    let path = dir.0.join("include/header.dm");
    let output = cli().arg("sync-header").arg(&path).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        include_str!("../include/spacelizard.dm")
    );
    std::fs::write(
        dir.0.join("test.dme"),
        "#include \"include/header.dm\"\n/proc/test()\n\treturn 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.0.join("spacelizard.toml"),
        "environment = \"test.dme\"\nheader_path = \"include/header.dm\"\n",
    )
    .unwrap();
    assert!(check(&dir.0, &["--verify-header"]).status.success());
    std::fs::write(&path, "// modified\n").unwrap();
    let bad = check(&dir.0, &["--verify-header"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("differs from the embedded header"));
}
