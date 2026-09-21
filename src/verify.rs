//! Working out how to validate *this* project.
//!
//! koda asks for a check before a turn that changed code can end, and a check
//! is only useful if it is the right one. A fixed command cannot be: the Rust
//! gate that suits one repository fails with "could not find `Cargo.toml`" in
//! every other, and the model is then told to fix a failure that was never
//! about its work. So the check is decided per call, from what is on disk:
//!
//! 1. **What the project itself declares** — a `check` or `verify` tool in its
//!    own `koda.toml`, or a `check` target in its Makefile. The project knows
//!    its gate better than any guess.
//! 2. **Its build system**, by marker file at the root: Cargo, Go modules,
//!    npm/pnpm/yarn/bun (using the scripts it defines), Python (compile the
//!    changed files, pytest when there are tests, ruff when configured),
//!    Maven, Gradle. A repository with several gets all of them.
//! 3. **Loose files** — a script in a folder that is no project at all — get a
//!    syntax check of just the files this turn changed: never a walk of
//!    whatever directory koda happens to be in.
//!
//! A step whose program is not installed is skipped and reported, not run to
//! fail. Nothing detected means nothing to run, and the turn is not held up.

use std::path::Path;

/// What `verify` will run here, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// What was detected: "rust", "node (pnpm)", "python", "koda.toml", …
    pub kinds: Vec<String>,
    /// Shell commands, run in order from the project root; the first failure stops.
    pub steps: Vec<String>,
    /// Checks that apply but cannot run here, with the reason.
    pub skipped: Vec<String>,
}

impl Plan {
    /// One line for a status row or a prompt: "rust — cargo build --all-targets, cargo test".
    pub fn describe(&self) -> String {
        format!("{} — {}", self.kinds.join(" + "), self.steps.join(", "))
    }

    /// The whole plan as one shell command that echoes each step before it
    /// runs it and stops at the first failure.
    pub fn script(&self) -> String {
        self.steps
            .iter()
            .map(|s| format!("echo {} && {s}", shell_quote(&format!("$ {s}"))))
            .collect::<Vec<_>>()
            .join(" && ")
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn has(root: &Path, name: &str) -> bool {
    root.join(name).exists()
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_default()
}

/// Whether `program` can be run here.
type Installed<'a> = &'a dyn Fn(&str) -> bool;

fn installed(program: &str) -> bool {
    crate::tools::which_in_path(program).is_some()
}

/// Decide the checks for `root`, given the files this turn changed (relative
/// to the root). `None` when there is nothing to check.
pub fn detect(root: &Path, changed: &[String]) -> Option<Plan> {
    detect_with(root, changed, &installed)
}

fn detect_with(root: &Path, changed: &[String], can_run: Installed) -> Option<Plan> {
    let mut plan = Plan {
        kinds: Vec::new(),
        steps: Vec::new(),
        skipped: Vec::new(),
    };
    let mut add = |plan: &mut Plan, kind: &str, program: &str, step: String| {
        if !plan.kinds.iter().any(|k| k == kind) {
            plan.kinds.push(kind.to_string());
        }
        if can_run(program) {
            plan.steps.push(step);
        } else {
            plan.skipped
                .push(format!("{step} ({program} is not installed)"));
        }
    };

    // 1. What the project declares.
    if let Some(cmd) = declared_check(root) {
        add(&mut plan, "koda.toml", "sh", cmd);
        return Some(plan);
    }
    if makefile_has(root, "check") {
        add(&mut plan, "make", "make", "make check".into());
        return Some(plan);
    }

    // 2. Build systems.
    if has(root, "Cargo.toml") {
        add(
            &mut plan,
            "rust",
            "cargo",
            "cargo build --all-targets".into(),
        );
        add(&mut plan, "rust", "cargo", "cargo test".into());
    }
    if has(root, "go.mod") {
        add(&mut plan, "go", "go", "go build ./...".into());
        add(&mut plan, "go", "go", "go vet ./...".into());
        add(&mut plan, "go", "go", "go test ./...".into());
    }
    if has(root, "package.json") {
        node(root, &mut plan, &mut add);
    }
    if is_python_project(root) {
        python(root, changed, &mut plan, &mut add);
    }
    if has(root, "pom.xml") {
        add(&mut plan, "java (maven)", "mvn", "mvn -q test".into());
    } else if has(root, "build.gradle") || has(root, "build.gradle.kts") {
        if has(root, "gradlew") {
            add(&mut plan, "java (gradle)", "sh", "./gradlew test".into());
        } else {
            add(&mut plan, "java (gradle)", "gradle", "gradle test".into());
        }
    }
    if !plan.kinds.is_empty() {
        return Some(plan);
    }

    // 3. Not a project: check just the files that changed.
    loose_files(changed, &mut plan, &mut add);
    if !plan.kinds.is_empty() {
        return Some(plan);
    }
    if makefile_has(root, "test") {
        add(&mut plan, "make", "make", "make test".into());
        return Some(plan);
    }
    None
}

/// A `check` or `verify` tool the project's own koda.toml defines (without
/// arguments), used as-is. Only the project file counts: a check in the
/// user's global config was written for some other project.
fn declared_check(root: &Path) -> Option<String> {
    let text = ["koda.toml", ".koda.toml"]
        .iter()
        .map(|n| read(root, n))
        .find(|t| !t.is_empty())?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    let tools = table.get("tools")?.as_array()?;
    for name in ["verify", "check"] {
        for t in tools {
            let Some(t) = t.as_table() else { continue };
            let takes_args = t
                .get("args")
                .and_then(|a| a.as_array())
                .is_some_and(|a| !a.is_empty());
            if t.get("name").and_then(|n| n.as_str()) == Some(name) && !takes_args {
                if let Some(cmd) = t.get("command").and_then(|c| c.as_str()) {
                    return Some(cmd.to_string());
                }
            }
        }
    }
    None
}

fn makefile_has(root: &Path, target: &str) -> bool {
    let text = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .map(|n| read(root, n))
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    text.lines().any(|l| {
        l.strip_prefix(target)
            .is_some_and(|rest| rest.trim_start().starts_with(':') && !rest.starts_with(":="))
    })
}

type Add<'a> = dyn FnMut(&mut Plan, &str, &str, String) + 'a;

/// npm, pnpm, yarn or bun, by lockfile; then the scripts the package defines
/// that check something — a typecheck, a linter, the tests — and `tsc` for a
/// TypeScript package that defines none of them.
fn node(root: &Path, plan: &mut Plan, add: &mut Add) {
    let (pm, run) = if has(root, "pnpm-lock.yaml") {
        ("pnpm", "pnpm run")
    } else if has(root, "yarn.lock") {
        ("yarn", "yarn run")
    } else if has(root, "bun.lockb") || has(root, "bun.lock") {
        ("bun", "bun run")
    } else {
        ("npm", "npm run")
    };
    let kind = format!("node ({pm})");
    let pkg: serde_json::Value =
        serde_json::from_str(&read(root, "package.json")).unwrap_or_default();
    let scripts = pkg.get("scripts").and_then(|s| s.as_object());
    let mut any = false;
    for name in ["typecheck", "lint", "test"] {
        let Some(body) = scripts.and_then(|s| s.get(name)).and_then(|v| v.as_str()) else {
            continue;
        };
        // npm init's placeholder is not a test suite.
        if name == "test" && body.contains("no test specified") {
            continue;
        }
        add(plan, &kind, pm, format!("{run} {name}"));
        any = true;
    }
    if !any && has(root, "tsconfig.json") {
        add(plan, &kind, "npx", "npx --no-install tsc --noEmit".into());
        any = true;
    }
    if !any {
        plan.kinds.push(kind);
        plan.skipped
            .push("package.json defines no typecheck, lint or test script".into());
    }
}

fn is_python_project(root: &Path) -> bool {
    [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "Pipfile",
        "tox.ini",
    ]
    .iter()
    .any(|m| has(root, m))
        || has_python_tests(root)
}

/// Tests pytest would find at the root or in `tests/`.
fn has_python_tests(root: &Path) -> bool {
    let is_test =
        |n: &str| n.ends_with(".py") && (n.starts_with("test_") || n.ends_with("_test.py"));
    let in_dir = |d: &Path| {
        std::fs::read_dir(d)
            .map(|rd| {
                rd.flatten()
                    .any(|e| is_test(&e.file_name().to_string_lossy()))
            })
            .unwrap_or(false)
    };
    in_dir(root) || in_dir(&root.join("tests")) || in_dir(&root.join("test"))
}

/// The changed Python files compiled (a syntax error is the cheapest thing to
/// catch), then ruff where the project configures it, then pytest where there
/// are tests.
fn python(root: &Path, changed: &[String], plan: &mut Plan, add: &mut Add) {
    let py: Vec<&String> = changed.iter().filter(|f| f.ends_with(".py")).collect();
    if !py.is_empty() {
        let files: Vec<String> = py.iter().map(|f| shell_quote(f)).collect();
        add(
            plan,
            "python",
            "python3",
            format!("python3 -m py_compile {}", files.join(" ")),
        );
    }
    let pyproject = read(root, "pyproject.toml");
    if has(root, "ruff.toml") || has(root, ".ruff.toml") || pyproject.contains("[tool.ruff") {
        add(plan, "python", "ruff", "ruff check .".into());
    }
    if has_python_tests(root) || pyproject.contains("[tool.pytest") || has(root, "pytest.ini") {
        add(plan, "python", "python3", "python3 -m pytest -q".into());
    }
    if plan.steps.is_empty() && plan.skipped.is_empty() {
        plan.kinds.push("python".into());
    }
}

/// Outside any project: a syntax check of each changed file its language's
/// own tool can check without building anything.
fn loose_files(changed: &[String], plan: &mut Plan, add: &mut Add) {
    let of = |ext: &[&str]| -> Vec<String> {
        changed
            .iter()
            .filter(|f| ext.iter().any(|e| f.ends_with(e)))
            .map(|f| shell_quote(f))
            .collect()
    };
    let py = of(&[".py"]);
    if !py.is_empty() {
        add(
            plan,
            "python file",
            "python3",
            format!("python3 -m py_compile {}", py.join(" ")),
        );
    }
    for f in of(&[".js", ".mjs", ".cjs"]) {
        add(plan, "javascript file", "node", format!("node --check {f}"));
    }
    for f in of(&[".sh", ".bash"]) {
        add(plan, "shell script", "bash", format!("bash -n {f}"));
    }
    for f in of(&[".rb"]) {
        add(plan, "ruby file", "ruby", format!("ruby -c {f}"));
    }
}

/// Whether a changed file is code a check could be about. Prose, data and
/// config — a `.txt`, a README, a `.json` — change nothing a build or a test
/// would catch, so writing one does not ask for a check.
pub fn is_code(path: &str) -> bool {
    crate::graph::language_of(Path::new(path)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("koda-verify-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (name, body) in files {
            let p = d.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn all(_: &str) -> bool {
        true
    }

    fn plan(tag: &str, files: &[(&str, &str)], changed: &[&str]) -> Option<Plan> {
        let d = dir(tag, files);
        let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
        let p = detect_with(&d, &changed, &all);
        let _ = std::fs::remove_dir_all(&d);
        p
    }

    /// The failure this exists for: in a folder that is no Rust project, no
    /// Rust check runs — and a text file changes nothing worth checking.
    #[test]
    fn a_plain_folder_with_a_text_file_has_nothing_to_verify() {
        assert_eq!(
            plan("txt", &[("languages.txt", "rust\n")], &["languages.txt"]),
            None
        );
        assert!(!is_code("languages.txt") && !is_code("README.md"));
        assert!(is_code("src/main.rs") && is_code("app.py"));
    }

    #[test]
    fn each_build_system_gets_its_own_checks() {
        let rust = plan("rs", &[("Cargo.toml", "[package]")], &[]).unwrap();
        assert_eq!(rust.steps, vec!["cargo build --all-targets", "cargo test"]);

        let go = plan("go", &[("go.mod", "module x")], &[]).unwrap();
        assert_eq!(
            go.steps,
            vec!["go build ./...", "go vet ./...", "go test ./..."]
        );

        let node = plan(
            "node",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"lint":"eslint .","test":"vitest","dev":"vite"}}"#,
                ),
                ("pnpm-lock.yaml", ""),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(node.kinds, vec!["node (pnpm)"]);
        assert_eq!(node.steps, vec!["pnpm run lint", "pnpm run test"]);

        let placeholder = plan(
            "npmi",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
                ),
                ("tsconfig.json", "{}"),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(placeholder.steps, vec!["npx --no-install tsc --noEmit"]);

        let py = plan("py", &[("cart.py", ""), ("test_cart.py", "")], &["cart.py"]).unwrap();
        assert_eq!(
            py.steps,
            vec!["python3 -m py_compile 'cart.py'", "python3 -m pytest -q"]
        );
    }

    /// What the project declares beats any guess.
    #[test]
    fn the_projects_own_check_wins() {
        let declared = plan(
            "decl",
            &[
                ("Cargo.toml", "[package]"),
                (
                    "koda.toml",
                    "[[tools]]\nname = \"check\"\ndescription = \"gate\"\ncommand = \"cargo fmt --check && cargo test\"\n",
                ),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(declared.steps, vec!["cargo fmt --check && cargo test"]);

        let make = plan(
            "make",
            &[("Makefile", "check: lint\n\tgo test\n"), ("go.mod", "")],
            &[],
        )
        .unwrap();
        assert_eq!(make.steps, vec!["make check"]);
    }

    #[test]
    fn a_repository_with_several_languages_checks_them_all() {
        let p = plan("multi", &[("Cargo.toml", ""), ("go.mod", "")], &[]).unwrap();
        assert_eq!(p.kinds, vec!["rust", "go"]);
        assert_eq!(p.steps.len(), 5);
    }

    /// Outside a project, only the changed files — never a walk of the folder.
    #[test]
    fn loose_files_are_syntax_checked_one_by_one() {
        let p = plan("loose", &[], &["tool.sh", "a.js", "notes.txt"]).unwrap();
        assert_eq!(p.steps, vec!["node --check 'a.js'", "bash -n 'tool.sh'"]);
    }

    #[test]
    fn a_missing_tool_is_skipped_and_said() {
        let d = dir("missing", &[("go.mod", "")]);
        let p = detect_with(&d, &[], &|prog| prog != "go").unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert!(p.steps.is_empty());
        assert_eq!(p.skipped.len(), 3);
        assert!(p.skipped[0].contains("go is not installed"));
    }

    #[test]
    fn the_script_echoes_each_step_and_stops_at_the_first_failure() {
        let p = Plan {
            kinds: vec!["x".into()],
            steps: vec!["false".into(), "echo it's".into()],
            skipped: vec![],
        };
        assert_eq!(
            p.script(),
            r"echo '$ false' && false && echo '$ echo it'\''s' && echo it's"
        );
    }
}
