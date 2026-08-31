//! Skills: reusable instructions the agent loads only when they apply.
//!
//! The system prompt is deliberately short, because every token in it competes
//! with the user's code for a local model's context. A skill solves that with
//! progressive disclosure: the prompt carries one line per skill (name and when
//! to use it), and the body is fetched with the `skill` tool only when the task
//! actually matches. An unused skill costs about fifteen tokens.
//!
//! Skills are plain markdown with YAML-ish frontmatter, so writing one needs no
//! tooling:
//!
//! ```text
//! ---
//! name: migrations
//! when: Writing or reviewing a database migration
//! ---
//! Migrations live in db/migrate/ ...
//! ```

use crate::{tel_debug, tel_warn};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// One line telling the model when this is relevant.
    pub when: String,
    pub body: String,
    /// If set, this skill defines a *role* a subagent can be spun up as
    /// (e.g. "dev", "qa", "manager", "tester"). The body becomes that agent's
    /// operating instructions when the orchestrator delegates to the role.
    pub role: Option<String>,
    pub source: PathBuf,
}

/// Where skills are read from, lowest priority first. A project skill with the
/// same name as a user skill wins, so a repo can override a personal habit.
pub fn dirs(root: &Path) -> Vec<PathBuf> {
    vec![
        crate::config::config_dir().join("skills"),
        root.join(".koda").join("skills"),
    ]
}

/// Load every skill, project overriding user by name.
pub fn load(root: &Path) -> Vec<Skill> {
    let mut found: Vec<Skill> = Vec::new();
    for dir in dirs(root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .map(|e| e == "md" || e == "markdown")
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();
        for path in paths {
            match parse_file(&path) {
                Ok(skill) => {
                    found.retain(|s| s.name != skill.name);
                    tel_debug!("skill", "loaded", "name" => skill.name, "from" => path.display());
                    found.push(skill);
                }
                Err(e) => {
                    tel_warn!("skill", format!("skipped {}: {e}", path.display()));
                }
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn parse_file(path: &Path) -> Result<Skill, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut skill = parse(&text).ok_or_else(|| {
        "missing frontmatter — needs `---`, then `name:` and `when:`, then `---`".to_string()
    })?;
    if skill.name.is_empty() {
        // Fall back to the filename, which is usually what the author meant.
        skill.name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    if skill.name.is_empty() || skill.when.is_empty() {
        return Err("frontmatter needs both `name` and `when`".into());
    }
    if skill.body.trim().is_empty() {
        return Err("skill has no body".into());
    }
    skill.source = path.to_path_buf();
    Ok(skill)
}

/// Parse frontmatter + body. Tolerant of `description:` as a synonym for
/// `when:`, since that is what other agent tools call it.
pub fn parse(text: &str) -> Option<Skill> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let (front, body) = rest.split_at(end);
    let body = body
        .trim_start_matches('\n')
        .trim_start_matches("---")
        .trim_start_matches('\n')
        .to_string();

    let mut name = String::new();
    let mut when = String::new();
    let mut role = None;
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim().to_ascii_lowercase().as_str() {
            "name" => name = v,
            "when" | "description" | "desc" => when = v,
            "role" => role = if v.is_empty() { None } else { Some(v.to_ascii_lowercase()) },
            _ => {}
        }
    }
    Some(Skill {
        name,
        when,
        body,
        role,
        source: PathBuf::new(),
    })
}

/// The catalogue injected into the system prompt: one line each.
pub fn catalogue(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nSkills — project conventions you must follow when they apply. Call \
         `skill` with the name to read one before doing that kind of work:\n",
    );
    for s in skills {
        let _ = writeln!(out, "- {}: {}", s.name, s.when);
    }
    let roles = roles(skills);
    if !roles.is_empty() {
        out.push_str(
            "\nRole agents — delegate a subtask to one with `delegate` (pass `role`). \
             Use `/orc`-style decomposition for multi-part work:\n",
        );
        for (role, when) in roles {
            let _ = writeln!(out, "- {role}: {when}");
        }
    }
    out.push_str(
        "\nWhen a request implies repeated, distinct kinds of work (e.g. implement + test + \
         review), you may create a focused role agent with `manage_agent`, then delegate to \
         it — rather than doing every specialised subtask yourself.\n",
    );
    out
}

pub fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let want = name.trim().to_ascii_lowercase();
    skills
        .iter()
        .find(|s| s.name.to_ascii_lowercase() == want)
        .or_else(|| {
            skills
                .iter()
                .find(|s| s.name.to_ascii_lowercase().contains(&want))
        })
}

/// The available role-agents defined by skill files: (role, when).
pub fn roles(skills: &[Skill]) -> Vec<(String, String)> {
    skills
        .iter()
        .filter_map(|s| s.role.as_ref().map(|r| (r.clone(), s.when.clone())))
        .collect()
}

/// Find a role-agent skill by its role name (e.g. "dev", "qa").
pub fn find_role<'a>(skills: &'a [Skill], role: &str) -> Option<&'a Skill> {
    let want = role.trim().to_ascii_lowercase();
    skills.iter().find(|s| s.role.as_deref() == Some(want.as_str()))
}

/// Write a starter skill so `koda skills --init` gives the user something to edit.
pub fn write_example(root: &Path) -> std::io::Result<PathBuf> {
    let dir = root.join(".koda").join("skills");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("example.md");
    if !path.exists() {
        std::fs::write(&path, EXAMPLE)?;
    }
    let role_path = dir.join("dev-agent.md");
    if !role_path.exists() {
        std::fs::write(&role_path, ROLE_EXAMPLE)?;
    }
    Ok(path)
}

pub const EXAMPLE: &str = r#"---
name: example
when: Never — delete this file once you have written a real skill
---

A skill is instructions the agent loads only when the `when:` line above matches
what you asked for. Keep it specific and imperative.

Good things to put in a skill:

- Conventions a newcomer would get wrong: file layout, naming, which helper to
  use instead of rolling a new one.
- Commands that must run: `just fmt` before committing, `npm run check` after
  touching types.
- Traps: "never DROP a column in the same release that stops writing to it."

Keep it under about 50 lines. If it is longer, it is probably two skills.

Files here are read from:
  ~/.config/koda/skills/     your own, every project
  <project>/.koda/skills/    this repo's, commit them for your team
"#;

/// A starter *role* skill: a skill with a `role:` line becomes a specialised
/// subagent the orchestrator (`/orc`) or `delegate` can spin up.
pub const ROLE_EXAMPLE: &str = r#"---
name: dev-agent
role: dev
when: Implementing a feature or fixing a bug end to end
---

You are the dev agent. Given a subtask brief (goal, what to change, how to
validate):

- Read the relevant code first; match the project's existing style and helpers.
- Make the change, then run the validation the brief names (tests, build, lint).
- Report exactly what you changed (files and why) and the validation result.
- If the brief is ambiguous, state the assumption you made rather than stalling.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let s = parse("---\nname: migrations\nwhen: Writing a migration\n---\nAlways add a down.\n")
            .unwrap();
        assert_eq!(s.name, "migrations");
        assert_eq!(s.when, "Writing a migration");
        assert_eq!(s.body.trim(), "Always add a down.");
        // No role by default.
        assert_eq!(s.role, None);
    }

    #[test]
    fn parses_a_role_agent_and_finds_it() {
        let s = parse("---\nname: qa-agent\nrole: QA\nwhen: Testing a change\n---\nRun the suite.\n")
            .unwrap();
        assert_eq!(s.role.as_deref(), Some("qa"), "role is lowercased");
        let skills = vec![s];
        assert!(find_role(&skills, "qa").is_some());
        assert!(find_role(&skills, "QA").is_some());
        assert!(find_role(&skills, "dev").is_none());
        assert_eq!(roles(&skills), vec![("qa".to_string(), "Testing a change".to_string())]);
    }

    #[test]
    fn accepts_description_as_a_synonym() {
        let s = parse("---\nname: x\ndescription: When doing x\n---\nbody\n").unwrap();
        assert_eq!(s.when, "When doing x");
    }

    #[test]
    fn rejects_text_without_frontmatter() {
        assert!(parse("just some markdown\n").is_none());
    }

    #[test]
    fn catalogue_is_one_line_per_skill() {
        let skills = vec![
            Skill {
                name: "a".into(),
                when: "doing a".into(),
                body: "x".into(),
                role: None,
                source: PathBuf::new(),
            },
            Skill {
                name: "b".into(),
                when: "doing b".into(),
                body: "y".into(),
                role: None,
                source: PathBuf::new(),
            },
        ];
        let c = catalogue(&skills);
        assert_eq!(c.lines().filter(|l| l.starts_with("- ")).count(), 2);
        assert!(c.contains("a: doing a"));
        // The bodies must not leak into the prompt.
        assert!(!c.contains('x'));
        assert!(catalogue(&[]).is_empty());
    }

    #[test]
    fn find_matches_exactly_then_loosely() {
        let skills = vec![Skill {
            name: "migrations".into(),
            when: "w".into(),
            body: "b".into(),
            role: None,
            source: PathBuf::new(),
        }];
        assert!(find(&skills, "Migrations").is_some());
        assert!(find(&skills, "migr").is_some());
        assert!(find(&skills, "nope").is_none());
    }

    #[test]
    fn project_skills_override_user_skills_by_name() {
        let base = std::env::temp_dir().join("koda-skill-test");
        std::fs::remove_dir_all(&base).ok();
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join(".koda/skills")).unwrap();
        std::fs::write(
            proj.join(".koda/skills/shared.md"),
            "---\nname: shared\nwhen: project\n---\nproject body\n",
        )
        .unwrap();
        let loaded = load(&proj);
        let s = find(&loaded, "shared").expect("skill should load");
        assert_eq!(s.when, "project");
        assert!(s.body.contains("project body"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_bundled_example_parses() {
        let s = parse(EXAMPLE).expect("example must be a valid skill");
        assert_eq!(s.name, "example");
        assert!(!s.when.is_empty());
    }
}
