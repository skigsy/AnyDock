use crate::{
    check,
    storage::{copy_exact, fault, fingerprint, lock_file, sync_dir},
    workspace::{self, VERSION_FILE, Workspace, directory, file, optional, slug, write_new},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

type Files = BTreeMap<PathBuf, String>;

pub fn run(
    start: &Path,
    marker: Option<&str>,
    backup: Option<&Path>,
    check_only: bool,
) -> Result<()> {
    let root = start.canonicalize()?;
    if marker.is_none() || workspace::has_marker(&root)? {
        check(&Workspace::open(&root)?)?;
        println!("This workspace is compatible with this binary.");
        return Ok(());
    }
    let marker = marker.context("choose the previous workspace marker")?;
    let layout = marker
        .strip_suffix(".version")
        .context("expected a previous workspace .version filename")?;
    slug(layout)?;
    ensure!(
        marker != VERSION_FILE,
        "use upgrade --check for current workspaces"
    );
    let marker_file = file(&root, marker)?;
    ensure!(
        fs::read_to_string(&marker_file)?.trim() == "1",
        "unsupported source workspace version"
    );
    let namespace = format!(".{layout}");

    let scratch = tempfile::tempdir()?;
    let source = workspace_files(&root, marker, &namespace)?;
    let validation = scratch.path().join("candidate");
    fs::create_dir(&validation)?;
    let validation = validation.canonicalize()?;
    let mut planned = BTreeMap::new();
    for (relative, digest) in &source {
        let converted = convert(relative, &namespace);
        let destination = validation.join(&converted);
        let selected = root.join(relative);
        if converted != *relative
            && relative
                .file_name()
                .is_some_and(|name| name == "registered")
        {
            fs::create_dir_all(
                destination
                    .parent()
                    .context("missing managed metadata folder")?,
            )?;
            write_new(&destination, b"anydock-managed\n")?;
            planned.insert(converted, file_digest(&destination)?);
        } else {
            copy_exact(&selected, &destination, digest)?;
            if converted != *relative {
                planned.insert(converted, digest.clone());
            }
        }
        if is_contract(relative) && legacy_contract(&selected, layout)? {
            let receipt = relative.with_file_name("contract-format");
            let destination = validation.join(&receipt);
            write_new(&destination, format!("{layout}\n").as_bytes())?;
            planned.insert(receipt, file_digest(&destination)?);
        }
    }
    let previous = PathBuf::from(".anydock").join("previous-layout");
    fs::create_dir_all(validation.join(".anydock"))?;
    write_new(
        &validation.join(&previous),
        format!("{layout}\n").as_bytes(),
    )?;
    planned.insert(previous.clone(), file_digest(&validation.join(previous))?);
    write_new(&validation.join(VERSION_FILE), b"1\n")?;
    check(&Workspace::open(&validation)?)?;
    ensure!(
        source == workspace_files(&root, marker, &namespace)?,
        "workspace changed during verification; retry when its files are saved"
    );
    for (path, digest) in &planned {
        preflight(&root, path, digest)?;
    }
    if check_only {
        println!("Upgrade check passed. The original workspace is unchanged.");
        return Ok(());
    }

    let backup = backup_path(&root, backup)?;
    let meta = directory(&root, ".anydock")?;
    fs::create_dir_all(&meta)?;
    let _lock = lock_file(&file(&root, ".anydock/upgrade.lock")?, "workspace upgrade")?;
    ensure!(
        source == workspace_files(&root, marker, &namespace)?,
        "workspace changed before upgrade; retry"
    );
    preserve_backup(&backup, &root, marker, &namespace, &source)?;
    fault("after-upgrade-backup");
    for (relative, digest) in &planned {
        install(&root, &validation, relative, digest)?;
    }
    fault("after-upgrade-files");
    ensure!(
        source == workspace_files(&root, marker, &namespace)?,
        "previous workspace changed during upgrade; its data and the backup are preserved"
    );
    workspace::append_ignore(&file(&root, ".gitignore")?)?;
    write_new(&root.join(VERSION_FILE), b"1\n")?;
    sync_dir(&root)?;
    println!(
        "Upgraded this workspace in place. Verified previous-workspace backup: {}",
        backup.display()
    );
    Ok(())
}

fn workspace_files(root: &Path, marker: &str, namespace: &str) -> Result<Files> {
    let mut files = Files::new();
    // Exclusion rules and instructions remain at their original paths.
    for name in [
        marker,
        "AGENTS.md",
        "runs",
        "workflows",
        "inbox",
        "bundles",
        "briefs",
        namespace,
    ] {
        collect(root, Path::new(name), &mut files)?;
    }
    Ok(files)
}

fn collect(root: &Path, relative: &Path, files: &mut Files) -> Result<()> {
    let path = root.join(relative);
    let Some(metadata) = optional(fs::symlink_metadata(&path))? else {
        return Ok(());
    };
    ensure!(
        path.canonicalize()? == path && !metadata.is_symlink(),
        "path redirects during upgrade: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(&path)? {
            let relative = relative.join(entry?.file_name());
            // These are files published by a prior interrupted migration, not source data.
            let name = relative.file_name().context("missing path name")?;
            let managed = relative.starts_with("runs") || relative.starts_with("workflows");
            let depth = relative.components().count();
            if managed
                && ((depth == 3 && name == ".anydock")
                    || (name == "contract-format"
                        && is_contract(&relative.with_file_name("stage.md"))))
            {
                continue;
            }
            collect(root, &relative, files)?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "unsupported file type: {}",
            path.display()
        );
        files.insert(relative.into(), file_digest(&path)?);
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<String> {
    Ok(fingerprint(fs::File::open(path)?)?)
}

fn convert(relative: &Path, namespace: &str) -> PathBuf {
    let components: Vec<_> = relative.components().collect();
    let managed = components.first().is_some_and(|part| matches!(part, Component::Normal(value) if *value == "runs" || *value == "workflows"));
    components
        .iter()
        .enumerate()
        .map(|(index, part)| {
            if (index == 0 || (managed && index == 2)) && part.as_os_str() == namespace {
                std::ffi::OsStr::new(".anydock")
            } else {
                part.as_os_str()
            }
        })
        .collect()
}

fn is_contract(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(|part| part.as_os_str()).collect();
    parts.len() == 5
        && (parts[0] == "runs" || parts[0] == "workflows")
        && parts[2] == "stages"
        && parts[4] == "stage.md"
}

fn legacy_contract(path: &Path, layout: &str) -> Result<bool> {
    let fence = format!("\x60\x60\x60{layout}");
    let bytes = fs::read_to_string(path)?;
    Ok(bytes
        .lines()
        .any(|line| line.trim_start_matches('\u{feff}').trim_end() == fence))
}

fn backup_path(root: &Path, supplied: Option<&Path>) -> Result<PathBuf> {
    let backup = if let Some(path) = supplied {
        root.join(path)
    } else {
        let mut name = root
            .file_name()
            .context("expected a named workspace folder")?
            .to_os_string();
        name.push(".pre-upgrade");
        root.with_file_name(name)
    };
    let parent = backup
        .parent()
        .context("missing backup parent")?
        .canonicalize()?;
    let backup = parent.join(backup.file_name().context("missing backup name")?);
    ensure!(
        !backup.starts_with(root) && !root.starts_with(&backup),
        "backup must be outside the workspace in an existing parent folder"
    );
    Ok(backup)
}

fn preserve_backup(
    backup: &Path,
    root: &Path,
    marker: &str,
    namespace: &str,
    source: &Files,
) -> Result<()> {
    if backup.try_exists()? {
        ensure!(
            backup.canonicalize()? == backup
                && workspace_files(backup, marker, namespace)? == *source,
            "existing backup differs from the previous workspace: {}",
            backup.display()
        );
        return Ok(());
    }
    let parent = backup.parent().context("missing backup parent")?;
    let staged = tempfile::tempdir_in(parent)?;
    let temp = staged.path().canonicalize()?;
    for (relative, digest) in source {
        copy_exact(&root.join(relative), &temp.join(relative), digest)?;
    }
    if let Some(ignore) = optional(fs::read(file(root, ".gitignore")?))? {
        write_new(&temp.join(".gitignore"), &ignore)?;
    }
    ensure!(
        workspace_files(&temp, marker, namespace)? == *source,
        "backup verification failed"
    );
    let mut receipt = tempfile::NamedTempFile::new_in(&temp)?;
    serde_json::to_writer_pretty(&mut receipt, source)?;
    writeln!(receipt)?;
    receipt.as_file().sync_all()?;
    receipt.persist_noclobber(temp.join("backup-sha256.json"))?;
    fs::rename(&temp, backup)?;
    sync_dir(parent)
}

fn preflight(root: &Path, relative: &Path, digest: &str) -> Result<bool> {
    let name = relative
        .to_str()
        .context("invalid metadata path")?
        .replace('\\', "/");
    let destination = file(root, &name)?;
    if destination.try_exists()? {
        ensure!(
            file_digest(&destination)? == digest,
            "upgrade would replace a different file: {}",
            destination.display()
        );
        return Ok(true);
    }
    Ok(false)
}

fn install(root: &Path, staged: &Path, relative: &Path, digest: &str) -> Result<()> {
    if preflight(root, relative, digest)? {
        return Ok(());
    }
    let destination = root.join(relative);
    let parent = destination
        .parent()
        .context("missing upgrade file parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(&mut fs::File::open(staged.join(relative))?, &mut temp)?;
    temp.as_file().sync_all()?;
    ensure!(
        file_digest(temp.path())? == digest,
        "staged upgrade file changed"
    );
    temp.persist_noclobber(&destination)?;
    sync_dir(parent)
}
