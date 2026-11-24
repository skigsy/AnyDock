use crate::workspace::{directory, file, optional};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::{NamedTempFile, TempDir};

pub type Hashes = BTreeMap<String, String>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Input {
    pub hash: String,
    pub producer_revision: Option<String>,
}

pub type Basis = BTreeMap<String, Input>;

#[derive(Deserialize, Serialize)]
pub struct Attempt {
    pub id: u64,
    pub basis: Basis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Revision {
    pub basis: Basis,
    pub outputs: Hashes,
    pub decision: Option<String>,
    pub edited: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
}

#[derive(Default, Deserialize, Serialize)]
pub struct Entry {
    pub attempt: Option<Attempt>,
    pub last_completed_attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<i64>,
    pub head: Option<String>,
    pub revisions: BTreeMap<String, Revision>,
    pub approvals: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rejections: BTreeMap<String, String>,
    #[serde(default)]
    pub order: Vec<String>,
}

fn revision_id(parent: Option<&str>, revision: &Revision) -> Result<String> {
    Ok(hash(&serde_json::to_vec(&(parent, revision))?))
}

impl Entry {
    pub fn mark_finalized(&mut self, finalized: bool) {
        self.finalized_at = finalized.then(|| chrono::Utc::now().timestamp());
    }
    pub fn current(&self) -> Option<(&str, &Revision)> {
        self.head.as_deref().map(|id| (id, &self.revisions[id]))
    }

    pub fn last_approved(&self) -> Option<&str> {
        self.last_approved_in(&self.order)
    }

    pub fn approved_at_or_before(&self, revision: &str) -> Option<&str> {
        if self.approvals.contains(revision) {
            return self.approvals.get(revision).map(String::as_str);
        }
        let position = self.order.iter().position(|id| id == revision)?;
        self.last_approved_in(&self.order[..position])
    }

    fn last_approved_in<'a>(&self, order: &'a [String]) -> Option<&'a str> {
        order
            .iter()
            .rev()
            .find(|id| self.approvals.contains(*id))
            .map(String::as_str)
    }

    pub fn record(&mut self, revision: Revision) -> Result<String> {
        let id = revision_id(self.head.as_deref(), &revision)?;
        self.revisions.insert(id.clone(), revision);
        self.order.push(id.clone());
        self.head = Some(id.clone());
        Ok(id)
    }

    pub fn ordered_ids(&self) -> Result<Vec<String>> {
        if self.order.len() == self.revisions.len() {
            return Ok(self.order.clone());
        }
        // Legacy records have no explicit order. Each ID hashes its predecessor.
        let mut order = Vec::new();
        let mut remaining: BTreeSet<_> = self.revisions.keys().collect();
        while !remaining.is_empty() {
            let parent = order.last().map(String::as_str);
            let next = remaining.iter().find(|id| {
                revision_id(parent, &self.revisions[**id]).is_ok_and(|expected| expected == ***id)
            });
            let id = *next.context("legacy revision history is inconsistent")?;
            order.push(id.clone());
            remaining.remove(id);
        }
        ensure!(
            self.head.as_ref() == order.last(),
            "legacy revision head does not match history"
        );
        Ok(order)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Origin {
    pub run: String,
    pub identity: String,
    pub snapshot: String,
}

#[derive(Deserialize, Serialize)]
pub struct ReusedRevision {
    pub from: String,
    pub revision: String,
}

#[derive(Deserialize, Serialize)]
pub struct RunUpdate {
    pub from: String,
    pub identity: String,
    pub inputs: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub baseline: BTreeMap<String, String>,
    pub reused: BTreeMap<String, ReusedRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ReviewPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<Hashes>,
}

impl RunUpdate {
    fn state_version(&self) -> u32 {
        if self.submission.is_some() {
            5
        } else if self.presentation.is_some() {
            4
        } else {
            3
        }
    }
}

pub fn validate_input_hashes(hashes: &Hashes) -> Result<()> {
    ensure!(
        !hashes.is_empty(),
        "recurring refresh needs a declared external input"
    );
    for (name, digest) in hashes {
        crate::workspace::validate_ref(name)?;
        ensure!(
            name.starts_with("inputs/") && is_hash(digest),
            "invalid submitted input {name}"
        );
    }
    Ok(())
}

pub type ReviewSet = Vec<(String, String)>;

#[derive(Clone, Deserialize, Serialize)]
pub struct PresentedRevision {
    pub stage: String,
    pub id: String,
    parent: Option<String>,
    pub record: Revision,
    pub reused: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ReviewPresentation {
    pub token: String,
    identity: String,
    pub stages: Vec<PresentedRevision>,
}

impl ReviewPresentation {
    pub fn capture(state: &State, token: &str) -> Result<Self> {
        if let Some(update) = &state.update
            && let Some(previous) = &update.presentation
            && previous.token == token
        {
            return Ok(previous.clone());
        }
        let stages = state
            .review_presentations
            .get(token)
            .context("earlier batch presentation is unavailable in the source run")?;
        let stages = stages
            .iter()
            .map(|(stage, id)| {
                let entry = &state.stages[stage];
                let index = entry
                    .order
                    .iter()
                    .position(|item| item == id)
                    .context("earlier review record is incomplete")?;
                Ok(PresentedRevision {
                    stage: stage.clone(),
                    id: id.clone(),
                    parent: index
                        .checked_sub(1)
                        .map(|previous| entry.order[previous].clone()),
                    record: entry.revisions[id].clone(),
                    reused: Some(id.clone()),
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            token: token.into(),
            identity: state.identity.clone().context("missing review identity")?,
            stages,
        })
    }

    pub fn digests(&self) -> impl Iterator<Item = &String> {
        self.stages
            .iter()
            .flat_map(|stage| stage.record.outputs.values())
    }

    fn validate(&self, state: &State, update: &RunUpdate) -> Result<()> {
        let revisions = self
            .stages
            .iter()
            .map(|item| (item.stage.clone(), item.id.clone()))
            .collect();
        ensure!(
            is_hash(&self.identity)
                && !self.stages.is_empty()
                && batch_token(&self.identity, &revisions)? == self.token,
            "invalid retained batch presentation"
        );
        let mut seen = BTreeSet::new();
        for item in &self.stages {
            crate::workspace::slug(&item.stage)?;
            ensure!(
                seen.insert(&item.stage)
                    && item.parent.as_deref().is_none_or(is_hash)
                    && !item.record.outputs.is_empty()
                    && item.record.decision.is_some()
                    && item.record.outputs.values().all(|digest| is_hash(digest))
                    && revision_id(item.parent.as_deref(), &item.record)? == item.id,
                "invalid retained presentation revision for {}",
                item.stage
            );
            for path in item.record.outputs.keys() {
                crate::workspace::validate_ref(path)?;
            }
            if let Some(reused) = &item.reused {
                ensure!(
                    update
                        .reused
                        .get(&item.stage)
                        .is_some_and(|entry| &entry.revision == reused)
                        && state
                            .stages
                            .get(&item.stage)
                            .and_then(|entry| entry.revisions.get(reused))
                            .is_some_and(|revision| revision.outputs == item.record.outputs
                                && revision.decision == item.record.decision),
                    "retained presentation has an invalid match for {}",
                    item.stage
                );
            }
        }
        Ok(())
    }
}

fn batch_token(identity: &str, stages: &ReviewSet) -> Result<String> {
    let revision = hash(&serde_json::to_vec(stages)?);
    Ok(hash(
        format!("review/{identity}/batch/{revision}").as_bytes(),
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecurringStart {
    pub task: String,
    pub request: String,
    pub submitted: Hashes,
}

#[derive(Deserialize, Serialize)]
pub struct State {
    version: u32,
    #[serde(default)]
    pub identity: Option<String>,
    pub sequence: u64,
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_input_default: Option<crate::input_scope::Format>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<RunUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_start: Option<RecurringStart>,
    #[serde(default)]
    pub batch_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_batch: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review_presentations: BTreeMap<String, ReviewSet>,
    pub stages: BTreeMap<String, Entry>,
    #[serde(default)]
    pub review_requirements: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_offer: Option<crate::transfer::Offer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_receipt: Option<String>,
}

impl State {
    pub fn input_format(&self, stage: &crate::workspace::Stage) -> crate::input_scope::Format {
        stage
            .fields
            .input_value_format
            .or(self.scoped_input_default)
            .unwrap_or(crate::input_scope::Format::Exact)
    }

    pub fn record_rejection(&mut self, stage: &str, revision: String, note: &str) -> Result<()> {
        self.stages
            .get_mut(stage)
            .context("missing stage")?
            .rejections
            .insert(revision, note.into());
        self.version = 7;
        Ok(())
    }
    pub fn new(workflow: Option<String>) -> Result<Self> {
        Ok(Self {
            version: 3,
            identity: Some(new_identity()?),
            sequence: 0,
            workflow,
            scoped_input_default: Some(crate::input_scope::Format::default()),
            origin: None,
            update: None,
            recurring_start: None,
            batch_review: false,
            last_batch: None,
            review_presentations: BTreeMap::new(),
            stages: BTreeMap::new(),
            review_requirements: BTreeMap::new(),
            switch_offer: None,
            switch_receipt: None,
        })
    }

    pub fn set_recurring_start(&mut self, start: RecurringStart) {
        self.version = 6;
        self.recurring_start = Some(start);
    }

    pub fn set_update(&mut self, update: RunUpdate) {
        self.version = update.state_version();
        self.update = Some(update);
    }

    pub fn upgrade(&mut self) -> Result<bool> {
        if self.version >= 3 {
            return Ok(false);
        }
        if self.version >= 2 {
            self.version = 3;
            return Ok(true);
        }
        for (stage, entry) in &mut self.stages {
            entry.order = entry.ordered_ids()?;
            if let Some(decision) = entry
                .current()
                .and_then(|(_, revision)| revision.decision.as_ref())
            {
                self.review_requirements
                    .insert(stage.clone(), decision.clone());
            }
        }
        self.identity = Some(new_identity()?);
        self.version = 3;
        Ok(true)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.version, 1..=7),
            "unsupported run state version"
        );
        if self.version >= 2 {
            ensure!(
                self.identity.as_deref().is_some_and(is_hash),
                "missing or invalid managed run identity"
            );
        }
        if let Some(origin) = &self.origin {
            crate::workspace::slug(&origin.run)?;
            ensure!(
                is_hash(&origin.identity) && is_hash(&origin.snapshot),
                "invalid fork origin"
            );
        }
        let expected_version = if let Some(start) = &self.recurring_start {
            crate::workspace::slug(&start.task)?;
            ensure!(
                self.update.is_none() && is_hash(&start.request),
                "invalid recurring start receipt"
            );
            validate_input_hashes(&start.submitted)?;
            6
        } else {
            self.update.as_ref().map_or(3, RunUpdate::state_version)
        };
        let expected_version = if self
            .stages
            .values()
            .any(|entry| !entry.rejections.is_empty())
        {
            7
        } else {
            expected_version
        };
        ensure!(
            self.version.max(3) == expected_version,
            "managed state version does not match its records"
        );
        if let Some(update) = &self.update {
            crate::workspace::slug(&update.from)?;
            ensure!(is_hash(&update.identity), "invalid update origin");
            for path in &update.inputs {
                crate::workspace::validate_ref(path)?;
                ensure!(path.starts_with("inputs/"), "invalid update input {path}");
            }
            ensure!(
                update.baseline.values().all(|id| is_hash(id)),
                "invalid update baseline"
            );
            if let Some(hashes) = &update.submission {
                validate_input_hashes(hashes)?;
            }
            if let Some(presentation) = &update.presentation {
                presentation.validate(self, update)?;
            }
            for (stage, reuse) in &update.reused {
                ensure!(
                    is_hash(&reuse.from)
                        && self
                            .stages
                            .get(stage)
                            .is_some_and(|entry| entry.revisions.contains_key(&reuse.revision)),
                    "invalid reused revision for {stage}"
                );
            }
        }
        ensure!(
            self.last_batch.as_deref().is_none_or(is_hash),
            "invalid review batch receipt"
        );
        for (token, stages) in &self.review_presentations {
            ensure!(
                !stages.is_empty() && self.batch_token(stages)? == *token,
                "invalid batch presentation"
            );
            for (stage, revision) in stages {
                ensure!(
                    self.stages
                        .get(stage)
                        .is_some_and(|entry| entry.revisions.contains_key(revision)),
                    "batch presentation refers to a missing revision"
                );
            }
        }
        let mut attempts = BTreeSet::new();
        for (stage, entry) in &self.stages {
            if let Some(head) = &entry.head {
                ensure!(
                    entry.revisions.contains_key(head),
                    "{stage} has an unknown current revision"
                );
            } else {
                ensure!(
                    entry.revisions.is_empty(),
                    "{stage} has revisions without a current record"
                );
            }
            if self.version >= 2 {
                ensure!(
                    entry.head.as_ref() == entry.order.last()
                        && entry.order.iter().collect::<BTreeSet<_>>()
                            == entry.revisions.keys().collect()
                        && entry.order.len() == entry.revisions.len(),
                    "{stage} has inconsistent archive order"
                );
                let mut parent = None;
                for id in &entry.order {
                    ensure!(
                        revision_id(parent, &entry.revisions[id])? == *id,
                        "{stage} has an invalid archived revision {id}"
                    );
                    parent = Some(id.as_str());
                }
            }
            for id in &entry.approvals {
                ensure!(
                    entry
                        .revisions
                        .get(id)
                        .is_some_and(|revision| revision.decision.is_some()),
                    "{stage} has an approval without a matching review"
                );
            }
            for (id, note) in &entry.rejections {
                ensure!(
                    entry
                        .revisions
                        .get(id)
                        .is_some_and(|revision| revision.decision.is_some())
                        && !entry.approvals.contains(id)
                        && crate::review::valid_note(note),
                    "{stage} has an invalid review rejection"
                );
            }
            for id in entry
                .attempt
                .iter()
                .map(|a| a.id)
                .chain(entry.last_completed_attempt)
            {
                ensure!(
                    id > 0 && id <= self.sequence && attempts.insert(id),
                    "{stage} has an invalid attempt ID"
                );
            }
            for revision in entry.revisions.values() {
                ensure!(
                    !revision.outputs.is_empty()
                        && revision.outputs.values().all(|digest| is_hash(digest)),
                    "{stage} has an invalid output archive"
                );
            }
        }
        Ok(())
    }

    pub fn review_token(&self, stage: &str, revision: &str) -> Option<String> {
        self.identity.as_ref().map(|identity| {
            // Identity and revision are fixed-length hexadecimal. Stage names
            // cannot contain the separator.
            hash(format!("review/{identity}/{stage}/{revision}").as_bytes())
        })
    }

    pub fn batch_token(&self, stages: &ReviewSet) -> Result<String> {
        let identity = self
            .identity
            .as_deref()
            .context("missing review identity")?;
        batch_token(identity, stages)
    }

    pub fn verify_archives(&self, run: &Path, all: bool) -> Result<()> {
        let mut checked = BTreeSet::new();
        for (stage, entry) in &self.stages {
            for (id, revision) in &entry.revisions {
                if all || entry.head.as_ref() == Some(id) {
                    for (name, digest) in &revision.outputs {
                        if checked.insert(digest) {
                            verified_blob(run, digest).with_context(|| format!("archive for {stage}, {name}; restore the run from a complete copy"))?;
                        }
                    }
                }
            }
        }
        if all
            && let Some(presentation) = self
                .update
                .as_ref()
                .and_then(|update| update.presentation.as_ref())
        {
            for digest in presentation.digests() {
                if checked.insert(digest) {
                    verified_blob(run, digest)
                        .context("retained review archive; restore the run from a complete copy")?;
                }
            }
        }
        Ok(())
    }
}

fn new_identity() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow::anyhow!("create run identity: {err}"))?;
    Ok(hash(&bytes))
}

pub fn lock_file(path: &Path, subject: &str) -> Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {
                ensure!(
                    Instant::now() < deadline,
                    "{subject} is busy; wait for its other AnyDock command and retry"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(err).with_context(|| format!("lock {}", path.display())),
        }
    }
    Ok(lock)
}

pub fn persist_replace(mut temp: NamedTempFile, path: &Path) -> io::Result<()> {
    // Windows scanners and editors can hold a replacement open briefly.
    let mut delays = [20, 40, 80, 160].into_iter();
    loop {
        match temp.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                let transient =
                    cfg!(windows) && matches!(error.error.raw_os_error(), Some(5 | 32 | 33));
                let Some(delay) = delays.next().filter(|_| transient) else {
                    return Err(error.error);
                };
                temp = error.file;
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
    }
}

pub struct LockedRun {
    state_path: PathBuf,
    run: PathBuf,
    _lock: File,
}

impl LockedRun {
    pub fn acquire(run: &Path, initialize: bool) -> Result<Option<Self>> {
        let meta = directory(run, ".anydock")?;
        if initialize {
            fs::create_dir_all(&meta).with_context(|| format!("create {}", meta.display()))?;
        } else if !meta.try_exists()? {
            return Ok(None);
        }
        let path = file(run, ".anydock/lock")?;
        let lock = lock_file(&path, "run")?;
        // Resolve the replaceable file only after acquiring the lock on Windows.
        let state_path = file(run, ".anydock/state.json")?;
        Ok(Some(Self {
            state_path,
            run: run.to_owned(),
            _lock: lock,
        }))
    }

    pub fn inspect(run: &Path) -> Result<Option<State>> {
        read_state(run, &file(run, ".anydock/state.json")?)
    }

    pub fn read(&self) -> Result<Option<State>> {
        read_state(&self.run, &self.state_path)
    }

    pub fn ensure_registered_marker(&self) -> Result<()> {
        let path = file(&self.run, ".anydock/registered")?;
        if !path.try_exists()? {
            let mut temp =
                NamedTempFile::new_in(path.parent().context("missing metadata folder")?)?;
            temp.write_all(b"anydock-managed\n")?;
            temp.as_file().sync_all()?;
            temp.persist_noclobber(&path)
                .context("record managed run marker")?;
            sync_dir(path.parent().context("missing metadata folder")?)?;
        }
        Ok(())
    }

    pub fn save(&self, state: &State) -> Result<()> {
        let folder = self
            .state_path
            .parent()
            .context("state directory is missing")?;
        let mut temp = NamedTempFile::new_in(folder)?;
        write_state(temp.as_file_mut(), state)?;
        fault("before-state-replace");
        persist_replace(temp, &self.state_path)
            .with_context(|| format!("replace {}", self.state_path.display()))?;
        sync_dir(folder)?;
        fault("after-state-replace");
        Ok(())
    }
}

fn write_state(file: &mut File, state: &State) -> Result<()> {
    serde_json::to_writer_pretty(&mut *file, state)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn read_state(run: &Path, path: &Path) -> Result<Option<State>> {
    let Some(bytes) = optional(fs::read(path))? else {
        ensure!(
            !(file(run, ".anydock/registered")?.try_exists()?
                || directory(run, ".anydock/blobs")?.try_exists()?),
            "missing .anydock/state.json for a managed run; restore the state and archives from a complete copy"
        );
        return Ok(None);
    };
    let state: State = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "read state {}; restore from a complete copy if damaged",
            path.display()
        )
    })?;
    state.validate()?;
    Ok(Some(state))
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn fingerprint(mut reader: impl io::Read) -> io::Result<String> {
    let mut digest = Sha256::new();
    io::copy(&mut reader, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

pub fn live_hash(run: &Path, name: &str) -> Result<Option<String>> {
    let path = file(run, name)?;
    optional(File::open(&path))?
        .map(fingerprint)
        .transpose()
        .with_context(|| format!("read {}", path.display()))
}

pub fn blob_path(run: &Path, digest: &str) -> Result<PathBuf> {
    ensure!(is_hash(digest), "invalid archive digest");
    file(run, &format!(".anydock/blobs/{digest}"))
}

pub fn verified_blob(run: &Path, digest: &str) -> Result<PathBuf> {
    let path = blob_path(run, digest)?;
    let blob =
        optional(File::open(&path))?.with_context(|| format!("missing .anydock/blobs/{digest}"))?;
    ensure!(
        fingerprint(blob)? == digest,
        "archive content does not match .anydock/blobs/{digest}"
    );
    Ok(path)
}

pub fn archive(run: &Path, outputs: &Hashes) -> Result<()> {
    archive_outputs(run, outputs, false)
}

pub fn archive_staged(run: &Path, outputs: &Hashes) -> Result<()> {
    archive_outputs(run, outputs, true)
}

fn archive_outputs(run: &Path, outputs: &Hashes, unpublished: bool) -> Result<()> {
    let folder = directory(run, ".anydock/blobs")?;
    fs::create_dir_all(&folder)?;
    for (name, digest) in outputs {
        let blob = file(run, &format!(".anydock/blobs/{digest}"))?;
        let source = file(run, name)?;
        let exists = blob.try_exists()?;
        if unpublished && !exists {
            copy_exact(&source, &blob, digest)?;
            continue;
        }
        let mut source = File::open(source).with_context(|| format!("open {name}"))?;
        if exists {
            verified_blob(run, digest)?;
            ensure!(
                fingerprint(&mut source)? == *digest,
                "{name} changed while being archived; inspect status and retry"
            );
            continue;
        }
        let mut temp = NamedTempFile::new_in(&folder)?;
        io::copy(&mut source, &mut temp)?;
        ensure!(
            fingerprint(File::open(temp.path())?)? == *digest,
            "{name} changed while being archived; inspect status and retry"
        );
        temp.as_file().sync_all()?;
        temp.persist_noclobber(blob)
            .with_context(|| format!("archive {name}"))?;
    }
    sync_dir(&folder)?;
    fault("after-archive");
    Ok(())
}

// The integration tests use the real binary and interrupt at commit boundaries.
// These hooks are absent from release builds.
pub fn fault(point: &str) {
    if cfg!(debug_assertions) && std::env::var("ANYDOCK_TEST_STOP").as_deref() == Ok(point) {
        eprintln!("interrupted at {point}");
        std::process::exit(71);
    }
}

pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn copy_exact(source: &Path, dest: &Path, digest: &str) -> Result<()> {
    let parent = dest.parent().context("missing destination directory")?;
    fs::create_dir_all(parent)?;
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dest)?;
    io::copy(&mut File::open(source)?, &mut out)?;
    out.sync_all()?;
    ensure!(
        fingerprint(File::open(dest)?)? == digest,
        "file changed while copying {}",
        source.display()
    );
    sync_dir(parent)
}

pub fn publish_run(staging: &TempDir, target: &Path, id: &str, state: &State) -> Result<()> {
    let staged = staging.path();
    let metadata = directory(staged, ".anydock")?;
    fs::create_dir_all(&metadata)?;
    // This directory becomes visible to run commands only at the final rename.
    // State and archives in an installed run still use atomic replacement.
    fault("before-state-replace");
    write_state(
        &mut File::create_new(file(staged, ".anydock/state.json")?)?,
        state,
    )?;
    fault("after-state-replace");
    let mut marker = File::create_new(file(staged, ".anydock/registered")?)?;
    marker.write_all(b"anydock-managed\n")?;
    marker.sync_all()?;
    drop(marker);
    sync_dir(&metadata)?;
    sync_dir(staged)?;
    let parent = target.parent().context("missing runs folder")?;
    fs::create_dir_all(parent)?;
    ensure!(!target.try_exists()?, "run {id} already exists");
    fs::rename(staged, target).with_context(|| format!("register run {id}"))?;
    sync_dir(parent)
}
