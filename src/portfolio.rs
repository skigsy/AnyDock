use crate::workspace::{Workspace, display_native, file, optional, replace_setup_file};
use anyhow::{Context, Result, bail, ensure};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "AGENTS.md",
    "anydock.version",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
];
const WORKFLOW: &str = "When the user asks to create a project in this folder or start substantive work in one, select the actual project. \
     Create the project directory first when needed. For a new or unconfigured project, run the trusted executable with `-C <project> setup`, \
     then follow that project's AGENTS.md for the task. For an existing project, resume the matching run or start a run as its instructions describe. \
     Keep tasks and handoffs in their own project and leave archive, backup, and unrelated folders untouched. \
     Quick questions and isolated read-only checks can proceed without setup.\n\n\
     If the terminal denies setup, stop using it for AnyDock in this session and keep the sandbox enabled. Use authorized native file tools to do and verify the project work. \
     Read any existing `.anydock-handoff.md` before continuing. For a new project with no `AGENTS.md`, use native tools to create it with the project purpose, a pointer to the AnyDock projects block in `../AGENTS.md`, and a pointer to `.anydock-handoff.md`. \
     Follow the shared library directions from this projects folder. In a Git project, add `/.anydock-handoff.md` to `.gitignore` before writing or updating that note. \
     Use the note to record the task, changed files, verification, and next step. Leave managed run state alone and report that managed setup is pending. \
     If the file tools also deny a write, report the blocker.\n";
const LIBRARY_RULES: &str = "For work in these projects, use shared skills in `Skills/<name>/SKILL.md` and reusable agent roles in `Agents/<name>.md`, relative to the shared library root. \
     Prefer project-owned definitions of the same name. Read descriptions first and load only relevant definitions and their references. Honor disabled entries and opt-in rules. \
     Apply the same role instructions in the current session when you cannot delegate. Follow existing permissions and approval rules for scripts, tools, and delegation. \
     When asked to create or update reusable skills or roles, keep the authored files in this library, with skill references beside their `SKILL.md`. Keep project-only definitions with the project.\n";

pub fn setup(start: &Path) -> Result<()> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = env::var_os("HOME");
    let home = home.filter(|path| !path.is_empty()).map(PathBuf::from);
    setup_in(start, home.as_deref())
}

fn setup_in(start: &Path, home: Option<&Path>) -> Result<()> {
    let root = start.canonicalize()?;
    ensure!(root.is_dir(), "expected a folder that contains projects");
    let home = home.map(Path::canonicalize).transpose()?;
    let executable = env::current_exe()?;
    let id = crate::storage::hash(root.as_os_str().as_encoded_bytes());
    let native_rules = rules(&root, &executable)?;
    let root_instructions = file(&root, "AGENTS.md")?;
    let library = library_root(&root);
    let folder_rules = format!(
        "## AnyDock projects\n\n\
         This folder holds the active projects. Use the installed AnyDock executable or the verified package.\n\n\
         {WORKFLOW}\n\
         Shared library root relative to this `AGENTS.md`: `{library}`.\n\n\
         {LIBRARY_RULES}"
    );
    write_rules(&root_instructions, &id[..16], &folder_rules)?;

    let mut projects = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if let Some(path) = project_path(&entry)? {
            projects.push(path);
        }
    }
    projects.sort();

    let mut failures = Vec::new();
    let mut configured = 0;
    for project in &projects {
        match Workspace::setup(project) {
            Ok(()) => configured += 1,
            Err(error) => failures.push(format!("{}: {error:#}", display_native(project))),
        }
    }

    let mut profiles = 0;
    if let Some(home) = home {
        for folder in [".codex", ".gemini"] {
            match configure_profile(&home, folder, &id[..16], &native_rules) {
                Ok(true) => profiles += 1,
                Ok(false) => {}
                Err(error) => failures.push(format!("{folder}: {error:#}")),
            }
        }
    }
    println!(
        "AnyDock configured {configured} of {} projects and {profiles} installed agent profiles in {}.",
        projects.len(),
        display_native(&root)
    );
    if !failures.is_empty() {
        bail!(
            "Setup completed where possible. Review these paths and rerun setup:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn library_root(root: &Path) -> &'static str {
    if root
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("Projects"))
        && root.parent().is_some_and(|parent| {
            ["Skills", "Agents"].iter().any(|name| {
                let path = parent.join(name);
                path.is_dir() && path.canonicalize().ok().as_ref() == Some(&path)
            })
        })
    {
        ".."
    } else {
        "."
    }
}

fn project_path(entry: &fs::DirEntry) -> Result<Option<PathBuf>> {
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
        return Ok(None);
    };
    if name.starts_with('.')
        || [
            "archive",
            "archives",
            "backup",
            "backups",
            "skills",
            "agents",
            "node_modules",
            "target",
        ]
        .iter()
        .any(|value| name.eq_ignore_ascii_case(value))
        || !entry.file_type()?.is_dir()
    {
        return Ok(None);
    }
    let path = entry.path();
    if path.canonicalize().ok().as_ref() != Some(&path) {
        return Ok(None);
    }
    if PROJECT_MARKERS.iter().any(|name| path.join(name).exists()) {
        return Ok(Some(path));
    }
    Ok(None)
}

fn configure_profile(home: &Path, folder: &str, id: &str, rules: &str) -> Result<bool> {
    let directory = home.join(folder);
    let Some(metadata) = optional(fs::symlink_metadata(&directory))? else {
        return Ok(false);
    };
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && directory.canonicalize()? == directory,
        "{} must be a local agent profile directory",
        display_native(&directory)
    );
    let instructions = file(&directory, "AGENTS.md")?;
    write_rules(&instructions, id, rules)?;
    Ok(true)
}

fn rules(root: &Path, executable: &Path) -> Result<String> {
    let root = serde_json::to_string(&display_native(root))?;
    let executable = serde_json::to_string(&display_native(executable))?;
    Ok(format!(
        "## AnyDock projects\n\n\
         This rule applies only to active projects directly inside the configured projects folder, including work in their subdirectories.\n\
         Projects folder (JSON path): {root}\n\
         AnyDock executable (JSON path): {executable}\n\n\
         {WORKFLOW}\n\
         Read the configured projects folder's `AGENTS.md` for its shared skills and agent roles."
    ))
}

fn write_rules(path: &Path, id: &str, rules: &str) -> Result<()> {
    let start = format!("<!-- anydock:projects:{id}:start -->");
    let end = format!("<!-- anydock:projects:{id}:end -->");
    let previous = optional(fs::read(path))?;
    let existing = std::str::from_utf8(previous.as_deref().unwrap_or_default())
        .with_context(|| format!("{} must be UTF-8", display_native(path)))?;
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let block = format!("{start}\n{}\n{end}", rules.trim_end()).replace('\n', newline);
    let starts: Vec<_> = existing
        .match_indices(&start)
        .map(|(index, _)| index)
        .collect();
    let ends: Vec<_> = existing
        .match_indices(&end)
        .map(|(index, _)| index)
        .collect();
    let updated = match (starts.as_slice(), ends.as_slice()) {
        ([from], [to]) if from < to => {
            format!(
                "{}{block}{}",
                &existing[..*from],
                &existing[to + end.len()..]
            )
        }
        ([], []) => {
            let separator = if existing.is_empty() {
                String::new()
            } else if existing.ends_with('\n') {
                newline.to_owned()
            } else {
                newline.repeat(2)
            };
            format!("{existing}{separator}{block}{newline}")
        }
        _ => bail!(
            "{} contains an incomplete or duplicate AnyDock projects block",
            display_native(path)
        ),
    };
    replace_setup_file(path, previous.as_deref(), updated.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_existing_projects_and_native_rules_for_new_projects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("Projects");
        let home = temp.path().join("Home");
        let first = root.join("First");
        let second = root.join("Second");
        let archive = root.join("Archive");
        let attachments = root.join("Attachments");
        let agents = root.join("Agents");
        for directory in [
            &first,
            &second,
            &archive,
            &attachments,
            &agents,
            &home.join(".codex"),
            &home.join(".gemini"),
        ] {
            fs::create_dir_all(directory)?;
        }
        fs::write(first.join("AGENTS.md"), "Existing first project rules.\n")?;
        fs::write(second.join("package.json"), "{}")?;
        fs::write(archive.join("AGENTS.md"), "Archived project rules.\n")?;
        fs::write(agents.join("AGENTS.md"), "Agent library rules.\n")?;
        let codex = home.join(".codex/AGENTS.md");
        let gemini = home.join(".gemini/AGENTS.md");
        fs::write(&codex, "Existing Codex rules.\r\n")?;
        fs::write(&gemini, "Existing Antigravity rules.\n")?;

        setup_in(&root, Some(&home))?;
        setup_in(&root, Some(&home))?;
        assert!(first.join("anydock.version").is_file());
        assert!(second.join("anydock.version").is_file());
        assert!(!root.join("anydock.version").exists());
        assert!(!archive.join("anydock.version").exists());
        assert!(!agents.join("anydock.version").exists());
        assert!(!attachments.join("AGENTS.md").exists());
        assert!(
            fs::read_to_string(first.join("AGENTS.md"))?
                .starts_with("Existing first project rules.\n")
        );
        assert!(fs::read_to_string(&codex)?.starts_with("Existing Codex rules.\r\n"));
        assert!(fs::read_to_string(&gemini)?.starts_with("Existing Antigravity rules.\n"));
        for path in [codex, gemini, root.join("AGENTS.md")] {
            let rules = fs::read_to_string(path)?;
            assert_eq!(rules.matches("## AnyDock projects").count(), 1);
            assert!(rules.contains("-C <project> setup"));
            assert!(rules.contains("keep the sandbox enabled"));
            assert!(rules.contains("/.anydock-handoff.md"));
        }
        let novel = root.join("New");
        fs::create_dir(&novel)?;
        Workspace::setup(&novel)?;
        assert!(novel.join("anydock.version").is_file());
        assert!(!first.join("runs").exists());
        assert!(!second.join("runs").exists());
        Ok(())
    }

    #[test]
    fn projects_use_existing_sibling_library_through_relative_rules() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("Projects");
        let project = root.join("Child repository");
        let skills = temp.path().join("Skills");
        fs::create_dir_all(&project)?;
        fs::create_dir(&skills)?;
        fs::write(project.join("package.json"), "{}")?;

        setup_in(&root, None)?;
        let folder_rules = fs::read_to_string(root.join("AGENTS.md"))?;
        assert!(folder_rules.contains("relative to this `AGENTS.md`: `..`"));
        assert!(folder_rules.contains("Skills/<name>/SKILL.md"));
        assert!(folder_rules.contains("Agents/<name>.md"));
        let project_rules = fs::read_to_string(project.join("AGENTS.md"))?;
        assert!(project_rules.contains("immediate parent's `AGENTS.md`"));
        assert!(project_rules.contains(".agents/skills/<name>/SKILL.md"));
        assert!(project_rules.contains(".agents/agents/<name>.md"));
        for rules in [folder_rules, project_rules] {
            assert!(!rules.contains(&display_native(temp.path())));
        }
        assert_eq!(fs::read_dir(skills)?.count(), 0);
        Ok(())
    }

    #[test]
    fn separate_project_folders_keep_scopes_and_user_edits() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("Home");
        let first = temp.path().join("First Projects");
        let second = temp.path().join("Second Projects");
        for directory in [&first, &second, &home.join(".codex")] {
            fs::create_dir_all(directory)?;
        }
        setup_in(&first, Some(&home))?;
        let profile = home.join(".codex/AGENTS.md");
        let existing = fs::read_to_string(&profile)?;
        fs::write(
            &profile,
            format!("{existing}\nKeep this later user note.\n"),
        )?;
        setup_in(&second, Some(&home))?;
        setup_in(&first, Some(&home))?;
        let rules = fs::read_to_string(profile)?;
        assert_eq!(rules.matches("## AnyDock projects").count(), 2);
        assert!(rules.contains("Keep this later user note."));
        assert!(!home.join(".gemini").exists());
        Ok(())
    }
}
