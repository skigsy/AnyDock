use crate::{
    engine::{Phase, StageView, View},
    storage::{Entry, LockedRun, ReviewSet, Revision, State, live_hash, verified_blob},
    workspace::{Workspace, file, optional, validate_ref},
};
use anyhow::{Context, Result, ensure};
use similar::{DiffTag, TextDiff};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::NamedTempFile;

const MAX_TEXT_DIFF: u64 = 64 * 1024;

#[derive(Clone, Copy)]
pub struct Target<'a> {
    pub run: &'a str,
    pub task: Option<&'a str>,
}

impl Target<'_> {
    pub fn selector(self) -> String {
        self.task
            .map_or_else(|| self.run.to_owned(), |task| format!("{task} --task"))
    }
}

struct Presentation {
    path: PathBuf,
    local: bool,
    retained: Option<BTreeMap<String, String>>,
    entries: BTreeMap<String, (String, Revision)>,
}

impl Presentation {
    fn load(space: &Workspace, run: &Path, state: &State, token: &str) -> Result<Self> {
        if let Some(stages) = state.review_presentations.get(token) {
            return Self::capture(run.to_owned(), true, state, stages);
        }
        if let Some(update) = &state.update
            && let Some(previous) = &update.presentation
            && previous.token == token
        {
            for digest in previous.digests().collect::<BTreeSet<_>>() {
                verified_blob(run, digest).context("retained review archive")?;
            }
            return Ok(Self {
                path: run.to_owned(),
                local: false,
                retained: Some(
                    previous
                        .stages
                        .iter()
                        .filter_map(|item| {
                            item.reused
                                .as_ref()
                                .map(|id| (item.stage.clone(), id.clone()))
                        })
                        .collect(),
                ),
                entries: previous
                    .stages
                    .iter()
                    .map(|item| (item.stage.clone(), (item.id.clone(), item.record.clone())))
                    .collect(),
            });
        }
        if let Some(update) = &state.update
            && let Ok(source) = space.run(&update.from)
            && let Ok(Some(prior)) = LockedRun::inspect(&source)
            && prior.identity.as_ref() == Some(&update.identity)
            && let Some(stages) = prior.review_presentations.get(token)
        {
            return Self::capture(source, false, &prior, stages);
        }
        anyhow::bail!(
            "earlier batch presentation is unavailable. Run review to see the full current batch."
        )
    }

    fn capture(path: PathBuf, local: bool, state: &State, stages: &ReviewSet) -> Result<Self> {
        let entries = stages
            .iter()
            .map(|(stage, revision)| {
                let before = state
                    .stages
                    .get(stage)
                    .and_then(|entry| entry.revisions.get(revision))
                    .context("earlier review record is incomplete")?;
                Ok((stage.clone(), (revision.clone(), before.clone())))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            path,
            local,
            retained: None,
            entries,
        })
    }

    fn unchanged(&self, state: &State, stage: &str, before: &str, now: &str) -> bool {
        if self.local {
            return before == now;
        }
        if let Some(retained) = &self.retained {
            return retained.get(stage).is_some_and(|id| id == now);
        }
        state
            .update
            .as_ref()
            .and_then(|update| update.reused.get(stage))
            .is_some_and(|reuse| reuse.from == before && reuse.revision == now)
    }
}

pub fn batch(
    space: &Workspace,
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    target: Target<'_>,
    view: &View<'_>,
    since: Option<&str>,
) -> Result<()> {
    let run = target.run;
    let (token, pending) = pending_batch(view, state)?;
    let previous = since
        .map(|token| Presentation::load(space, path, state, token))
        .transpose()?;
    println!("Review {} exact revisions for {run}.", pending.len());
    if let Some(since) = since {
        println!("Compared with earlier presentation {since}.");
    }
    for item in view
        .stages
        .iter()
        .filter(|item| item.phase == Phase::Review)
    {
        let stage = &item.stage.id;
        let prior = previous
            .as_ref()
            .and_then(|p| p.entries.get(stage).map(|entry| (p, entry)));
        if let Some((p, (before, _))) = prior
            && let Some(now) = &item.revision
            && p.unchanged(state, stage, before, now)
        {
            println!(
                "Unchanged pending review: {stage} | {}",
                item.stage.fields.review.as_deref().unwrap_or_default()
            );
            continue;
        }
        let baseline = prior.map(|(p, (_, revision))| (p.path.as_path(), revision));
        gate(space, path, state, target, item, false, baseline)?;
        if let Some(next) = view.first_gated_by(item.stage, state.batch_review) {
            println!("Gates: {next}");
        }
    }
    if !state.review_presentations.contains_key(&token) {
        state.review_presentations.insert(token.clone(), pending);
        lock.save(state)?;
    }
    println!("Batch revision: {token}");
    println!(
        "If approved: {} approve-batch {} --revision {}",
        crate::workspace::invocation(&space.root),
        target.selector(),
        command_token(&token, batch_tokens(state))
    );
    Ok(())
}

const SHORT_TOKEN: usize = 12;

pub fn batch_tokens(state: &State) -> impl Iterator<Item = &String> {
    state
        .review_presentations
        .keys()
        .chain(state.last_batch.iter())
}

pub fn stage_tokens<'a>(state: &'a State, stage: &'a str) -> impl Iterator<Item = String> + 'a {
    state
        .stages
        .get(stage)
        .into_iter()
        .flat_map(|entry| entry.revisions.keys())
        .filter_map(|archive| state.review_token(stage, archive))
}

fn command_token(current: &str, known: impl IntoIterator<Item = impl AsRef<str>>) -> &str {
    let short = &current[..SHORT_TOKEN];
    if matches_token(short, current, known) {
        short
    } else {
        current
    }
}

pub fn matches_token(
    supplied: &str,
    current: &str,
    known: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    supplied == current
        || (supplied.len() >= SHORT_TOKEN
            && current.starts_with(supplied)
            && !known.into_iter().any(|candidate| {
                let candidate = candidate.as_ref();
                candidate != current && candidate.starts_with(supplied)
            }))
}

pub fn valid_note(note: &str) -> bool {
    !note.trim().is_empty() && note.len() <= 1024 && !note.chars().any(char::is_control)
}

pub fn pending_batch(view: &View<'_>, state: &State) -> Result<(String, ReviewSet)> {
    let pending = view
        .stages
        .iter()
        .filter(|item| item.phase == Phase::Review)
        .map(|item| {
            Ok((
                item.stage.id.clone(),
                item.revision.clone().context("missing review revision")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!pending.is_empty(), "no pending reviews");
    let token = state.batch_token(&pending)?;
    Ok((token, pending))
}

pub fn gate(
    space: &Workspace,
    run: &Path,
    state: &State,
    target: Target<'_>,
    item: &StageView<'_>,
    individual: bool,
    baseline: Option<(&Path, &Revision)>,
) -> Result<()> {
    let id = target.run;
    let stage = &item.stage.id;
    let entry = state.stages.get(stage).context("missing stage record")?;
    let (revision, current) = entry.current().context("missing review revision")?;
    let token = state
        .review_token(stage, revision)
        .context("missing review identity")?;
    let decision = current
        .decision
        .as_deref()
        .context("missing review decision")?;
    println!("Review {id} | {stage}");
    println!("Decision: {decision}");

    if let Some((path, before)) = baseline {
        print_delta(path, run, before, current, id)?;
    } else if let Some(prior) = approved_baseline(space, run, state, stage) {
        if let Some(source) = &prior.source_run {
            println!("Compared with the approved revision from runs/{source}.");
        }
        print_delta(&prior.path, run, &prior.revision, current, id)?;
    } else {
        if state
            .update
            .as_ref()
            .is_some_and(|update| update.baseline.contains_key(stage))
        {
            println!("Prior approved archive unavailable. Review the full current files.");
        }
        for (path, digest) in &current.outputs {
            println!("Review full file: runs/{id}/{path}");
            print_first_draft(run, digest)?;
        }
    }
    warn_unbound_links(run, current, item)?;
    if individual {
        println!("Revision: {token}");
        println!(
            "If approved: {} approve {} {stage} --revision {}",
            crate::workspace::invocation(&space.root),
            target.selector(),
            command_token(&token, stage_tokens(state, stage))
        );
    }
    Ok(())
}

fn warn_unbound_links(run: &Path, revision: &Revision, item: &StageView<'_>) -> Result<()> {
    let mut warnings = BTreeSet::new();
    for (path, digest) in revision
        .outputs
        .iter()
        .filter(|(path, _)| path.ends_with(".md"))
    {
        if let Some(markdown) = text_file(&verified_blob(run, digest)?)? {
            for target in crate::quality::markdown_targets(path, &markdown) {
                match target {
                    Ok(target)
                        if revision.outputs.contains_key(&target)
                            || (revision.basis.contains_key(&target)
                                && !item.stage.fields.input_fields.contains_key(&target)) => {}
                    Ok(target) => {
                        warnings.insert(format!(
                            "{path} links to {target}, whose bytes are outside this review."
                        ));
                    }
                    Err(error) => {
                        warnings.insert(format!(
                            "{path} contains an unverified file link. {error:#}"
                        ));
                    }
                }
            }
        }
    }
    for warning in warnings.into_iter().take(8) {
        println!("Review warning: {warning}");
    }
    Ok(())
}

fn print_first_draft(run: &Path, digest: &str) -> Result<()> {
    if let Some(text) = text_file(&verified_blob(run, digest)?)? {
        print_preview(&text.lines().collect::<Vec<_>>(), 240);
    }
    Ok(())
}

struct ApprovedBaseline {
    path: PathBuf,
    revision: Revision,
    source_run: Option<String>,
}

fn approved_baseline(
    space: &Workspace,
    run: &Path,
    state: &State,
    stage: &str,
) -> Option<ApprovedBaseline> {
    let entry = state.stages.get(stage)?;
    if let Some(previous) = entry.last_approved() {
        return Some(ApprovedBaseline {
            path: run.to_owned(),
            revision: entry.revisions.get(previous)?.clone(),
            source_run: None,
        });
    }
    let update = state.update.as_ref()?;
    let previous = update.baseline.get(stage)?;
    let source = space.run(&update.from).ok()?;
    let prior = LockedRun::inspect(&source).ok()??;
    if prior.identity.as_ref() != Some(&update.identity) {
        return None;
    }
    let entry = prior.stages.get(stage)?;
    let previous = entry.approved_at_or_before(previous)?;
    Some(ApprovedBaseline {
        path: source,
        revision: entry.revisions.get(previous)?.clone(),
        source_run: Some(update.from.clone()),
    })
}

pub struct ChangePreview {
    pub file: String,
    pub lines: Vec<String>,
    pub truncated: bool,
}

pub fn approved_changes(
    space: &Workspace,
    run: &Path,
    state: &State,
    item: &StageView<'_>,
) -> Result<Vec<ChangePreview>> {
    let mut changes = Vec::new();
    let Some(revision) = &item.revision else {
        return Ok(changes);
    };
    let current = &state.stages[&item.stage.id].revisions[revision];
    let Some(baseline) = approved_baseline(space, run, state, &item.stage.id) else {
        if state
            .update
            .as_ref()
            .is_some_and(|update| update.baseline.contains_key(&item.stage.id))
        {
            for file in current.outputs.keys() {
                changes.push(unavailable_baseline(file));
            }
        }
        return Ok(changes);
    };
    for (file, digest) in &current.outputs {
        let Some(previous) = baseline.revision.outputs.get(file) else {
            continue;
        };
        let Ok(prior_file) = verified_blob(&baseline.path, previous) else {
            changes.push(unavailable_baseline(file));
            continue;
        };
        let current_file = verified_blob(run, digest)?;
        if previous == digest {
            changes.push(ChangePreview {
                file: file.clone(),
                lines: vec![
                    "Same output bytes. Review the current input changes before approval."
                        .to_owned(),
                ],
                truncated: false,
            });
        } else if let (Some(before), Some(after)) =
            (text_file(&prior_file)?, text_file(&current_file)?)
        {
            let (lines, truncated) = bounded_preview(&review_lines(&before, &after), 160);
            changes.push(ChangePreview {
                file: file.clone(),
                lines,
                truncated,
            });
        }
    }
    Ok(changes)
}

fn unavailable_baseline(file: &str) -> ChangePreview {
    ChangePreview {
        file: file.to_owned(),
        lines: vec!["Prior approved archive unavailable. Open the full current file.".into()],
        truncated: false,
    }
}

fn print_delta(
    prior_run: &Path,
    run: &Path,
    prior: &Revision,
    current: &Revision,
    id: &str,
) -> Result<()> {
    for (name, digest) in &current.outputs {
        let current = verified_blob(run, digest)?;
        let Some(previous) = prior.outputs.get(name) else {
            println!("Review new file: runs/{id}/{name}");
            print_first_draft(run, digest)?;
            continue;
        };
        let Ok(prior_file) = verified_blob(prior_run, previous) else {
            println!("Review full file: runs/{id}/{name}. Prior archive unavailable.");
            continue;
        };
        println!("Review file: runs/{id}/{name}");
        if previous == digest {
            println!("Same output bytes. Review the current input changes before approval.");
        } else if let (Some(before), Some(after)) = (text_file(&prior_file)?, text_file(&current)?)
        {
            print_review_delta(&before, &after);
        } else {
            println!("File changed. Open it to review the current contents.");
        }
    }
    Ok(())
}

fn print_review_delta(before: &str, after: &str) {
    print_preview(&review_lines(before, after), 2048);
}

fn review_lines(before: &str, after: &str) -> Vec<String> {
    let diff = line_diff(before, after);
    let mut lines = Vec::new();
    for operation in diff.ops() {
        let old = &diff.old_slices()[operation.old_range()];
        let new = &diff.new_slices()[operation.new_range()];
        if operation.tag() == DiffTag::Equal {
            continue;
        }
        if old.len() == new.len() {
            for (&left, &right) in old.iter().zip(new) {
                if let Some(line) = inline_change(left, right) {
                    lines.push(line);
                } else {
                    lines.push(format!("-{}", left.trim_end_matches(['\r', '\n'])));
                    lines.push(format!("+{}", right.trim_end_matches(['\r', '\n'])));
                }
            }
        } else {
            lines.extend(
                old.iter()
                    .map(|line| format!("-{}", line.trim_end_matches(['\r', '\n']))),
            );
            lines.extend(
                new.iter()
                    .map(|line| format!("+{}", line.trim_end_matches(['\r', '\n']))),
            );
        }
    }
    lines
}

fn inline_change(left: &str, right: &str) -> Option<String> {
    let before = left.trim_end_matches(['\r', '\n']);
    let after = right.trim_end_matches(['\r', '\n']);
    if ["[-", "-]", "{+", "+}"]
        .iter()
        .any(|marker| before.contains(marker) || after.contains(marker))
    {
        return None;
    }
    let earlier_ending = &left[before.len()..];
    let later_ending = &right[after.len()..];
    let newline = if earlier_ending != later_ending {
        format!(" (line ending {:?} to {:?})", earlier_ending, later_ending)
    } else {
        String::new()
    };
    if before == after {
        return Some(format!("~ Whitespace or line ending changed{newline}."));
    }
    let prefix = shared_bytes(before.chars().zip(after.chars()));
    if prefix < 8 && newline.is_empty() {
        return None;
    }
    let suffix = shared_bytes(
        before[prefix..]
            .chars()
            .rev()
            .zip(after[prefix..].chars().rev()),
    );
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    let inline = format!(
        "{}[-{}-]{{+{}+}}{}",
        &before[..prefix],
        &before[prefix..old_end],
        &after[prefix..new_end],
        &after[new_end..]
    );
    if (256..=2048).contains(&before.len().max(after.len())) {
        let compact = compact_long_line(before, after);
        if compact.len() < inline.len() {
            return Some(format!("~ {compact}{newline}"));
        }
    }
    Some(format!("~ {inline}{newline}"))
}

fn compact_long_line(before: &str, after: &str) -> String {
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(25))
        .diff_chars(before, after);
    let mut text = String::new();
    let mut removed = String::new();
    let mut inserted = String::new();
    for operation in diff.ops() {
        let left = &diff.old_slices()[operation.old_range()];
        let right = &diff.new_slices()[operation.new_range()];
        // Keep substantial common spans visible. Short matches inside an edit
        // stay with that edit so dates and identifiers are easier to read.
        if operation.tag() == DiffTag::Equal && right.len() >= 8 {
            push_inline_edit(&mut text, &mut removed, &mut inserted);
            for part in right {
                text.push_str(part);
            }
        } else {
            for part in left {
                removed.push_str(part);
            }
            for part in right {
                inserted.push_str(part);
            }
        }
    }
    push_inline_edit(&mut text, &mut removed, &mut inserted);
    text
}

fn push_inline_edit(text: &mut String, removed: &mut String, inserted: &mut String) {
    if removed == inserted {
        text.push_str(removed);
    } else {
        text.push_str("[-");
        text.push_str(removed);
        text.push_str("-]{+");
        text.push_str(inserted);
        text.push_str("+}");
    }
    removed.clear();
    inserted.clear();
}

fn shared_bytes(chars: impl Iterator<Item = (char, char)>) -> usize {
    chars
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum()
}

fn print_preview<T: AsRef<str>>(lines: &[T], width: usize) {
    let (preview, truncated) = bounded_preview(lines, width);
    for line in preview {
        println!("{line}");
    }
    if truncated {
        println!("Open the full review file for the complete changes.");
    }
}

fn bounded_preview<T: AsRef<str>>(lines: &[T], width: usize) -> (Vec<String>, bool) {
    let mut preview = Vec::new();
    let mut truncated = lines.len() > 24;
    for line in lines.iter().take(24).map(AsRef::as_ref) {
        let end = line
            .char_indices()
            .nth(width)
            .map_or(line.len(), |(index, _)| index);
        if end < line.len() {
            preview.push(format!("{}…", &line[..end]));
            truncated = true;
        } else {
            preview.push(line.to_owned());
        }
    }
    (preview, truncated)
}

fn print_bounded_diff(before: &str, after: &str, earlier: &str, later: &str) {
    let rendered = line_diff(before, after)
        .unified_diff()
        .context_radius(1)
        .header(earlier, later)
        .to_string();
    print_preview(&rendered.lines().collect::<Vec<_>>(), 512);
}

pub fn diff(run: &Path, entry: &Entry, name: &str) -> Result<()> {
    let (current_id, current) = entry.current().context("no recorded handoff")?;
    let order = entry.ordered_ids()?;
    let prior = order
        .iter()
        .rev()
        .skip(1)
        .find(|id| entry.approvals.contains(*id))
        .or_else(|| order.iter().rev().nth(1));
    let Some(prior) = prior else {
        println!("{name} | first recorded handoff {current_id}");
        for (output, digest) in &current.outputs {
            verified_blob(run, digest)?;
            println!("{output} | {digest}");
        }
        return Ok(());
    };
    let before = &entry.revisions[prior].outputs;
    println!("{name} | {prior} -> {current_id}");
    let paths: BTreeSet<_> = before.keys().chain(current.outputs.keys()).collect();
    for output in paths {
        let old = before.get(output);
        let new = current.outputs.get(output);
        if old == new {
            continue;
        }
        println!("{output}");
        let left = text(run, old)?;
        let right = text(run, new)?;
        match (left, right) {
            (Some(left), Some(right)) => print!(
                "{}",
                line_diff(&left, &right)
                    .unified_diff()
                    .header("previous", "current")
            ),
            _ => println!(
                "  Digest: {} -> {}",
                old.map_or("absent", String::as_str),
                new.map_or("absent", String::as_str)
            ),
        }
    }
    Ok(())
}

fn text(run: &Path, digest: Option<&String>) -> Result<Option<String>> {
    let Some(digest) = digest else {
        return Ok(Some(String::new()));
    };
    text_file(&verified_blob(run, digest)?)
}

fn text_file(path: &Path) -> Result<Option<String>> {
    let Some(metadata) = optional(fs::metadata(path))? else {
        return Ok(None);
    };
    if metadata.len() > MAX_TEXT_DIFF {
        return Ok(None);
    }
    let Some(content) = optional(fs::read(path))? else {
        return Ok(None);
    };
    if content.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(content).ok())
}

pub fn restore(run: &Path, entry: &Entry, revision: &str, output: &str, dest: &str) -> Result<()> {
    validate_ref(dest)?;
    ensure!(
        dest.starts_with("recovered/"),
        "write the recovered copy under recovered/"
    );
    let digest = entry
        .revisions
        .get(revision)
        .context("unknown archive ID")?
        .outputs
        .get(output)
        .context("output is absent from the archived revision")?;
    let archive = verified_blob(run, digest)?;
    let dest = file(run, dest)?;
    let parent = dest.parent().context("missing destination folder")?;
    fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    io::copy(&mut File::open(archive)?, &mut temp)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&dest)
        .with_context(|| format!("write recovered copy at {}", dest.display()))?;
    crate::storage::sync_dir(parent)?;
    println!("Recovered copy: {}", dest.display());
    Ok(())
}

pub fn working_diff(run: &Path, entry: &Entry, stage: &str) -> Result<()> {
    let (_, revision) = entry.current().context("no recorded handoff")?;
    println!("{stage} | unrecorded changes against the last recorded handoff");
    for (name, digest) in &revision.outputs {
        let current = live_hash(run, name)?;
        if current.as_ref() == Some(digest) {
            continue;
        }
        println!("{name}");
        let live = file(run, name)?;
        if let (Some(before), Some(after)) = (text(run, Some(digest))?, text_file(&live)?) {
            print_bounded_diff(&before, &after, "recorded", "working");
        } else {
            println!(
                "  Digest: {digest} -> {}",
                current.as_deref().unwrap_or("absent")
            );
        }
    }
    Ok(())
}

fn line_diff<'a>(left: &'a str, right: &'a str) -> TextDiff<'a, 'a, 'a, str> {
    TextDiff::configure()
        .timeout(Duration::from_millis(100))
        .diff_lines(left, right)
}

pub fn compare_archives(
    left: &Path,
    left_digest: &str,
    right: &Path,
    right_digest: &str,
) -> Result<()> {
    if let (Some(left), Some(right)) = (text_file(left)?, text_file(right)?) {
        let diff = line_diff(&left, &right);
        print!("{}", diff.unified_diff().header("left", "right"));
    } else {
        println!("  Digest: {left_digest} -> {right_digest}");
    }
    Ok(())
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn repeated_link_edits_preserve_every_value() {
        let previous = (100001..=100008)
            .map(|id| format!("[#{id}](issues/{id})"))
            .collect::<Vec<_>>()
            .join(", ");
        let next = (200001..=200008)
            .map(|id| format!("[#{id}](issues/{id})"))
            .collect::<Vec<_>>()
            .join(", ");
        let old = format!("Changes: {previous}");
        let new = format!("Changes: {next}");
        let shown = inline_change(&old, &new).unwrap();
        assert!(shown.len() < old.len() + new.len());
        let mut remaining = shown.strip_prefix("~ ").unwrap();
        let mut reconstructed = (String::new(), String::new());
        while let Some((common, rest)) = remaining.split_once("[-") {
            let (removed, rest) = rest.split_once("-]{+").unwrap();
            let (inserted, rest) = rest.split_once("+}").unwrap();
            reconstructed.0.push_str(common);
            reconstructed.0.push_str(removed);
            reconstructed.1.push_str(common);
            reconstructed.1.push_str(inserted);
            remaining = rest;
        }
        reconstructed.0.push_str(remaining);
        reconstructed.1.push_str(remaining);
        assert_eq!(reconstructed, (old, new));
    }
}
