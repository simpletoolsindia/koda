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
    /// The shell the steps are written for.
    pub dialect: Dialect,
}

/// How the shell that runs a plan quotes. `cmd.exe`, Windows' default, keeps
/// single quotes as part of the word -- `python -m py_compile 'a.py'` looks for
/// a file called `'a.py'` -- and PowerShell escapes a quote by doubling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    #[default]
    Posix,
    Cmd,
    PowerShell,
}

impl Dialect {
    /// The dialect of a configured `shell`.
    pub fn of(shell: &str) -> Self {
        let base = shell
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(shell)
            .trim_end_matches(".exe")
            .to_ascii_lowercase();
        match base.as_str() {
            "cmd" => Dialect::Cmd,
            "powershell" | "pwsh" => Dialect::PowerShell,
            _ => Dialect::Posix,
        }
    }

    fn quote(self, s: &str) -> String {
        match self {
            Dialect::Posix => format!("'{}'", s.replace('\'', r"'\''")),
            // A Windows file name cannot contain a double quote.
            Dialect::Cmd => format!("\"{s}\""),
            Dialect::PowerShell => format!("'{}'", s.replace('\'', "''")),
        }
    }

    /// A command that prints `s` as it is.
    fn echo(self, s: &str) -> String {
        match self {
            // cmd's echo prints the rest of the line, quotes included, so the
            // text goes unquoted with its operators escaped.
            Dialect::Cmd => {
                let mut out = String::from("echo ");
                for c in s.chars() {
                    if "^&|<>()%".contains(c) {
                        out.push('^');
                    }
                    out.push(c);
                }
                out
            }
            d => format!("echo {}", d.quote(s)),
        }
    }
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
            .map(|s| format!("{} && {s}", self.dialect.echo(&format!("$ {s}"))))
            .collect::<Vec<_>>()
            .join(" && ")
    }
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

/// A step that needs nothing but the shell running it.
const SHELL: &str = "";

/// The Python to run. On Windows `python3` is usually the Microsoft Store's
/// "install me" stub; the python.org launcher is `py`.
fn python_program(can_run: Installed) -> &'static str {
    let order: &[&'static str] = if cfg!(windows) {
        &["py", "python", "python3"]
    } else {
        &["python3", "python"]
    };
    order
        .iter()
        .copied()
        .find(|p| can_run(p))
        .unwrap_or(order[0])
}

/// Decide the checks for `root`, given the files this turn changed (relative
/// to the root), written for `shell`. `None` when there is nothing to check.
pub fn detect(root: &Path, changed: &[String], shell: &str) -> Option<Plan> {
    detect_with(root, changed, Dialect::of(shell), &installed)
}

fn detect_with(
    root: &Path,
    changed: &[String],
    dialect: Dialect,
    can_run: Installed,
) -> Option<Plan> {
    let mut plan = Plan {
        kinds: Vec::new(),
        steps: Vec::new(),
        skipped: Vec::new(),
        dialect,
    };
    let py = python_program(can_run);
    let mut add = |plan: &mut Plan, kind: &str, program: &str, step: String| {
        if !plan.kinds.iter().any(|k| k == kind) {
            plan.kinds.push(kind.to_string());
        }
        if program == SHELL || can_run(program) {
            plan.steps.push(step);
        } else {
            plan.skipped
                .push(format!("{step} ({program} is not installed)"));
        }
    };

    // 1. What the project declares.
    if let Some(cmd) = declared_check(root) {
        add(&mut plan, "koda.toml", SHELL, cmd);
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
        python(root, changed, py, &mut plan, &mut add);
    }
    if has(root, "pom.xml") {
        add(&mut plan, "java (maven)", "mvn", "mvn -q test".into());
    } else if has(root, "build.gradle") || has(root, "build.gradle.kts") {
        let wrapper = match dialect {
            Dialect::Posix => has(root, "gradlew").then_some("./gradlew test"),
            Dialect::Cmd => has(root, "gradlew.bat").then_some("gradlew.bat test"),
            Dialect::PowerShell => has(root, "gradlew.bat").then_some(".\\gradlew.bat test"),
        };
        if let Some(step) = wrapper {
            add(&mut plan, "java (gradle)", SHELL, step.into());
        } else {
            add(&mut plan, "java (gradle)", "gradle", "gradle test".into());
        }
    }
    if !plan.kinds.is_empty() {
        return Some(plan);
    }

    // 3. Not a project: check just the files that changed.
    loose_files(changed, py, &mut plan, &mut add);
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
fn python(root: &Path, changed: &[String], python: &str, plan: &mut Plan, add: &mut Add) {
    let d = plan.dialect;
    let py: Vec<&String> = changed.iter().filter(|f| f.ends_with(".py")).collect();
    if !py.is_empty() {
        let files: Vec<String> = py.iter().map(|f| d.quote(f)).collect();
        add(
            plan,
            "python",
            python,
            format!("{python} -m py_compile {}", files.join(" ")),
        );
    }
    let pyproject = read(root, "pyproject.toml");
    if has(root, "ruff.toml") || has(root, ".ruff.toml") || pyproject.contains("[tool.ruff") {
        add(plan, "python", "ruff", "ruff check .".into());
    }
    if has_python_tests(root) || pyproject.contains("[tool.pytest") || has(root, "pytest.ini") {
        add(plan, "python", python, format!("{python} -m pytest -q"));
    }
    if plan.steps.is_empty() && plan.skipped.is_empty() {
        plan.kinds.push("python".into());
    }
}

/// Outside any project: a syntax check of each changed file its language's
/// own tool can check without building anything.
fn loose_files(changed: &[String], python: &str, plan: &mut Plan, add: &mut Add) {
    let d = plan.dialect;
    let of = |ext: &[&str]| -> Vec<String> {
        changed
            .iter()
            .filter(|f| ext.iter().any(|e| f.ends_with(e)))
            .map(|f| d.quote(f))
            .collect()
    };
    let py = of(&[".py"]);
    if !py.is_empty() {
        add(
            plan,
            "python file",
            python,
            format!("{python} -m py_compile {}", py.join(" ")),
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
        let p = detect_with(&d, &changed, Dialect::Posix, &all);
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
        let python = python_program(&all);
        assert_eq!(
            py.steps,
            vec![
                format!("{python} -m py_compile 'cart.py'"),
                format!("{python} -m pytest -q")
            ]
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
        let p = detect_with(&d, &[], Dialect::Posix, &|prog| prog != "go").unwrap();
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
            dialect: Dialect::Posix,
        };
        assert_eq!(
            p.script(),
            r"echo '$ false' && false && echo '$ echo it'\''s' && echo it's"
        );
    }

    /// cmd.exe keeps single quotes as part of a word, so a quoted file name
    /// became a file that does not exist; and `./gradlew` is not a program to it.
    #[test]
    fn windows_shells_get_steps_they_can_run() {
        assert_eq!(Dialect::of(r"C:\Windows\system32\cmd.exe"), Dialect::Cmd);
        assert_eq!(Dialect::of("pwsh"), Dialect::PowerShell);
        assert_eq!(Dialect::of("/bin/bash"), Dialect::Posix);

        let d = dir(
            "win",
            &[
                ("requirements.txt", ""),
                ("gradlew.bat", ""),
                ("build.gradle", ""),
            ],
        );
        let changed = vec!["my cart.py".to_string()];
        let p = detect_with(&d, &changed, Dialect::Cmd, &all).unwrap();
        let py = if cfg!(windows) { "py" } else { "python3" };
        assert!(
            p.steps
                .contains(&format!("{py} -m py_compile \"my cart.py\"")),
            "{:?}",
            p.steps
        );
        assert!(
            p.steps.contains(&"gradlew.bat test".to_string()),
            "{:?}",
            p.steps
        );
        let p = detect_with(&d, &changed, Dialect::PowerShell, &all).unwrap();
        assert!(
            p.steps.contains(&r".\gradlew.bat test".to_string()),
            "{:?}",
            p.steps
        );

        let script = Plan {
            kinds: vec![],
            steps: vec!["a && b".into()],
            skipped: vec![],
            dialect: Dialect::Cmd,
        }
        .script();
        assert_eq!(script, "echo $ a ^&^& b && a && b");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A project's own check needs no `sh`: on Windows there usually is none,
    /// and the check was skipped as "sh is not installed".
    #[test]
    fn a_declared_check_does_not_need_sh() {
        let d = dir(
            "declared-nosh",
            &[(
                "koda.toml",
                "[[tools]]\nname = \"check\"\ndescription = \"gate\"\ncommand = \"make lint\"\n",
            )],
        );
        let p = detect_with(&d, &[], Dialect::Cmd, &|prog| prog != "sh").unwrap();
        assert_eq!(p.steps, vec!["make lint"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
