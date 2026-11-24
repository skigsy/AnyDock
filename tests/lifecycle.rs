use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Space(tempfile::TempDir);

impl Space {
    fn new() -> Self {
        let space = Self(tempfile::tempdir().unwrap());
        space.write("anydock.version", "1\n");
        space
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn brief(&self, run: &str) {
        self.write(&format!("runs/{run}/brief.md"), "Authorized test work.\n");
    }

    fn stage(&self, run: &str, stage: &str, inputs: &[&str], review: Option<&str>) -> String {
        let output = format!("stages/{stage}/outputs/handoff.md");
        let fields = json!({ "inputs": inputs, "outputs": [&output], "review": review });
        self.write(
            &format!("runs/{run}/stages/{stage}/stage.md"),
            contract(&fields),
        );
        output
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anydock"));
        command.current_dir(self.0.path()).args(args);
        command
    }

    fn ok(&self, args: &[&str]) -> String {
        good(self.command(args).output().unwrap())
    }

    fn fails(&self, args: &[&str], message: &str) {
        let result = self.command(args).output().unwrap();
        let diagnostic = String::from_utf8(result.stderr).unwrap();
        assert!(
            !result.status.success() && diagnostic.contains(message),
            "{args:?}: {diagnostic}"
        );
    }

    fn state(&self, run: &str) -> Value {
        serde_json::from_slice(
            &fs::read(self.path(&format!("runs/{run}/.anydock/state.json"))).unwrap(),
        )
        .unwrap()
    }
}

fn contract(fields: &Value) -> String {
    format!(
        "# Stage\n\n\x60\x60\x60anydock\n{fields}\n\x60\x60\x60\n\n## Work\n\nWrite the test handoff.\n"
    )
}

fn good(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn revision(status: &str) -> &str {
    status
        .lines()
        .find_map(|line| line.trim().strip_prefix("Revision: "))
        .unwrap()
}

#[test]
fn blank_is_valid_idle_and_git_free() {
    let space = Space::new();
    assert!(space.ok(&["check"]).contains("0 runs, 0 workflows"));
    assert!(space.ok(&["status"]).contains("idle"));
    assert_eq!(fs::read_dir(space.0.path()).unwrap().count(), 1);
    space.write("anydock.version", "2");
    space.fails(&["check"], "unsupported workspace version");
}

#[test]
fn one_off_checks_files_and_waits_for_the_exact_review() {
    let space = Space::new();
    space.brief("work");
    let first = space.stage(
        "work",
        "01-prepare",
        &["inputs/source.txt"],
        Some("Accept the handoff?"),
    );
    let last = space.stage("work", "02-finish", &[&first], None);
    space.ok(&["check"]);
    space.ok(&["start", "work"]);
    assert!(space.ok(&["start", "work"]).contains("already registered"));
    assert!(
        space
            .ok(&["status", "work"])
            .contains("missing input: inputs/source.txt")
    );
    space.fails(&["next", "work"], "missing input");
    space.write("runs/work/inputs/source.txt", "source");
    assert!(space.ok(&["next", "work"]).contains("attempt 1"));
    assert!(
        space
            .ok(&["status", "work"])
            .contains("Next: resume 01-prepare")
    );
    assert!(space.ok(&["next", "work"]).contains("attempt 1"));
    space.fails(
        &["complete", "work", "01-prepare", "--attempt", "1"],
        "missing output",
    );
    space.write(&format!("runs/work/{first}"), "review bytes");
    let pending = space.ok(&["complete", "work", "01-prepare", "--attempt", "1"]);
    assert!(pending.contains(&format!("Review file: runs/work/{first}")));
    assert!(pending.contains("Decision: Accept the handoff?"));
    assert!(pending.contains("Gates: 02-finish"));
    let id = revision(&pending);
    space.fails(&["next", "work"], "awaits human review");
    space.fails(
        &["approve", "work", "01-prepare", "--revision", "unseen"],
        "revision changed",
    );
    space.fails(
        &["complete", "work", "02-finish", "--attempt", "2"],
        "first unresolved",
    );
    space.ok(&["approve", "work", "01-prepare", "--revision", id]);
    assert!(
        space
            .ok(&["approve", "work", "01-prepare", "--revision", id])
            .contains("already approved")
    );
    assert!(
        space
            .ok(&["next", "work"])
            .contains("02-finish | attempt 2")
    );
    space.write(&format!("runs/work/{last}"), "final bytes");
    assert!(
        space
            .ok(&["complete", "work", "02-finish", "--attempt", "2"])
            .contains("Run work is complete")
    );
}

#[test]
fn editing_a_reviewed_handoff_archives_and_invalidates_only_its_consumers() {
    let space = Space::new();
    space.brief("work");
    let upstream = space.stage("work", "01-source", &[], Some("Accept these bytes?"));
    let dependent = space.stage("work", "02-copy", &[&upstream], None);
    let independent = space.stage("work", "03-other", &[], None);
    let transitive = space.stage("work", "04-final", &[&dependent], None);
    space.ok(&["start", "work"]);
    let mut original_token = String::new();
    for (id, attempt, output) in [
        ("01-source", "1", &upstream),
        ("02-copy", "2", &dependent),
        ("03-other", "3", &independent),
        ("04-final", "4", &transitive),
    ] {
        space.ok(&["next", "work"]);
        space.write(&format!("runs/work/{output}"), id);
        let status = space.ok(&["complete", "work", id, "--attempt", attempt]);
        if id == "01-source" {
            original_token = revision(&status).to_owned();
            space.ok(&["approve", "work", id, "--revision", &original_token]);
        }
    }
    let original = space.state("work")["stages"]["01-source"]["head"]
        .as_str()
        .unwrap()
        .to_owned();
    space.write(&format!("runs/work/{upstream}"), "human revision");
    let status = space.ok(&["status", "work"]);
    let edited = revision(&status).to_owned();
    assert_ne!(original_token, edited);
    let edited_id = space.state("work")["stages"]["01-source"]["head"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(status.contains("02-copy | stale") && status.contains("04-final | stale"));
    assert!(status.contains("03-other | done") && status.contains("01-source | awaiting review"));
    assert!(
        space
            .ok(&["status", "work"])
            .contains(&format!("output edited: {upstream}"))
    );
    space.fails(
        &[
            "approve",
            "work",
            "01-source",
            "--revision",
            &original_token,
        ],
        "revision changed",
    );
    space.write(&format!("runs/work/{upstream}"), "01-source");
    let reverted = space.ok(&["status", "work"]);
    assert_ne!(revision(&reverted), original_token);
    assert_ne!(revision(&reverted), edited);
    space.ok(&[
        "approve",
        "work",
        "01-source",
        "--revision",
        revision(&reverted),
    ]);
    let status = space.ok(&["status", "work"]);
    assert!(status.contains(&format!("upstream revision changed: {upstream}")));
    assert!(status.contains("04-final | stale"));
    let state = space.state("work");
    let blob = state["stages"]["01-source"]["revisions"][&edited_id]["outputs"][&upstream]
        .as_str()
        .unwrap();
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/work/.anydock/blobs/{blob}"))).unwrap(),
        "human revision"
    );
    assert!(
        space
            .ok(&["history", "work", "01-source"])
            .contains(&format!("{original} | approved"))
    );
    assert!(
        space
            .ok(&["next", "work", "--retry"])
            .contains("02-copy | attempt 5")
    );
}

#[test]
fn source_contract_and_brief_changes_fence_old_attempts() {
    let space = Space::new();
    space.brief("work");
    let handoff = space.stage("work", "01-write", &["inputs/source.txt"], None);
    space.write("runs/work/inputs/source.txt", "source");
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{handoff}"), "partial");
    space.write("runs/work/inputs/source.txt", "source changed");
    space.fails(
        &["complete", "work", "01-write", "--attempt", "1"],
        "file changed: inputs/source.txt",
    );
    space.fails(&["next", "work"], "use next --retry");
    assert!(space.ok(&["next", "work", "--retry"]).contains("attempt 2"));
    space.fails(
        &["complete", "work", "01-write", "--attempt", "1"],
        "cannot complete attempt 1",
    );
    let file = "runs/work/stages/01-write/stage.md";
    let old = fs::read_to_string(space.path(file)).unwrap();
    space.write(file, format!("{old}\nRevised work.\n"));
    space.write("runs/work/brief.md", "Revised authorized task");
    let status = space.ok(&["status", "work"]);
    assert!(status.contains("file changed: brief.md"));
    assert!(status.contains("file changed: stages/01-write/stage.md"));
    assert!(space.ok(&["next", "work", "--retry"]).contains("attempt 3"));
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/work/{handoff}"))).unwrap(),
        "partial"
    );
    space.write(&format!("runs/work/{handoff}"), "final");
    space.ok(&["complete", "work", "01-write", "--attempt", "3"]);
    assert!(
        space
            .ok(&["complete", "work", "01-write", "--attempt", "3"])
            .contains("already recorded")
    );
}

#[test]
fn concurrent_commands_share_an_attempt_and_commit_once() {
    let space = Space::new();
    space.brief("work");
    let output = space.stage("work", "01-write", &[], None);
    space.ok(&["start", "work"]);
    let concurrent = |args: &[&str]| {
        (0..8)
            .map(|_| {
                space
                    .command(args)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|child| good(child.wait_with_output().unwrap()))
            .collect::<Vec<_>>()
    };
    assert!(
        concurrent(&["next", "work"])
            .iter()
            .all(|s| s.contains("attempt 1"))
    );
    space.write(&format!("runs/work/{output}"), "handoff");
    assert!(
        concurrent(&["complete", "work", "01-write", "--attempt", "1"])
            .iter()
            .all(|s| s.contains("Run work is complete"))
    );
    let state = space.state("work");
    assert_eq!(state["sequence"], 1);
    assert_eq!(
        state["stages"]["01-write"]["revisions"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn lock_holder() {
    let Some(folder) = std::env::var_os("ANYDOCK_TEST_INTERRUPTED_RUN") else {
        return;
    };
    let folder = PathBuf::from(folder);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(folder.join("lock"))
        .unwrap();
    lock.lock().unwrap();
    fs::write(folder.join("incomplete.tmp"), "{").unwrap();
    loop {
        thread::park();
    }
}

#[test]
fn process_death_releases_the_lock_and_an_incomplete_temp_is_ignored() {
    let space = Space::new();
    space.brief("work");
    let handoff = space.stage("work", "01-write", &[], None);
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{handoff}"), "partial");
    let mut holder = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder"])
        .env(
            "ANYDOCK_TEST_INTERRUPTED_RUN",
            space.path("runs/work/.anydock"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !space.path("runs/work/.anydock/incomplete.tmp").exists() {
        if Instant::now() > deadline {
            let _ = holder.kill();
            let _ = holder.wait();
            panic!("lock holder did not start");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut waiter = space
        .command(&["next", "work"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(70));
    let waited = waiter.try_wait().unwrap().is_none();
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(waited, "next should wait for the lock");
    assert!(good(waiter.wait_with_output().unwrap()).contains("attempt 1"));
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/work/{handoff}"))).unwrap(),
        "partial"
    );
    space.ok(&["complete", "work", "01-write", "--attempt", "1"]);
}

#[test]
fn check_rejects_bad_paths_and_bad_graphs() {
    let space = Space::new();
    space.brief("work");
    let contract_path = "runs/work/stages/01-write/stage.md";
    for input in [
        "../secret",
        "/absolute",
        "inputs/../../secret",
        "inputs/a\\b",
        "inputs/report.txt:stream",
        "inputs/con.txt",
        "inputs/A.txt",
    ] {
        space.stage("work", "01-write", &[input], None);
        space.fails(&["check"], "invalid portable path");
    }
    space.stage(
        "work",
        "01-write",
        &["stages/02-later/outputs/handoff.md"],
        None,
    );
    space.stage("work", "02-later", &[], None);
    space.fails(&["check"], "declared by an earlier stage");
    space.stage("work", "01-write", &["inputs/a", "inputs/a/b"], None);
    space.fails(&["check"], "conflicting file paths");
    space.write(
        contract_path,
        contract(&json!({ "inputs": [], "outputs": ["inputs/a"], "review": null })),
    );
    space.fails(&["check"], "must write under");
    space.write(
        contract_path,
        contract(&json!({ "inputs": [], "outputs": ["stages/01-write/outputs/report.md"] })),
    );
    space.fails(&["check"], "missing field `review`");
    space.write(contract_path, "# Invalid contract\n");
    space.fails(&["check"], "missing anydock fence");
    space.stage("work", "01-write", &[], None);
    space.stage("work", "01-dupe", &[], None);
    space.fails(&["check"], "duplicate stage number");
    space.fails(&["start", ".."], "invalid portable path");
}

#[test]
fn rejects_a_directory_redirect_outside_the_run() {
    let space = Space::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("source.txt"), "outside").unwrap();
    space.brief("work");
    space.stage("work", "01-write", &["inputs/source.txt"], None);
    let link = space.path("runs").join("work").join("inputs");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    {
        good(
            Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&link)
                .arg(outside.path())
                .output()
                .unwrap(),
        );
    }
    space.fails(&["check"], "path redirects");
    space.fails(&["start", "work"], "path redirects");
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
}

#[test]
fn reused_workflows_copy_definitions_and_runs_are_isolated() {
    let space = Space::new();
    space.brief("alpha");
    let output = space.stage("alpha", "01-write", &["inputs/source.txt"], None);
    space.write("runs/alpha/inputs/source.txt", "first input");
    space.ok(&["start", "alpha"]);
    space.ok(&["next", "alpha"]);
    space.write(&format!("runs/alpha/{output}"), "first result");
    space.ok(&["complete", "alpha", "01-write", "--attempt", "1"]);
    space.ok(&["save-workflow", "alpha", "definitions"]);
    space.fails(&["save-workflow", "alpha", "definitions"], "already exists");
    assert!(!space.path("workflows/definitions/brief.md").exists());
    assert!(
        !space
            .path(&format!("workflows/definitions/{output}"))
            .exists()
    );
    space.brief("beta");
    space.ok(&["start", "beta", "--workflow", "definitions"]);
    assert!(
        space
            .ok(&["start", "beta", "--workflow", "definitions"])
            .contains("already registered")
    );
    assert!(!space.path(&format!("runs/beta/{output}")).exists());
    assert!(
        space
            .ok(&["status", "beta"])
            .contains("missing input: inputs/source.txt")
    );
    space.write("runs/beta/inputs/source.txt", "second input");
    assert!(space.ok(&["next", "beta"]).contains("attempt 1"));
    space.write(&format!("runs/beta/{output}"), "second result");
    space.ok(&["complete", "beta", "01-write", "--attempt", "1"]);
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/alpha/{output}"))).unwrap(),
        "first result"
    );
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/beta/{output}"))).unwrap(),
        "second result"
    );
    assert!(space.ok(&["check"]).contains("2 runs, 1 workflows"));
}

#[test]
fn copied_runs_keep_their_approval_and_partial_copies_are_identified() {
    let space = Space::new();
    let mut tokens = Vec::new();
    for run in ["alpha", "beta"] {
        space.brief(run);
        let out = space.stage(run, "01-draft", &[], Some("Accept the draft?"));
        space.ok(&["start", run]);
        space.ok(&["next", run]);
        space.write(&format!("runs/{run}/{out}"), "same handoff");
        let status = space.ok(&["complete", run, "01-draft", "--attempt", "1"]);
        tokens.push(revision(&status).to_owned());
    }
    assert_ne!(tokens[0], tokens[1]);
    space.fails(
        &["approve", "beta", "01-draft", "--revision", &tokens[0]],
        "revision changed",
    );
    let alpha = space.path("runs/alpha");
    fs::rename(&alpha, space.path("runs/restored")).unwrap();
    fs::remove_file(space.path("runs/restored/.anydock/lock")).unwrap();
    assert_eq!(revision(&space.ok(&["status", "restored"])), tokens[0]);
    space.ok(&["approve", "restored", "01-draft", "--revision", &tokens[0]]);
    fs::remove_file(space.path("runs/restored/.anydock/state.json")).unwrap();
    space.fails(&["start", "restored"], "missing .anydock/state.json");
    let result = space.command(&["status"]).output().unwrap();
    let report = String::from_utf8(result.stdout).unwrap();
    assert!(!result.status.success());
    assert!(report.contains("Run beta") && report.contains("Run restored | unavailable"));
}

#[test]
fn review_policy_cannot_be_removed_implicitly() {
    let space = Space::new();
    space.brief("work");
    space.stage("work", "01-draft", &[], Some("Accept the draft?"));
    space.stage("work", "02-send", &[], None);
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.stage("work", "01-draft", &[], None);
    space.fails(&["next", "work", "--retry"], "pending review policy");
    fs::rename(space.path("runs/work/stages/01-draft"), space.path("prior")).unwrap();
    space.fails(&["next", "work"], "pending review policy");
    let report = space.ok(&["status", "work"]);
    assert!(report.contains("01-draft previously required: Accept the draft?"));
    let token = report
        .lines()
        .find_map(|s| s.split_once(" --change ").map(|(_, v)| v))
        .unwrap();
    space.fails(
        &[
            "authorize-review-change",
            "work",
            "01-draft",
            "--change",
            "other",
        ],
        "no longer matches",
    );
    space.ok(&[
        "authorize-review-change",
        "work",
        "01-draft",
        "--change",
        token,
    ]);
    assert!(space.ok(&["next", "work"]).contains("02-send | attempt 2"));
}

#[test]
fn inspection_preserves_files_and_corrupt_archives_block_advancement() {
    let space = Space::new();
    space.brief("work");
    let out = space.stage("work", "01-draft", &[], Some("Accept?"));
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{out}"), "first");
    space.ok(&["complete", "work", "01-draft", "--attempt", "1"]);
    let before = space.state("work");
    space.write(&format!("runs/work/{out}"), "edited");
    let report = space.ok(&["status", "work", "--inspect", "--diff"]);
    assert!(report.contains("output edited") && !report.contains("Revision:"));
    assert!(report.contains("-first") && report.contains("+edited"));
    assert_eq!(space.state("work"), before);
    let status = space.ok(&["status", "work"]);
    let state = space.state("work");
    let entry = &state["stages"]["01-draft"];
    let id = entry["head"].as_str().unwrap();
    let digest = entry["revisions"][id]["outputs"][&out].as_str().unwrap();
    space.write(&format!("runs/work/.anydock/blobs/{digest}"), "damaged");
    space.fails(
        &[
            "approve",
            "work",
            "01-draft",
            "--revision",
            revision(&status),
        ],
        "archive content does not match",
    );
    space.fails(&["check"], "restore the run from a complete copy");
}

#[test]
fn legacy_approval_and_order_survive_migration() {
    let space = Space::new();
    space.brief("legacy");
    let out = space.stage("legacy", "01-draft", &[], Some("Accept?"));
    space.ok(&["start", "legacy"]);
    space.ok(&["next", "legacy"]);
    space.write(&format!("runs/legacy/{out}"), "first");
    space.ok(&["complete", "legacy", "01-draft", "--attempt", "1"]);
    space.write(&format!("runs/legacy/{out}"), "updated");
    let status = space.ok(&["status", "legacy"]);
    space.ok(&[
        "approve",
        "legacy",
        "01-draft",
        "--revision",
        revision(&status),
    ]);
    let mut state = space.state("legacy");
    state["version"] = json!(1);
    for key in ["identity", "review_requirements"] {
        state.as_object_mut().unwrap().remove(key);
    }
    state["stages"]["01-draft"]
        .as_object_mut()
        .unwrap()
        .remove("order");
    space.write(
        "runs/legacy/.anydock/state.json",
        serde_json::to_vec(&state).unwrap(),
    );
    assert!(
        space
            .ok(&["status", "legacy"])
            .contains("Run legacy is complete")
    );
    let upgraded = space.state("legacy");
    assert_eq!(upgraded["version"], 3);
    assert_eq!(
        upgraded["stages"]["01-draft"]["order"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        upgraded["stages"]["01-draft"]["approvals"],
        state["stages"]["01-draft"]["approvals"]
    );
}

#[test]
fn real_commit_boundaries_allow_retry() {
    if !cfg!(debug_assertions) {
        return;
    }
    for point in [
        "after-archive",
        "before-state-replace",
        "after-state-replace",
    ] {
        let space = Space::new();
        space.brief("work");
        let out = space.stage("work", "01-draft", &[], None);
        space.ok(&["start", "work"]);
        space.ok(&["next", "work"]);
        space.write(&format!("runs/work/{out}"), "handoff");
        let args = ["complete", "work", "01-draft", "--attempt", "1"];
        let failed = space
            .command(&args)
            .env("ANYDOCK_TEST_STOP", point)
            .output()
            .unwrap();
        assert_eq!(
            failed.status.code(),
            Some(71),
            "{point}: {:?}",
            failed.stderr
        );
        assert!(space.ok(&args).contains("Run work is complete"));
        space.ok(&["check"]);
        let state = space.state("work");
        assert_eq!(
            state["stages"]["01-draft"]["revisions"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn saved_references_are_pinned_and_only_selected_sections_invalidate_work() {
    let space = Space::new();
    space.brief("source");
    let output = space.stage("source", "01-write", &[], None);
    let plain = space.stage("source", "02-other", &[], None);
    let path = "references/guide.md";
    let fields = json!({
        "inputs": [], "outputs": [&output], "review": null,
        "references": [{"path": path, "section": "Voice"}]
    });
    space.write("runs/source/stages/01-write/stage.md", contract(&fields));
    let v1 = "# Voice\n\nUse plain English.\n\n# Layout\n\nUse bullets.\n";
    space.write(&format!("runs/source/{path}"), v1);
    space.ok(&["save-workflow", "source", "updates"]);
    space.brief("alpha");
    space.ok(&["start", "alpha", "--workflow", "updates"]);
    let next = space.ok(&["next", "alpha", "--context"]);
    assert!(next.contains("references/guide.md (heading \"Voice\")"));
    assert!(next.contains("Use plain English.") && !next.contains("Use bullets."));
    space.write(&format!("runs/alpha/{output}"), "stage one");
    space.ok(&["complete", "alpha", "01-write", "--attempt", "1"]);
    space.ok(&["next", "alpha"]);
    space.write(&format!("runs/alpha/{plain}"), "unrelated");
    space.ok(&["complete", "alpha", "02-other", "--attempt", "2"]);

    let shared = format!("workflows/updates/{path}");
    space.write(&shared, v1.replace("Use bullets.", "Use numbered lists."));
    space.ok(&["adopt-reference", "alpha", path]);
    assert!(
        space
            .ok(&["status", "alpha"])
            .contains("Run alpha is complete")
    );

    let v2 = v1.replace("Use plain English.", "Use headline case.");
    space.write(&shared, &v2);
    assert!(
        space
            .ok(&["status", "alpha"])
            .contains("Run alpha is complete")
    );
    space.brief("beta");
    space.ok(&["start", "beta", "--workflow", "updates"]);
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/beta/{path}"))).unwrap(),
        v2
    );
    space.ok(&["adopt-reference", "alpha", path]);
    let status = space.ok(&["status", "alpha"]);
    assert!(status.contains("01-write | stale") && status.contains("02-other | done"));
    assert!(status.contains("file changed: references/guide.md"));
    fs::rename(space.path("runs/alpha"), space.path("runs/moved")).unwrap();
    assert!(space.ok(&["status", "moved"]).contains("01-write | stale"));
    space.fails(
        &["adopt-reference", "moved", "inputs/secret.txt"],
        "not a declared",
    );
}

#[test]
fn diff_and_restore_use_verified_history_without_overwriting_live_work() {
    let space = Space::new();
    space.brief("work");
    let out = space.stage("work", "01-draft", &[], Some("Accept?"));
    let live = format!("runs/work/{out}");
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&live, "first revision\n");
    let pending = space.ok(&["complete", "work", "01-draft", "--attempt", "1"]);
    let first_id = space.state("work")["stages"]["01-draft"]["head"]
        .as_str()
        .unwrap()
        .to_owned();
    space.ok(&[
        "approve",
        "work",
        "01-draft",
        "--revision",
        revision(&pending),
    ]);
    space.write(&live, "edited revision\n");
    space.ok(&["status", "work"]);
    let difference = space.ok(&["diff", "work", "01-draft"]);
    assert!(difference.contains("-first revision") && difference.contains("+edited revision"));
    let args = [
        "restore",
        "work",
        "01-draft",
        "--revision",
        &first_id,
        "--output",
        &out,
        "--to",
        "recovered/draft.md",
    ];
    space.ok(&args);
    assert_eq!(
        fs::read_to_string(space.path("runs/work/recovered/draft.md")).unwrap(),
        "first revision\n"
    );
    assert_eq!(
        fs::read_to_string(space.path(&live)).unwrap(),
        "edited revision\n"
    );
    space.fails(&args, "write recovered copy");
}

#[cfg(windows)]
#[test]
fn state_replacement_can_be_retried_after_windows_sharing_failure() {
    use std::os::windows::fs::OpenOptionsExt;
    let space = Space::new();
    space.brief("work");
    let out = space.stage("work", "01-draft", &[], None);
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{out}"), "new handoff");
    let state_file = space.path("runs/work/.anydock/state.json");
    // Allow reads and writes but deny rename/delete while this handle is held.
    let handle = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(state_file)
        .unwrap();
    let args = ["complete", "work", "01-draft", "--attempt", "1"];
    space.fails(&args, "replace");
    drop(handle);
    assert!(space.ok(&args).contains("Run work is complete"));
}

#[test]
fn moving_to_another_harness_preserves_the_work_and_fences_the_old_attempt() {
    let space = Space::new();
    space.brief("work");
    let output = space.stage(
        "work",
        "01-draft",
        &["inputs/data.txt"],
        Some("Use this draft?"),
    );
    space.write("runs/work/inputs/data.txt", "source");
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{output}"), "partly written");
    space.ok(&[
        "checkpoint",
        "work",
        "--attempt",
        "1",
        "--note",
        "Finish the second paragraph.",
    ]);
    let before = space.state("work");
    let first: Value = serde_json::from_str(&space.ok(&["handoff", "work", "--json"])).unwrap();
    assert_eq!(first["schema"], 1);
    assert_eq!(first["instructions"], "AGENTS.md");
    assert_eq!(first["current"]["attempt"], 1);
    assert_eq!(
        first["current"]["checkpoint"],
        "Finish the second paragraph."
    );
    assert_eq!(
        first["current"]["outputs"][0],
        format!("runs/work/{output}")
    );
    assert_eq!(space.state("work"), before);
    assert!(
        space
            .ok(&["takeover", "work", "--attempt", "1"])
            .contains("Replaced attempt 1 with 2")
    );
    assert!(
        space
            .ok(&["handoff", "work"])
            .contains("Finish the second paragraph.")
    );
    space.fails(
        &["checkpoint", "work", "--attempt", "1", "--note", "outdated"],
        "no longer active",
    );
    space.fails(&["takeover", "work", "--attempt", "1"], "no longer active");
    space.fails(
        &["complete", "work", "01-draft", "--attempt", "1"],
        "cannot complete",
    );
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/work/{output}"))).unwrap(),
        "partly written"
    );
    space.write(&format!("runs/work/{output}"), "finished in second harness");
    space.ok(&["complete", "work", "01-draft", "--attempt", "2"]);
    let waiting: Value = serde_json::from_str(&space.ok(&["handoff", "work", "--json"])).unwrap();
    assert_eq!(waiting["current"]["state"], "awaiting review");
    assert_eq!(waiting["current"]["decision"], "Use this draft?");
    assert!(waiting["current"]["attempt"].is_null());
    assert!(!waiting.to_string().contains("--revision"));
}

#[test]
fn required_checks_fail_before_recording_and_when_a_reviewed_output_is_edited() {
    let space = Space::new();
    space.brief("work");
    space.write("runs/work/inputs/source.md", "Source.");
    let output = "stages/01-report/outputs";
    let csv = format!("{output}/rows.csv");
    let json_path = format!("{output}/summary.json");
    let md = format!("{output}/links.md");
    space.write(
        "runs/work/stages/01-report/stage.md",
        contract(&json!({
            "inputs": ["inputs/source.md"], "outputs": [&csv, &json_path, &md],
            "review": "Accept the report?",
            "checks": [
              {"kind":"csv-header","path":&csv,"columns":["name","total"]},
              {"kind":"json-keys","path":&json_path,"keys":["sum"]},
              {"kind":"markdown-links","path":&md},
              {"kind":"nonempty","path":&md}
            ]
        })),
    );
    space.ok(&["check"]);
    space.ok(&["start", "work"]);
    assert!(
        space
            .ok(&["next", "work"])
            .contains("Required check: csv-header")
    );
    space.write(&format!("runs/work/{csv}"), "name,incorrect\nx,1\n");
    space.write(&format!("runs/work/{json_path}"), r#"{"some":1}"#);
    space.write(
        &format!("runs/work/{md}"),
        "[Missing](../../../inputs/missing.md)",
    );
    let args = ["complete", "work", "01-report", "--attempt", "1"];
    let failed = space.command(&args).output().unwrap();
    assert!(!failed.status.success());
    let error = String::from_utf8(failed.stderr).unwrap();
    assert!(
        error.contains("expected columns")
            && error.contains("missing keys")
            && error.contains("missing linked file")
    );
    assert!(space.state("work")["stages"]["01-report"]["head"].is_null());
    space.write(&format!("runs/work/{csv}"), "name,total\nx,1\n");
    space.write(&format!("runs/work/{json_path}"), r#"{"sum":1}"#);
    space.write(
        &format!("runs/work/{md}"),
        "[Source](../../../inputs/source.md)",
    );
    let report = space.ok(&args);
    assert!(report.contains("Check passed: markdown-links"));
    let token = revision(&report).to_owned();
    space.ok(&["approve", "work", "01-report", "--revision", &token]);
    space.write(&format!("runs/work/{csv}"), "name,total\nx,1,invalid\n");
    let stale = space.ok(&["status", "work"]);
    assert!(stale.contains("01-report | stale") && stale.contains("check failed: csv-header"));
    space.fails(
        &["approve", "work", "01-report", "--revision", &token],
        "is stale",
    );
}

#[test]
fn forks_share_a_baseline_and_choose_only_confirmed_selected_outputs() {
    let space = Space::new();
    space.brief("source");
    let handoff = space.stage(
        "source",
        "01-write",
        &["inputs/data.txt"],
        Some("Use this version?"),
    );
    space.write("runs/source/inputs/data.txt", "pinned source");
    space.write("runs/source/inputs/unrelated.txt", "private unrelated file");
    space.ok(&["start", "source"]);
    space.ok(&["next", "source"]);
    space.write(&format!("runs/source/{handoff}"), "source working file");
    space.ok(&["fork", "source", "alpha"]);
    space.ok(&["fork", "source", "beta"]);
    space.fails(&["fork", "source", "alpha"], "already exists");
    assert!(!space.path(&format!("runs/alpha/{handoff}")).exists());
    assert!(!space.path("runs/alpha/inputs/unrelated.txt").exists());
    assert_eq!(
        fs::read_to_string(space.path("runs/alpha/inputs/data.txt")).unwrap(),
        "pinned source"
    );
    let mut candidates = Vec::new();
    for (run, text) in [("alpha", "Calm version\n"), ("beta", "Bold version\n")] {
        space.ok(&["next", run]);
        space.write(&format!("runs/{run}/{handoff}"), text);
        let report = space.ok(&["complete", run, "01-write", "--attempt", "1"]);
        candidates.push(revision(&report).to_owned());
    }
    let compare = space.ok(&["compare", "alpha", "beta"]);
    assert!(compare.contains("same fork starting files: yes"));
    assert!(
        compare.contains("awaiting review")
            && compare.contains("-Calm version")
            && compare.contains("+Bold version")
    );
    let selection = [
        "delivery",
        "alpha",
        "--name",
        "selected",
        "--include",
        &handoff,
    ];
    space.fails(&selection, "awaiting review");
    space.fails(
        &[
            "delivery",
            "alpha",
            "--name",
            "selected",
            "--include",
            "inputs/data.txt",
        ],
        "not a declared handoff",
    );
    space.ok(&["approve", "alpha", "01-write", "--revision", &candidates[0]]);
    assert!(space.ok(&["status", "beta"]).contains("awaiting review"));
    let preview = space.ok(&selection);
    assert!(!preview.contains("Use this version?"));
    assert!(!space.path("runs/alpha/deliveries").exists());
    let token = preview
        .lines()
        .find_map(|line| line.strip_prefix("Confirmation: "))
        .unwrap();
    let mut args = selection.to_vec();
    args.extend(["--confirm", token]);
    space.write(&format!("runs/alpha/{handoff}"), "unrecorded change");
    space.fails(&args, "awaiting review");
    space.write(&format!("runs/alpha/{handoff}"), "Calm version\n");
    space.ok(&args);
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/alpha/deliveries/selected/files/{handoff}")))
            .unwrap(),
        "Calm version\n"
    );
    assert!(
        !space
            .path("runs/alpha/deliveries/selected/files/inputs/data.txt")
            .exists()
    );
    space.fails(&args, "already exists");
}

#[test]
fn quick_begins_reviewed_work_with_only_the_explicit_inbox_inputs() {
    let space = Space::new();
    space.write("inbox/notes.md", "Known source facts.");
    space.write("inbox/unrelated.md", "Unrelated file.");
    let args = [
        "quick",
        "work",
        "--brief",
        "Write the handoff.",
        "--work",
        "Use the notes.",
        "--input",
        "inbox/notes.md",
        "--output",
        "handoff.md",
        "--review",
        "Approve?",
    ];
    let begin = space.ok(&args);
    assert!(begin.contains("01-work | attempt 1"));
    assert!(begin.contains("runs/work/inputs/notes.md"));
    assert!(!space.path("runs/work/inputs/unrelated.md").exists());
    space.fails(&args, "already exists");
    let context = space.ok(&["next", "work", "--context"]);
    assert!(context.contains("Known source facts.") && context.contains("Write the handoff."));
    let result = "runs/work/stages/01-work/outputs/handoff.md";
    space.write(result, "Verified handoff.");
    let gate = space.ok(&["complete", "work", "01-work", "--attempt", "1"]);
    assert!(gate.contains("awaiting review"));
    assert_eq!(
        fs::read_to_string(space.path("inbox/notes.md")).unwrap(),
        "Known source facts."
    );
}

fn offered_ticket(bootstrap: &str) -> &str {
    bootstrap
        .split_once(" --ticket ")
        .unwrap()
        .1
        .split_once('.')
        .unwrap()
        .0
}

#[test]
fn failed_launch_and_interrupted_claim_leave_the_old_attempt_available() {
    let space = Space::new();
    space.brief("work");
    let output = space.stage("work", "01-draft", &[], Some("Use this draft?"));
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    space.write(&format!("runs/work/{output}"), "partly written");
    space.ok(&[
        "checkpoint",
        "work",
        "--attempt",
        "1",
        "--note",
        "Finish the draft.",
    ]);
    space.fails(
        &[
            "switch",
            "work",
            "--attempt",
            "1",
            "--",
            "anydock-test-no-such-harness.exe",
            "{handoff}",
        ],
        "only changes the attempt after a claim",
    );
    space.fails(
        &[
            "switch",
            "work",
            "--attempt",
            "1",
            "--",
            env!("CARGO_BIN_EXE_anydock"),
            "--help",
            "{handoff}",
        ],
        "exited without a confirmed claim",
    );
    assert!(space.ok(&["next", "work"]).contains("attempt 1"));

    let old = space.ok(&["offer", "work", "--attempt", "1"]);
    let offer = space.ok(&["offer", "work", "--attempt", "1"]);
    space.fails(
        &["claim", "work", "--ticket", offered_ticket(&old)],
        "does not match",
    );
    let ticket = offered_ticket(&offer);
    assert!(!space.state("work").to_string().contains(ticket));
    let interrupted = space
        .command(&["claim", "work", "--ticket", ticket])
        .env("ANYDOCK_TEST_STOP", "before-state-replace")
        .output()
        .unwrap();
    assert_eq!(interrupted.status.code(), Some(71));
    assert!(space.ok(&["next", "work"]).contains("attempt 1"));

    let recovered = space.ok(&["claim", "work", "--ticket", ticket]);
    assert!(recovered.contains("Claimed handoff as attempt 2"));
    assert!(recovered.contains("Finish the draft."));
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/work/{output}"))).unwrap(),
        "partly written"
    );
    space.fails(&["claim", "work", "--ticket", ticket], "no pending handoff");
    space.fails(
        &["complete", "work", "01-draft", "--attempt", "1"],
        "cannot complete",
    );

    let waiting = space.ok(&["complete", "work", "01-draft", "--attempt", "2"]);
    let gate = space.ok(&["offer", "work"]);
    assert!(
        space
            .ok(&["claim", "work", "--ticket", offered_ticket(&gate)])
            .contains("Use this draft?")
    );
    assert_eq!(revision(&space.ok(&["status", "work"])), revision(&waiting));
}

#[test]
fn handoff_rejects_changed_inputs_and_allows_only_one_concurrent_claim() {
    let space = Space::new();
    space.brief("work");
    space.stage("work", "01-draft", &["inputs/data.txt"], None);
    space.write("runs/work/inputs/data.txt", "original");
    space.ok(&["start", "work"]);
    space.ok(&["next", "work"]);
    let initial = space.ok(&["offer", "work", "--attempt", "1"]);
    space.write("runs/work/inputs/data.txt", "changed");
    space.fails(
        &["claim", "work", "--ticket", offered_ticket(&initial)],
        "no longer current",
    );
    space.ok(&["next", "work", "--retry"]);
    let offer = space.ok(&["offer", "work", "--attempt", "2"]);
    let args = ["claim", "work", "--ticket", offered_ticket(&offer)];
    let first = space.command(&args).stdout(Stdio::piped()).spawn().unwrap();
    let second = space.command(&args).stdout(Stdio::piped()).spawn().unwrap();
    let a = first.wait_with_output().unwrap();
    let b = second.wait_with_output().unwrap();
    assert_ne!(a.status.success(), b.status.success());
    assert!(space.ok(&["handoff", "work"]).contains("Attempt: 3"));
}

fn copy_bundle(source: &std::path::Path, destination: &std::path::Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_bundle(&entry.path(), &path);
        } else {
            fs::copy(entry.path(), path).unwrap();
        }
    }
}

#[test]
fn verified_bundle_preserves_revisions_and_rejects_bad_content_and_duplicate_identity() {
    let from = Space::new();
    from.brief("work");
    let output = from.stage("work", "01-draft", &[], Some("Use this draft?"));
    from.stage("work", "02-finish", &[&output], None);
    from.write("runs/work/inputs/unlisted.txt", "private scratch");
    from.ok(&["start", "work"]);
    from.ok(&["next", "work"]);
    let live = format!("runs/work/{output}");
    from.write(&live, "first revision");
    let first = from.ok(&["complete", "work", "01-draft", "--attempt", "1"]);
    from.ok(&[
        "approve",
        "work",
        "01-draft",
        "--revision",
        revision(&first),
    ]);
    from.write(&live, "edited revision");
    let second = from.ok(&["status", "work"]);
    assert_ne!(revision(&first), revision(&second));
    let offer = from.ok(&["offer", "work"]);
    from.ok(&["pack", "work", "travel"]);
    assert!(from.state("work")["switch_offer"].is_object());
    assert!(
        !from
            .path("bundles/travel/files/inputs/unlisted.txt")
            .exists()
    );

    let destination = Space::new();
    copy_bundle(
        &from.path("bundles/travel"),
        &destination.path("bundles/travel"),
    );
    destination.write(&format!("bundles/travel/files/{output}"), "tampered");
    destination.fails(&["unpack", "travel", "new-run"], "changed while copying");
    assert!(!destination.path("runs/new-run").exists());
    destination.write(&format!("bundles/travel/files/{output}"), "edited revision");

    let manifest = "bundles/travel/manifest.json";
    let original = fs::read(destination.path(manifest)).unwrap();
    let mut bad: Value = serde_json::from_slice(&original).unwrap();
    bad["files"]["../outside"] = json!("invalid");
    destination.write(manifest, serde_json::to_vec(&bad).unwrap());
    destination.fails(
        &["unpack", "travel", "new-run"],
        "invalid portable path component",
    );
    assert!(!destination.path("runs/new-run").exists());
    destination.write(manifest, original);
    destination.ok(&["unpack", "travel", "new-run"]);
    destination.ok(&["check"]);
    destination.fails(
        &["unpack", "travel", "second-copy"],
        "identity is already installed",
    );
    destination.fails(
        &["claim", "new-run", "--ticket", offered_ticket(&offer)],
        "no pending handoff",
    );
    let restored = destination.ok(&["status", "new-run"]);
    assert_eq!(revision(&restored), revision(&second));
    destination.fails(&["next", "new-run"], "awaits human review");
    let diff = destination.ok(&["diff", "new-run", "01-draft"]);
    assert!(diff.contains("first revision") && diff.contains("edited revision"));
    destination.ok(&[
        "approve",
        "new-run",
        "01-draft",
        "--revision",
        revision(&second),
    ]);
    assert!(destination.ok(&["next", "new-run"]).contains("02-finish"));
}

fn confirmation(preview: &str) -> &str {
    preview
        .lines()
        .find_map(|line| line.strip_prefix("Confirmation: "))
        .unwrap()
}

#[test]
fn approved_corrections_teach_only_future_runs_with_fresh_inputs() {
    let space = Space::new();
    space.brief("first");
    let output = space.stage(
        "first",
        "01-draft",
        &["inputs/notes.md"],
        Some("Send this update?"),
    );
    space.stage("first", "02-file", &[&output], None);
    space.write("runs/first/inputs/notes.md", "OLD CLIENT PRIVATE FACT");
    space.ok(&["start", "first"]);
    space.ok(&["next", "first"]);
    space.write(
        &format!("runs/first/{output}"),
        "First draft with speculation",
    );
    let state = space.ok(&["complete", "first", "01-draft", "--attempt", "1"]);
    space.ok(&[
        "approve",
        "first",
        "01-draft",
        "--revision",
        revision(&state),
    ]);
    space.ok(&["save-workflow", "first", "updates"]);

    let args = [
        "learn",
        "first",
        "01-draft",
        "--workflow",
        "updates",
        "--rule",
        "Keep speculation out of client updates.",
    ];
    space.fails(&args, "no recorded edit");
    space.write(&format!("runs/first/{output}"), "Edited client-ready draft");
    space.fails(&args, "must be approved");
    let state = space.ok(&["status", "first"]);
    space.ok(&[
        "approve",
        "first",
        "01-draft",
        "--revision",
        revision(&state),
    ]);
    let original_state = space.state("first");
    let guide = "workflows/updates/guidance/01-draft/review.md";
    let preview = space.ok(&args);
    assert!(preview.contains("Edited file") && preview.contains("Add: Keep speculation"));
    assert!(!space.path(guide).exists());

    space.write(guide, "# Review guidance\n\n- Keep dates explicit.\n");
    let stale = [&args[..], &["--confirm", confirmation(&preview)]].concat();
    space.fails(&stale, "proposal or workflow changed");
    let preview = space.ok(&args);
    let confirm = [&args[..], &["--confirm", confirmation(&preview)]].concat();
    space.ok(&confirm);
    assert!(space.ok(&confirm).contains("already recorded"));
    let guidance = fs::read_to_string(space.path(guide)).unwrap();
    assert_eq!(guidance.matches("Keep speculation out").count(), 1);
    assert!(guidance.contains("Keep dates explicit."));
    assert!(!guidance.contains("OLD CLIENT") && !guidance.contains("Edited client-ready"));

    space.write("inbox/new-notes.md", "FRESH CLIENT FACT");
    space.write("inbox/unselected.md", "DO NOT READ");
    let repeat = [
        "repeat",
        "updates",
        "second",
        "--brief",
        "Prepare an update for the new client.",
    ];
    space.fails(&repeat, "map the required inputs");
    let invalid = [&repeat[..], &["--input", "inbox/../secret=inputs/notes.md"]].concat();
    space.fails(&invalid, "invalid portable path");
    let duplicate = [
        &repeat[..],
        &[
            "--input",
            "inbox/new-notes.md=inputs/notes.md",
            "--input",
            "inbox/unselected.md=inputs/notes.md",
        ],
    ]
    .concat();
    space.fails(&duplicate, "repeated destination");
    assert!(!space.path("runs/second").exists());
    let repeat = [
        &repeat[..],
        &["--input", "inbox/new-notes.md=inputs/notes.md"],
    ]
    .concat();
    let context = space.ok(&repeat);
    assert!(context.contains("Stage 01-draft | attempt 1"));
    assert!(context.contains("Keep speculation out") && context.contains("FRESH CLIENT FACT"));
    assert!(!context.contains("DO NOT READ") && !context.contains("OLD CLIENT"));
    assert_eq!(space.state("first"), original_state);

    let local_guide = "runs/second/references/anydock/01-draft/review-1.md";
    assert_eq!(
        fs::read_to_string(space.path(local_guide)).unwrap(),
        guidance
    );
    let downstream = fs::read_to_string(space.path("runs/second/stages/02-file/stage.md")).unwrap();
    assert!(!downstream.contains("references/anydock"));
    space.write(
        guide,
        "# Review guidance\n\n- The owner can refine this anytime.\n",
    );
    assert_eq!(
        fs::read_to_string(space.path(local_guide)).unwrap(),
        guidance
    );
    assert!(
        space
            .ok(&["next", "second", "--context"])
            .contains("Keep speculation out")
    );
    space.ok(&["check"]);
}

#[test]
fn new_corrections_from_workflow_runs_keep_the_original_stage_scope() {
    let space = Space::new();
    space.brief("origin");
    let output = space.stage("origin", "01-write", &[], Some("Accept this draft?"));
    space.ok(&["start", "origin"]);
    space.ok(&["save-workflow", "origin", "writing"]);
    space.write(
        "workflows/writing/guidance/01-write/review.md",
        "# Review guidance\n\n- Start with the ask.\n",
    );
    let context = space.ok(&[
        "repeat",
        "writing",
        "working",
        "--brief",
        "Draft a new note.",
    ]);
    assert!(context.contains("Start with the ask."));
    space.write(&format!("runs/working/{output}"), "draft");
    space.ok(&["complete", "working", "01-write", "--attempt", "1"]);
    space.write(&format!("runs/working/{output}"), "edited");
    let status = space.ok(&["status", "working"]);
    space.ok(&[
        "approve",
        "working",
        "01-write",
        "--revision",
        revision(&status),
    ]);
    let args = [
        "learn",
        "working",
        "01-write",
        "--workflow",
        "writing",
        "--rule",
        "Close with the decision needed.",
    ];
    let preview = space.ok(&args);
    let confirmation = [&args[..], &["--confirm", confirmation(&preview)]].concat();
    space.ok(&confirmation);
    let next = space.ok(&[
        "repeat",
        "writing",
        "next-job",
        "--brief",
        "Draft another note.",
    ]);
    assert!(
        next.contains("Start with the ask.") && next.contains("Close with the decision needed.")
    );
    let old = space.ok(&["handoff", "working"]);
    assert!(old.contains("Run complete"));
}

#[test]
fn update_keeps_only_verified_current_branches_and_reopens_review() {
    let space = Space::new();
    space.brief("prior");
    let lead = space.stage(
        "prior",
        "01-lead",
        &["inputs/lead.md"],
        Some("Accept lead?"),
    );
    let side = space.stage(
        "prior",
        "02-side",
        &["inputs/side.md"],
        Some("Accept side?"),
    );
    let edit = space.stage("prior", "03-lead-edit", &[&lead], Some("Accept lead edit?"));
    let support = space.stage("prior", "04-side-edit", &[&side], Some("Accept side edit?"));
    let join = space.stage("prior", "05-packet", &[&edit, &support], None);
    space.write("runs/prior/inputs/lead.md", "old lead source");
    space.write("runs/prior/inputs/side.md", "stable side source");
    space.ok(&["start", "prior"]);
    let mut previous_lead = String::new();
    for (i, (stage, output)) in [
        ("01-lead", &lead),
        ("02-side", &side),
        ("03-lead-edit", &edit),
        ("04-side-edit", &support),
        ("05-packet", &join),
    ]
    .into_iter()
    .enumerate()
    {
        space.ok(&["next", "prior"]);
        let path = format!("runs/prior/{output}");
        space.write(&path, format!("completed {stage}"));
        let result = space.ok(&[
            "complete",
            "prior",
            stage,
            "--attempt",
            &(i + 1).to_string(),
        ]);
        if stage == "05-packet" {
            continue;
        }
        let token = revision(&result);
        space.ok(&["approve", "prior", stage, "--revision", token]);
        if stage == "01-lead" {
            previous_lead = token.to_owned();
        }
        if stage == "02-side" {
            space.write(&path, "approved edit of side");
            let edited = space.ok(&["status", "prior"]);
            space.ok(&["approve", "prior", stage, "--revision", revision(&edited)]);
        }
    }
    let before = space.state("prior");
    space.write("inbox/new-lead.md", "today’s lead source");
    space.fails(
        &[
            "update",
            "prior",
            "bad",
            "--input",
            "inbox/../new-lead.md=inputs/lead.md",
        ],
        "invalid portable path",
    );
    space.fails(
        &[
            "update",
            "prior",
            "bad",
            "--input",
            "inbox/new-lead.md=inputs/unlisted.md",
        ],
        "not a declared run input",
    );
    assert!(!space.path("runs/bad").exists());
    let update = space.ok(&[
        "update",
        "prior",
        "today",
        "--input",
        "inbox/new-lead.md=inputs/lead.md",
    ]);
    assert!(update.contains("Changed input: runs/today/inputs/lead.md"));
    assert!(
        update.contains("Rework 01-lead")
            && update.contains("Rework 03-lead-edit")
            && update.contains("Rework 05-packet")
    );
    assert!(update.contains("Reused 02-side") && update.contains("Reused 04-side-edit"));
    assert!(update.contains("today’s lead source") && !update.contains("stable side source"));
    assert!(!space.path(&format!("runs/today/{edit}")).exists());
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/today/{side}"))).unwrap(),
        "approved edit of side"
    );
    let state = space.state("today");
    assert_ne!(state["identity"], before["identity"]);
    assert_eq!(
        state["stages"]["02-side"]["order"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        state["stages"]["02-side"]["head"],
        before["stages"]["02-side"]["head"]
    );
    assert_eq!(
        state["stages"]["04-side-edit"]["approvals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(space.state("prior"), before);

    let handoff = space.ok(&["handoff", "today", "--json"]);
    assert!(handoff.contains("runs/prior") && handoff.contains("runs/today/inputs/lead.md"));
    space.write(&format!("runs/today/{lead}"), "new lead");
    let pending = space.ok(&["complete", "today", "01-lead", "--attempt", "1"]);
    assert!(pending.contains("awaiting review") && pending.contains("04-side-edit | done"));
    space.fails(
        &["approve", "today", "01-lead", "--revision", &previous_lead],
        "revision changed",
    );
    space.fails(&["next", "today"], "awaits human review");
    space.fails(&["update", "today", "unfinished"], "finish the source run");
    space.ok(&[
        "approve",
        "today",
        "01-lead",
        "--revision",
        revision(&pending),
    ]);
    assert!(space.ok(&["next", "today"]).contains("03-lead-edit"));
    space.ok(&["check"]);
}

#[test]
fn update_survives_an_interrupted_commit_and_can_finish_without_work() {
    let space = Space::new();
    space.brief("prior");
    let output = space.stage("prior", "01-work", &[], Some("Accept the draft?"));
    space.ok(&["start", "prior"]);
    space.ok(&["next", "prior"]);
    space.write(&format!("runs/prior/{output}"), "approved");
    let status = space.ok(&["complete", "prior", "01-work", "--attempt", "1"]);
    space.ok(&[
        "approve",
        "prior",
        "01-work",
        "--revision",
        revision(&status),
    ]);
    let args = ["update", "prior", "current"];
    let stopped = space
        .command(&args)
        .env("ANYDOCK_TEST_STOP", "before-state-replace")
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(71));
    assert!(!space.path("runs/current").exists());
    assert!(space.ok(&args).contains("All stages are current"));
    assert!(space.ok(&["status", "current"]).contains("01-work | done"));
    assert_eq!(space.state("current")["sequence"], 0);
}

#[test]
fn update_can_map_an_explicit_full_inbox_without_guessing() {
    let space = Space::new();
    space.brief("prior");
    let output = space.stage("prior", "01-work", &["inputs/assets/notes.md"], None);
    space.write("runs/prior/inputs/assets/notes.md", "prior source");
    space.ok(&["start", "prior"]);
    space.ok(&["next", "prior"]);
    space.write(&format!("runs/prior/{output}"), "completed draft");
    space.ok(&["complete", "prior", "01-work", "--attempt", "1"]);
    space.write("inbox/week/assets/unselected.md", "private");
    space.fails(
        &["update", "prior", "today", "--inbox", "inbox/week"],
        "missing update file",
    );
    assert!(!space.path("runs/today").exists());
    space.write("inbox/week/assets/notes.md", "fresh source");
    let result = space.ok(&["update", "prior", "today", "--inbox", "inbox/week"]);
    assert!(result.contains("fresh source"));
    assert!(
        !space
            .path("runs/today/inputs/assets/unselected.md")
            .exists()
    );
    space.write("inbox/assets/notes.md", "complete default source set");
    let result = space.ok(&["update", "prior", "default", "--inbox"]);
    assert!(result.contains("complete default source set"));
}

#[test]
fn definition_publishes_existing_contracts_and_declared_references_only() {
    let space = Space::new();
    let definition = json!({
        "stages": [
            {
                "id": "01-draft",
                "work": "Summarize the source.",
                "inputs": ["inputs/data.txt"],
                "outputs": ["stages/01-draft/outputs/result.md"],
                "review": "Accept this draft?",
                "references": [{"path": "references/style.md"}]
            }
        ]
    });
    space.write(
        "inbox/setup/briefing.json",
        serde_json::to_vec(&definition).unwrap(),
    );
    let args = ["define", "briefing", "--from", "inbox/setup/briefing.json"];
    space.fails(&args, "missing reference");
    assert!(!space.path("workflows/briefing").exists());
    space.write("inbox/setup/references/style.md", "Keep it short.");
    space.write("inbox/setup/references/private.md", "Unselected.");
    assert!(space.ok(&args).contains("Defined 1 stages"));
    assert!(
        !space
            .path("workflows/briefing/references/private.md")
            .exists()
    );
    space.fails(&args, "already exists");
    space.write("inbox/data.txt", "Selected source.");
    let context = space.ok(&[
        "repeat",
        "briefing",
        "work",
        "--brief",
        "Prepare a briefing.",
        "--input",
        "inbox/data.txt",
    ]);
    assert!(context.contains("Keep it short.") && context.contains("Selected source."));
    space.ok(&["check"]);
}

#[test]
fn review_shows_an_exact_delta_and_survives_a_missing_previous_run() {
    let space = Space::new();
    space.brief("first");
    let output = space.stage(
        "first",
        "01-draft",
        &["inputs/data.txt"],
        Some("Approve the packet?"),
    );
    space.write("runs/first/inputs/data.txt", "old source");
    space.ok(&["start", "first"]);
    space.ok(&["next", "first"]);
    space.write(
        &format!("runs/first/{output}"),
        "Previous approved wording.\nStable release: v0.7.0\n",
    );
    space.ok(&["complete", "first", "01-draft", "--attempt", "1"]);
    let first = space.ok(&["review", "first"]);
    assert!(first.contains("Review full file:"));
    space.ok(&[
        "approve",
        "first",
        "01-draft",
        "--revision",
        revision(&first),
    ]);
    space.write("inbox/data.txt", "new source");
    space.ok(&["update", "first", "second", "--inbox"]);
    let draft = format!("runs/second/{output}");
    space.write(&draft, "New wording.\nStable release: v0.7.2\n");
    space.ok(&["complete", "second", "01-draft", "--attempt", "1"]);
    let delta = space.ok(&["review", "second"]);
    assert!(delta.contains("Approve the packet?"));
    assert!(delta.contains("-Previous approved wording.") && delta.contains("+New wording."));
    assert!(delta.contains("Stable release: v0.7.[-0-]{+2+}"));
    space.write(&draft, "Changed after presentation.\n");
    space.fails(
        &[
            "approve",
            "second",
            "01-draft",
            "--revision",
            revision(&delta),
        ],
        "revision changed",
    );
    fs::rename(space.path("runs/first"), space.path("runs/moved")).unwrap();
    let current = space.ok(&["review", "second"]);
    assert!(current.contains("Review full file:"));
    assert_ne!(revision(&delta), revision(&current));
    space.ok(&[
        "approve",
        "second",
        "01-draft",
        "--revision",
        revision(&current),
    ]);
}

#[test]
fn batch_review_binds_all_revisions_without_unlocking_dependencies_early() {
    let space = Space::new();
    space.brief("pack");
    let left = space.stage("pack", "01-brief", &[], Some("Approve brief?"));
    let right = space.stage("pack", "02-notes", &[], Some("Approve notes?"));
    space.stage("pack", "03-join", &[&left, &right], None);
    space.ok(&["start", "pack"]);
    space.ok(&["batch-review", "pack"]);
    space.ok(&["next", "pack"]);
    space.write(&format!("runs/pack/{left}"), "draft left");
    space.ok(&["complete", "pack", "01-brief", "--attempt", "1"]);
    assert!(space.ok(&["next", "pack"]).contains("02-notes"));
    space.write(&format!("runs/pack/{right}"), "draft right");
    space.ok(&["complete", "pack", "02-notes", "--attempt", "2"]);
    space.fails(&["next", "pack"], "awaits human review");
    let batch_token = |text: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix("Batch revision: ").map(str::to_owned))
            .unwrap()
    };
    let first = space.ok(&["review", "pack"]);
    assert!(first.contains("Approve brief?") && first.contains("Approve notes?"));
    let stale = batch_token(&first);
    space.write(&format!("runs/pack/{right}"), "edited after review");
    space.fails(
        &["approve-batch", "pack", "--revision", &stale],
        "review batch changed",
    );
    let state = space.state("pack");
    assert_eq!(state["stages"]["01-brief"]["approvals"], json!([]));
    assert_eq!(state["stages"]["02-notes"]["approvals"], json!([]));
    let fresh = batch_token(&space.ok(&["review", "pack"]));
    let args = ["approve-batch", "pack", "--revision", &fresh];
    let interrupted = space
        .command(&args)
        .env("ANYDOCK_TEST_STOP", "before-state-replace")
        .output()
        .unwrap();
    assert_eq!(interrupted.status.code(), Some(71));
    assert_eq!(
        space.state("pack")["stages"]["01-brief"]["approvals"],
        json!([])
    );
    assert!(space.ok(&args).contains("Approved 2 exact revisions"));
    assert!(space.ok(&args).contains("already approved"));
    assert!(space.ok(&["next", "pack"]).contains("03-join"));
}

#[test]
fn local_session_uses_the_same_commands_and_recovers_after_command_errors() {
    use std::io::Write;
    let space = Space::new();
    let mut child = space
        .command(&["session"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for (id, args) in [
        ("one", vec!["check"]),
        ("two", vec!["review", "missing"]),
        ("three", vec!["status"]),
        ("four", vec!["session"]),
    ] {
        writeln!(stdin, "{}", json!({"id":id,"args":args})).unwrap();
    }
    drop(stdin);
    let out = good(child.wait_with_output().unwrap());
    let responses: Vec<Value> = out
        .lines()
        .filter_map(|line| line.strip_prefix('\u{1e}'))
        .map(|frame| serde_json::from_str(frame).unwrap())
        .collect();
    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0], json!({"id":"one","ok":true}));
    assert!(
        responses[1]["error"]
            .as_str()
            .unwrap()
            .contains("not registered")
    );
    assert_eq!(responses[2], json!({"id":"three","ok":true}));
    assert!(
        responses[3]["error"]
            .as_str()
            .unwrap()
            .contains("separate AnyDock process")
    );
    assert!(out.contains("0 runs, 0 workflows") && out.contains("idle"));
}
#[test]
fn pending_update_keeps_exact_drafts_and_reopens_only_the_changed_review() {
    let space = Space::new();
    space.brief("packet");
    let a = space.stage("packet", "01-uv", &["inputs/uv.txt"], Some("Approve uv?"));
    let b = space.stage(
        "packet",
        "02-ruff",
        &["inputs/ruff.txt"],
        Some("Approve Ruff?"),
    );
    let c = space.stage("packet", "03-join", &[&a, &b], None);
    space.write("runs/packet/inputs/uv.txt", "uv 1");
    space.write("runs/packet/inputs/ruff.txt", "Ruff 1");
    space.ok(&["start", "packet"]);
    space.ok(&["batch-review", "packet"]);
    space.ok(&["next", "packet"]);
    space.write(&format!("runs/packet/{a}"), "Release value: version 1");
    space.ok(&["complete", "packet", "01-uv", "--attempt", "1"]);
    space.ok(&["next", "packet"]);
    space.write(
        &format!("runs/packet/{b}"),
        "Unchanged exact draft for Ruff",
    );
    space.ok(&["complete", "packet", "02-ruff", "--attempt", "2"]);
    let presented = space.ok(&["review", "packet"]);
    let earlier = batch_revision(&presented);
    let source = space.state("packet");

    assert!(
        space
            .ok(&["update", "packet", "matching", "--pending"])
            .contains("Pending review:")
    );
    space.write("inbox/new-uv.txt", "uv 2");
    space.fails(
        &[
            "update",
            "packet",
            "corrected",
            "--input",
            "inbox/new-uv.txt=inputs/uv.txt",
        ],
        "finish the source run",
    );
    let amended = space.ok(&[
        "update",
        "packet",
        "corrected",
        "--pending",
        "--input",
        "inbox/new-uv.txt=inputs/uv.txt",
    ]);
    assert!(amended.contains("draft awaits review"));
    assert!(amended.contains("kept 0 approvals"));
    assert!(amended.contains("Stage 01-uv | attempt 1"));
    assert_eq!(space.state("packet"), source);
    assert_eq!(
        fs::read_to_string(space.path(&format!("runs/corrected/{b}"))).unwrap(),
        "Unchanged exact draft for Ruff"
    );
    assert!(!space.path(&format!("runs/corrected/{c}")).exists());

    space.write(&format!("runs/corrected/{a}"), "Release value: version 2");
    let completed = space.ok(&[
        "complete",
        "corrected",
        "01-uv",
        "--attempt",
        "1",
        "--quiet",
    ]);
    assert_eq!(completed.trim(), "Completed 01-uv at attempt 1.");
    space.fails(&["next", "corrected"], "awaits human review");
    space.fails(
        &["review", "corrected", "--since", "unrecognized"],
        "unavailable",
    );
    let review = space.ok(&["review", "corrected", "--since", &earlier]);
    assert!(review.contains("Release value: version [-1-]{+2+}"));
    assert!(review.contains("Unchanged pending review: 02-ruff"));
    assert!(!review.contains(&format!("runs/corrected/{b}")));
    let fresh = batch_revision(&review);
    assert_ne!(fresh, earlier);
    space.fails(
        &["approve-batch", "corrected", "--revision", &earlier],
        "review batch changed",
    );
    space.ok(&["approve-batch", "corrected", "--revision", &fresh]);
    assert!(space.ok(&["next", "corrected"]).contains("Stage 03-join"));
}

#[test]
fn same_run_since_only_skips_the_exact_previously_presented_revision() {
    let space = Space::new();
    space.brief("pack");
    let a = space.stage("pack", "01-a", &[], Some("Approve A?"));
    let b = space.stage("pack", "02-b", &[], Some("Approve B?"));
    space.ok(&["start", "pack"]);
    space.ok(&["batch-review", "pack"]);
    space.ok(&["next", "pack"]);
    space.write(&format!("runs/pack/{a}"), "Release version: v1");
    space.ok(&["complete", "pack", "01-a", "--attempt", "1"]);
    space.ok(&["next", "pack"]);
    space.write(&format!("runs/pack/{b}"), "Reviewed context");
    space.ok(&["complete", "pack", "02-b", "--attempt", "2"]);
    let first = batch_revision(&space.ok(&["review", "pack"]));
    space.write(&format!("runs/pack/{a}"), "Release version: v2");
    let second = space.ok(&["review", "pack", "--since", &first]);
    assert!(second.contains("Release version: v[-1-]{+2+}"));
    assert!(second.contains("Unchanged pending review: 02-b"));
    assert_ne!(batch_revision(&second), first);
}

fn batch_revision(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("Batch revision: "))
        .unwrap()
        .into()
}

#[test]
fn an_earlier_batch_survives_two_updates_and_a_verified_move() {
    let from = Space::new();
    from.brief("first");
    let a = from.stage("first", "01-uv", &["inputs/uv.txt"], Some("Approve uv?"));
    let b = from.stage(
        "first",
        "02-ruff",
        &["inputs/ruff.txt"],
        Some("Approve Ruff?"),
    );
    let final_path = from.stage("first", "03-final", &[&a, &b], None);
    from.write("runs/first/inputs/uv.txt", "uv 1");
    from.write("runs/first/inputs/ruff.txt", "ruff 1");
    from.ok(&["start", "first"]);
    from.ok(&["batch-review", "first"]);
    from.ok(&["next", "first"]);
    from.write(&format!("runs/first/{a}"), "Release value: version 1");
    from.ok(&["complete", "first", "01-uv", "--attempt", "1"]);
    from.ok(&["next", "first"]);
    from.write(&format!("runs/first/{b}"), "Exact unchanged Ruff draft");
    from.ok(&["complete", "first", "02-ruff", "--attempt", "2"]);
    let token = batch_revision(&from.ok(&["review", "first"]));

    from.fails(
        &[
            "update",
            "first",
            "wrong",
            "--pending",
            "--since",
            "unknown",
        ],
        "unavailable",
    );
    assert!(!from.path("runs/wrong").exists());
    for (source, destination, version) in [("first", "second", "2"), ("second", "latest", "3")] {
        from.write("inbox/uv.txt", format!("uv {version}"));
        from.ok(&[
            "update",
            source,
            destination,
            "--pending",
            "--since",
            &token,
            "--input",
            "inbox/uv.txt",
        ]);
        from.write(
            &format!("runs/{destination}/{a}"),
            format!("Release value: version {version}"),
        );
        from.ok(&["complete", destination, "01-uv", "--attempt", "1"]);
    }

    let state = from.state("latest");
    assert_eq!(state["version"], 4);
    let retained_digest = state["update"]["presentation"]["stages"][0]["record"]["outputs"][&a]
        .as_str()
        .unwrap();
    let blob = format!("runs/latest/.anydock/blobs/{retained_digest}");
    fs::remove_file(from.path(&blob)).unwrap();
    from.fails(
        &["review", "latest", "--since", &token],
        "retained review archive",
    );
    from.fails(&["pack", "latest", "portable"], "retained review archive");
    from.write(&blob, "altered original");
    from.fails(
        &["review", "latest", "--since", &token],
        "retained review archive",
    );
    from.write(&blob, "Release value: version 1");
    from.ok(&["pack", "latest", "portable"]);

    let to = Space::new();
    copy_bundle(&from.path("bundles/portable"), &to.path("bundles/portable"));
    to.ok(&["unpack", "portable", "resumed"]);
    assert!(!to.path("runs/first").exists() && !to.path("runs/second").exists());
    let handoff = to.ok(&["handoff", "resumed"]);
    assert!(handoff.contains(&format!("anydock review resumed --since {token}")));
    let review = to.ok(&["review", "resumed", "--since", &token]);
    assert!(review.contains("Release value: version [-1-]{+3+}"));
    assert!(review.contains("Unchanged pending review: 02-ruff"));
    assert!(!review.contains(&format!("runs/resumed/{b}")));
    assert!(!to.path(&format!("runs/resumed/{final_path}")).exists());
    to.fails(&["next", "resumed"], "awaits human review");
    to.fails(
        &["approve-batch", "resumed", "--revision", &token],
        "review batch changed",
    );
    to.ok(&[
        "approve-batch",
        "resumed",
        "--revision",
        &batch_revision(&review),
    ]);
    assert!(to.ok(&["next", "resumed"]).contains("Stage 03-final"));
}

#[test]
fn scoped_input_keeps_review_only_when_exact_selected_values_match() {
    let space = Space::new();
    let stage = "01-review";
    let path = "inputs/source.json";
    let output = "stages/01-review/outputs/handoff.md";
    let initial =
        r#"{"metadata":"unselected first","result":{"n":9007199254740993.0},"pkg/list":[true]}"#;
    let refreshed =
        r#"{"pkg/list":[true],"metadata":"unselected refresh","result":{"n":9007199254740993.0}}"#;
    let revised =
        r#"{"metadata":"unselected second","result":{"n":9007199254740992.0},"pkg/list":[true]}"#;
    space.brief("first");
    let fields = json!({
        "inputs": [path],
        "input_fields": {path: ["/result/n", "/pkg~1list/0"]},
        "outputs": [output],
        "review": "Approve exact selected values?"
    });
    space.write("runs/first/stages/01-review/stage.md", contract(&fields));
    space.write("runs/first/inputs/source.json", initial);
    space.ok(&["start", "first"]);
    let next = space.ok(&["next", "first", "--context"]);
    assert!(next.contains("9007199254740993.0"));
    assert!(!next.contains("unselected first"));
    assert!(next.contains("anydock input first 01-review inputs/source.json"));
    let view = space.ok(&["input", "first", stage, path]);
    assert!(view.contains("\"/pkg~1list/0\": true"));
    assert!(view.contains("\"/result/n\": 9007199254740993.0"));
    assert!(!view.contains("metadata"));
    let handoff = space.ok(&["handoff", "first", "--json"]);
    assert!(handoff.contains("anydock input first 01-review inputs/source.json"));

    space.write("runs/first/inputs/source.json", refreshed);
    space.write(&format!("runs/first/{output}"), "approved selected values");
    let status = space.ok(&["complete", "first", stage, "--attempt", "1"]);
    let original_token = revision(&status);
    space.ok(&["approve", "first", stage, "--revision", original_token]);

    space.write("inbox/source.json", initial);
    let update = space.ok(&["update", "first", "refreshed", "--inbox"]);
    assert!(update.contains("kept 1 approvals"));
    assert!(update.contains("There is no new work to review."));
    assert_eq!(
        fs::read_to_string(space.path("runs/refreshed/inputs/source.json")).unwrap(),
        initial
    );

    space.write("inbox/source.json", revised);
    let update = space.ok(&["update", "refreshed", "changed", "--inbox"]);
    assert!(update.contains("kept 0 approvals") && update.contains("Rework 01-review"));
    assert!(update.contains("9007199254740992.0"));
    space.write(&format!("runs/changed/{output}"), "revised selected values");
    space.write("runs/changed/inputs/source.json", initial);
    space.fails(
        &["input", "changed", stage, path],
        "changed during attempt 1",
    );
    space.fails(&["complete", "changed", stage, "--attempt", "1"], "stale");
    let retry = space.ok(&["next", "changed", "--retry"]);
    assert!(retry.contains("attempt 2"));
    let status = space.ok(&["complete", "changed", stage, "--attempt", "2"]);
    assert_ne!(revision(&status), original_token);
    space.fails(
        &["approve", "changed", stage, "--revision", original_token],
        "revision changed",
    );
}

#[test]
fn scoped_input_reports_malformed_ambiguous_or_missing_values() {
    let space = Space::new();
    let path = "inputs/source.json";
    space.brief("run");
    let fields = |pointer| {
        json!({
            "inputs": [path],
            "input_fields": {path: [pointer]},
            "outputs": ["stages/01-go/outputs/output.md"],
            "review": null
        })
    };
    space.write(
        "runs/run/stages/01-go/stage.md",
        contract(&fields("/items/~2")),
    );
    space.fails(&["check"], "invalid JSON pointer");
    space.write(
        "runs/run/stages/01-go/stage.md",
        contract(&fields("/items/0/k")),
    );
    space.ok(&["start", "run"]);
    for (content, reason) in [
        ("{", "contains invalid JSON"),
        (r#"{"items":[]}"#, "missing array index 0"),
        (r#"{"items":[{"k":1,"k":2}]}"#, "ambiguous JSON object"),
        (r#"{"items":[{}]}"#, "missing key \"k\""),
    ] {
        space.write("runs/run/inputs/source.json", content);
        let status = space.ok(&["status", "run"]);
        assert!(
            status.contains("blocked") && status.contains(path) && status.contains(reason),
            "{status}"
        );
        space.fails(&["next", "run"], reason);
    }
}

#[test]
fn recurring_refresh_recovers_a_published_run_and_preserves_its_review() {
    let space = Space::new();
    space.brief("seed");
    let output = space.stage(
        "seed",
        "01-work",
        &["inputs/data.txt"],
        Some("Approve the edited packet?"),
    );
    space.write("runs/seed/inputs/data.txt", "first packet");
    space.ok(&["start", "seed"]);
    space.fails(&["refresh", "desk", "--from", "seed"], "finish seed run");
    space.ok(&["next", "seed"]);
    space.write(&format!("runs/seed/{output}"), "first handoff");
    let first = space.ok(&["complete", "seed", "01-work", "--attempt", "1"]);
    space.ok(&["approve", "seed", "01-work", "--revision", revision(&first)]);
    assert!(
        space
            .ok(&["refresh", "desk", "--from", "seed"])
            .contains("Current run: seed")
    );

    space.write("inbox/data.txt", "first packet");
    assert!(
        space
            .ok(&["refresh", "desk", "--inbox"])
            .contains("already recorded")
    );
    assert!(!space.path("runs/desk-0001").exists());
    space.fails(
        &["refresh", "desk", "--inbox", "inbox/../outside"],
        "invalid portable path",
    );
    space.write("inbox/data.txt", "second packet");
    let interrupted = space
        .command(&["refresh", "desk", "--inbox"])
        .env("ANYDOCK_TEST_STOP", "after-refresh-publish")
        .output()
        .unwrap();
    assert_eq!(interrupted.status.code(), Some(71));
    let head_path = space.path(".anydock/recurring/desk/head.json");
    let read_head = || serde_json::from_slice::<Value>(&fs::read(&head_path).unwrap()).unwrap();
    assert_eq!(read_head()["run"], "seed");
    assert_eq!(space.state("desk-0001")["version"], 5);
    let queued = space.ok(&["queue"]);
    assert!(queued.contains("desk | in progress | desk-0001"));
    assert_eq!(read_head()["run"], "seed");
    space.ok(&["check"]);
    assert_eq!(read_head()["run"], "seed");

    let recovered = space.ok(&["refresh", "desk", "--inbox"]);
    assert!(recovered.contains("Recovered published refresh desk-0001"));
    assert!(recovered.contains("Current run: desk-0001") && recovered.contains("Attempt: 1"));
    assert_eq!(read_head()["run"], "desk-0001");
    fs::rename(space.path("runs/seed"), space.path("archived-seed")).unwrap();
    space.write(&format!("runs/desk-0001/{output}"), "second handoff");
    let review = space.ok(&["complete", "desk-0001", "01-work", "--attempt", "1"]);
    let pending = space.state("desk-0001");
    let replay = space.ok(&["refresh", "desk", "--inbox"]);
    assert!(
        replay.contains("Approve the edited packet?") && replay.contains("Current run: desk-0001")
    );
    assert_eq!(space.state("desk-0001"), pending);

    space.write("inbox/data.txt", "third packet");
    space.fails(&["refresh", "desk", "--inbox"], "finish the source run");
    assert!(!space.path("runs/desk-0002").exists());
    space.ok(&[
        "approve",
        "desk-0001",
        "01-work",
        "--revision",
        revision(&review),
    ]);
    let next = space.ok(&["refresh", "desk", "--inbox"]);
    assert!(next.contains("Current run: desk-0002"));
    space.ok(&["check"]);
}

#[test]
fn concurrent_recurring_submissions_share_a_run_and_reject_unrelated_destinations() {
    let space = Space::new();
    space.brief("seed");
    let output = space.stage("seed", "01-work", &["inputs/data.txt"], None);
    space.write("runs/seed/inputs/data.txt", "old");
    space.ok(&["start", "seed"]);
    space.ok(&["next", "seed"]);
    space.write(&format!("runs/seed/{output}"), "old handoff");
    space.ok(&["complete", "seed", "01-work", "--attempt", "1"]);
    space.ok(&["refresh", "desk", "--from", "seed"]);
    space.write("inbox/data.txt", "new");
    let submit = || {
        space
            .command(&["refresh", "desk", "--inbox"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = submit();
    let second = submit();
    let reports = [
        good(first.wait_with_output().unwrap()),
        good(second.wait_with_output().unwrap()),
    ];
    assert!(
        reports
            .iter()
            .all(|report| report.contains("Current run: desk-0001"))
    );
    assert_eq!(
        reports
            .iter()
            .filter(|report| report.contains("Updated seed as desk-0001"))
            .count(),
        1
    );
    assert_eq!(
        reports
            .iter()
            .filter(|report| report.contains("already recorded"))
            .count(),
        1
    );
    assert!(!space.path("runs/desk-0002").exists());

    space.brief("desk-0002");
    space.stage("desk-0002", "01-work", &[], None);
    space.ok(&["start", "desk-0002"]);
    space.fails(
        &["refresh", "desk"],
        "does not belong to the pending refresh",
    );
}

#[test]
fn queue_reports_current_review_files_and_isolates_unavailable_tasks() {
    let space = Space::new();
    assert_eq!(
        serde_json::from_str::<Value>(&space.ok(&["queue", "--json"])).unwrap()["tasks"],
        json!([])
    );
    space.brief("seed");
    let output = space.stage(
        "seed",
        "01-work",
        &["inputs/data.txt"],
        Some("Approve this draft?"),
    );
    let final_output = space.stage("seed", "02-publish", &[&output], None);
    space.write("runs/seed/inputs/data.txt", "old");
    space.ok(&["start", "seed"]);
    space.ok(&["next", "seed"]);
    space.write(&format!("runs/seed/{output}"), "initial draft");
    let first = space.ok(&["complete", "seed", "01-work", "--attempt", "1"]);
    space.ok(&["approve", "seed", "01-work", "--revision", revision(&first)]);
    space.ok(&["next", "seed"]);
    space.write(
        &format!("runs/seed/{final_output}"),
        "initial published file",
    );
    space.ok(&["complete", "seed", "02-publish", "--attempt", "2"]);
    space.ok(&["refresh", "desk", "--from", "seed"]);
    assert!(!space.ok(&["queue"]).contains("desk"));
    assert!(space.ok(&["queue", "--all"]).contains("desk | done | seed"));

    space.write("inbox/data.txt", "new");
    space.ok(&["refresh", "desk", "--inbox"]);
    space.write(&format!("runs/desk-0001/{output}"), "current draft");
    space.ok(&["complete", "desk-0001", "01-work", "--attempt", "1"]);
    // An old run and an unrecorded edit are both visible on disk.
    space.write(&format!("runs/seed/{output}"), "edited historical draft");
    space.write(&format!("runs/desk-0001/{output}"), "edited current draft");
    space.write(".anydock/recurring/damaged/head.json", "{");
    let before = space.state("desk-0001");
    let view: Value = serde_json::from_str(&space.ok(&["queue", "--json"])).unwrap();
    let tasks = view["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["name"], "desk");
    assert_eq!(tasks[0]["run"], "desk-0001");
    assert_eq!(tasks[0]["state"], "awaiting review");
    assert_eq!(tasks[0]["reviews"][0]["decision"], "Approve this draft?");
    assert_eq!(
        tasks[0]["reviews"][0]["files"][0],
        format!("runs/desk-0001/{output}")
    );
    assert_eq!(tasks[0]["reviews"][0]["gates"], "02-publish");
    assert_eq!(tasks[0]["next"], "anydock review desk-0001");
    assert_eq!(tasks[1]["name"], "damaged");
    assert_eq!(tasks[1]["state"], "unavailable");
    assert_eq!(space.state("desk-0001"), before);

    space.write("runs/desk-0001/inputs/data.txt", "changed again");
    let stale = space.ok(&["queue"]);
    assert!(stale.contains("desk | stale") && stale.contains("file changed: inputs/data.txt"));
    assert!(!stale.contains("Review file:"));
    let contract_path = "runs/desk-0001/stages/01-work/stage.md";
    let changed = fs::read_to_string(space.path(contract_path))
        .unwrap()
        .replace("Approve this draft?", "Approve the new policy?");
    space.write(contract_path, changed);
    let policy = space.ok(&["queue"]);
    assert!(
        policy.contains("desk | review policy change")
            && policy.contains("Approve this draft?")
            && policy.contains("Approve the new policy?")
    );
    assert_eq!(space.state("desk-0001"), before);
}

#[cfg(windows)]
#[test]
fn rejects_misspelled_existing_ancestor_of_a_missing_input() {
    let space = Space::new();
    space.brief("path");
    space.stage("path", "01-check", &["inputs/reports/missing.txt"], None);
    space.write("runs/path/Inputs/reports/present.txt", "Existing input");
    space.fails(&["check"], "different spelling");
}

#[test]
fn direct_recurring_start_recovers_before_review_and_rejects_changed_setup() {
    let space = Space::new();
    let definition = "inbox/setup/task.json";
    let reference = "references/checks.md";
    let output = "stages/01-review/outputs/handoff.md";
    let spec = json!({
        "stages": [{
            "id": "01-review",
            "work": "Review the submitted values.",
            "inputs": ["inputs/data.txt"],
            "outputs": [output],
            "review": "Approve the current values?",
            "references": [{"path": reference}]
        }]
    });
    space.write(definition, spec.to_string());
    space.write(
        "inbox/setup/references/checks.md",
        "Read each submitted value.",
    );
    let start = [
        "begin",
        "desk",
        "--definition",
        definition,
        "--brief",
        "Review this packet.",
        "--inbox",
        "inbox/packet",
    ];
    space.fails(&start, "missing submitted input");
    assert!(!space.path("runs/desk-0000").exists());
    assert!(space.ok(&["check"]).contains("0 runs, 0 workflows"));

    space.write("inbox/packet/data.txt", "First packet");
    let crashed = space
        .command(&start)
        .env("ANYDOCK_TEST_STOP", "after-begin-publish")
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(71));
    let pointer = space.path(".anydock/recurring/desk/head.json");
    assert!(!pointer.exists());
    let original = space.state("desk-0000");
    assert_eq!(original["version"], 6);
    assert_eq!(original["recurring_start"]["task"], "desk");
    fs::remove_file(space.path(definition)).unwrap();
    assert!(
        space
            .ok(&["queue"])
            .contains("desk | in progress | desk-0000")
    );
    assert!(!pointer.exists());
    assert!(
        space
            .ok(&["refresh", "desk"])
            .contains("Recovered published initial run desk-0000")
    );
    assert!(pointer.exists());
    space.write(definition, spec.to_string());
    assert!(
        space
            .ok(&start)
            .contains("initial submission is already recorded")
    );
    assert_eq!(space.state("desk-0000"), original);
    space.write(
        "inbox/setup/references/checks.md",
        "A different set of checks.",
    );
    space.fails(&start, "different start request");
    assert_eq!(space.state("desk-0000"), original);
    let named_ref = space.path("runs/desk-0000/references/checks.md");
    assert_eq!(
        fs::read_to_string(named_ref).unwrap(),
        "Read each submitted value."
    );

    space.write(&format!("runs/desk-0000/{output}"), "Initial result");
    space.ok(&["complete", "desk-0000", "01-review", "--attempt", "1"]);
    let completed = space.ok(&["review", "desk", "--task"]);
    assert!(completed.contains("anydock approve desk --task 01-review"));
    assert!(space.ok(&["queue"]).contains("Approve the current values?"));
    space.write("inbox/packet/data.txt", "Second packet");
    space.fails(
        &["refresh", "desk", "--inbox", "inbox/packet"],
        "finish the source run",
    );
    space.ok(&[
        "approve",
        "desk",
        "01-review",
        "--task",
        "--revision",
        revision(&completed),
    ]);
    let updated = space.ok(&["refresh", "desk", "--inbox", "inbox/packet"]);
    assert!(updated.contains("Current run: desk-0001") && updated.contains("attempt 1"));
    assert!(space.ok(&["check"]).contains("2 runs, 0 workflows"));
}

#[test]
fn concurrent_direct_starts_publish_one_initial_run() {
    let space = Space::new();
    let definition = "inbox/task.json";
    space.write(
        definition,
        json!({
            "stages": [{
                "id": "01-review", "work": "Read the packet.",
                "inputs": ["inputs/source.txt"], "outputs": ["stages/01-review/outputs/handoff.md"],
                "review": "Approve packet?"
            }]
        })
        .to_string(),
    );
    space.write("inbox/source.txt", "Original packet");
    let command = || {
        space
            .command(&[
                "begin",
                "desk",
                "--definition",
                definition,
                "--brief",
                "Review packet.",
                "--batch-review",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = command();
    let b = command();
    let results = [
        good(a.wait_with_output().unwrap()),
        good(b.wait_with_output().unwrap()),
    ];
    assert!(results.iter().all(|r| r.contains("Current run: desk-0000")));
    assert_eq!(
        results
            .iter()
            .filter(|r| r.contains("initial submission is already recorded"))
            .count(),
        1
    );
    assert_eq!(space.state("desk-0000")["batch_review"], true);
    assert!(space.ok(&["check"]).contains("1 runs, 0 workflows"));
}

#[test]
fn named_amendment_keeps_the_current_task_and_fences_the_old_batch() {
    let space = Space::new();
    let a = "stages/01-a/outputs/handoff.md";
    let b = "stages/02-b/outputs/handoff.md";
    space.write("inbox/desk.json", json!({"stages": [
        {"id":"01-a","work":"Draft A.","inputs":["inputs/a.txt"],"outputs":[a],"review":"Approve A?"},
        {"id":"02-b","work":"Draft B.","inputs":["inputs/b.txt"],"outputs":[b],"review":"Approve B?"}
    ]}).to_string());
    space.write("inbox/a.txt", "A source 1");
    space.write("inbox/b.txt", "B source");
    space.ok(&[
        "begin",
        "desk",
        "--definition",
        "inbox/desk.json",
        "--brief",
        "Review the packet.",
        "--batch-review",
    ]);
    space.write("inbox/a.txt", "A source 2");
    space.fails(
        &["refresh", "desk", "--inbox", "--pending"],
        "waiting for review",
    );
    space.write(&format!("runs/desk-0000/{a}"), "A draft value: version 1");
    space.ok(&["complete", "desk-0000", "01-a", "--attempt", "1"]);
    space.ok(&["next", "desk-0000"]);
    space.write(&format!("runs/desk-0000/{b}"), "B draft stays unchanged");
    space.ok(&["complete", "desk-0000", "02-b", "--attempt", "2"]);
    let first = space.ok(&["review", "desk", "--task"]);
    assert!(first.contains("anydock approve-batch desk --task --revision"));
    let earlier = batch_revision(&first);
    let before = space.state("desk-0000");

    space.fails(&["refresh", "desk", "--inbox"], "finish the source run");
    space.fails(
        &[
            "refresh",
            "desk",
            "--inbox",
            "--pending",
            "--since",
            "unknown",
        ],
        "unavailable",
    );
    assert!(!space.path("runs/desk-0001").exists());
    fs::remove_file(space.path("inbox/b.txt")).unwrap();
    let request = [
        "refresh",
        "desk",
        "--input",
        "inbox/a.txt",
        "--pending",
        "--since",
        &earlier,
    ];
    let crashed = space
        .command(&request)
        .env("ANYDOCK_TEST_STOP", "after-refresh-publish")
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(71));
    assert!(space.ok(&["queue"]).contains("desk-0001"));
    assert!(
        space
            .ok(&request)
            .contains("This submission is already recorded.")
    );
    space.fails(
        &[
            "refresh",
            "desk",
            "--input",
            "inbox/a.txt",
            "--pending",
            "--since",
            "wrong",
        ],
        "different review comparison",
    );
    assert_eq!(space.state("desk-0000"), before);
    assert!(!space.path("runs/desk-0002").exists());

    fs::rename(space.path("runs/desk-0000"), space.path("older-drafts")).unwrap();
    space.write(&format!("runs/desk-0001/{a}"), "A draft value: version 2");
    space.ok(&["complete", "desk-0001", "01-a", "--attempt", "1"]);
    let fresh = space.ok(&["review", "desk", "--task", "--retained"]);
    assert!(fresh.contains("A draft value: version [-1-]{+2+}"));
    assert!(fresh.contains("Unchanged pending review: 02-b"));
    assert!(!fresh.contains(&format!("runs/desk-0001/{b}")));
    assert_eq!(
        fs::read_to_string(space.path("runs/desk-0001/inputs/b.txt")).unwrap(),
        "B source"
    );
    let current = batch_revision(&fresh);
    space.fails(
        &["approve-batch", "desk", "--task", "--revision", &earlier],
        "review batch changed",
    );
    space.ok(&["approve-batch", "desk", "--task", "--revision", &current]);
    assert!(space.ok(&["refresh", "desk"]).contains("Run complete."));
    space.ok(&["check"]);
}

#[test]
fn partial_refresh_uses_only_named_replacements_and_verified_inherited_inputs() {
    let space = Space::new();
    let output = "stages/01-work/outputs/brief.md";
    space.write("inbox/task.json", json!({
        "stages": [{
            "id":"01-work","work":"Use the current inputs.",
            "inputs":["inputs/a.txt","inputs/b.txt","inputs/c.txt"],"outputs":[output],"review":null
        }]
    }).to_string());
    space.write("inbox/a.txt", "First A");
    space.write("inbox/b.txt", "First B");
    space.write("inbox/c.txt", "Same C");
    space.ok(&[
        "begin",
        "notes",
        "--definition",
        "inbox/task.json",
        "--brief",
        "Prepare the packet.",
    ]);
    space.write(&format!("runs/notes-0000/{output}"), "First brief");
    space.ok(&["complete", "notes-0000", "01-work", "--attempt", "1"]);
    let c = "runs/notes-0000/inputs/c.txt";

    space.write("inbox/replacements/alternate.txt", "Next A");
    space.write("inbox/b.txt", "Next B");
    fs::remove_file(space.path("inbox/c.txt")).unwrap();
    let request = [
        "refresh",
        "notes",
        "--input",
        "inbox/replacements/alternate.txt=inputs/a.txt",
        "--input",
        "inbox/b.txt",
    ];
    fs::remove_file(space.path(c)).unwrap();
    space.fails(&request, "missing inherited input inputs/c.txt");
    assert!(!space.path("runs/notes-0001").exists());
    space.write(c, "Same C");
    space.fails(
        &["refresh", "notes", "--inbox", "--input", "inbox/b.txt"],
        "cannot be used with",
    );
    space.fails(
        &[
            "refresh",
            "notes",
            "--input",
            "inbox/a.txt",
            "--input",
            "inbox/b.txt=inputs/a.txt",
        ],
        "repeated destination",
    );
    space.fails(
        &[
            "refresh",
            "notes",
            "--input",
            "inbox/../outside.txt=inputs/a.txt",
        ],
        "invalid portable path",
    );

    let updated = space.ok(&request);
    assert!(updated.contains("Current run: notes-0001"));
    for (file, value) in [
        ("a.txt", "Next A"),
        ("b.txt", "Next B"),
        ("c.txt", "Same C"),
    ] {
        assert_eq!(
            fs::read_to_string(space.path(&format!("runs/notes-0001/inputs/{file}"))).unwrap(),
            value
        );
    }
    let state = space.state("notes-0001");
    assert_eq!(
        state["update"]["inputs"],
        json!(["inputs/a.txt", "inputs/b.txt"])
    );
    assert_eq!(state["update"]["submission"].as_object().unwrap().len(), 3);
    assert!(
        space
            .ok(&request)
            .contains("This submission is already recorded.")
    );
    assert_eq!(space.state("notes-0001"), state);
    assert!(!space.path("runs/notes-0002").exists());
}

#[test]
fn recurring_source_is_snapshotted_and_recovers_without_inbox_copies() {
    let space = Space::new();
    let source = tempfile::tempdir().unwrap();
    let path = source.path().to_str().unwrap();
    let data = source.path().join("data.txt");
    let definition = "inbox/task.json";
    let output = "stages/01-review/outputs/handoff.md";
    space.write(
        definition,
        json!({"stages": [{
            "id": "01-review",
            "work": "Review the submitted values.",
            "inputs": ["inputs/data.txt"],
            "outputs": [output],
            "review": "Approve the source values?"
        }]})
        .to_string(),
    );
    let begin = [
        "begin",
        "desk",
        "--definition",
        definition,
        "--brief",
        "Review this packet.",
        "--source",
        path,
    ];
    space.fails(&begin, "missing submitted input");
    assert!(!space.path("runs/desk-0000").exists());

    fs::write(&data, "First packet").unwrap();
    fs::write(source.path().join("unrelated.txt"), "Do not import").unwrap();
    let stopped = space
        .command(&begin)
        .env("ANYDOCK_TEST_STOP", "after-begin-publish")
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(71));
    assert!(!space.path(".anydock/recurring/desk/head.json").exists());
    assert!(
        space
            .ok(&["refresh", "desk"])
            .contains("Recovered published initial run")
    );
    assert!(
        space
            .ok(&["refresh", "desk", "--submit"])
            .contains("already recorded")
    );
    assert!(!space.path("runs/desk-0000/inputs/unrelated.txt").exists());
    assert!(!space.path("inbox/data.txt").exists());
    fs::write(source.path().join("unrelated.txt"), "Ignore this change").unwrap();
    let unchanged = space.ok(&["queue", "--sources", "--json"]);
    assert!(!unchanged.contains("source_changes"));

    fs::write(&data, "Second packet").unwrap();
    assert_eq!(
        fs::read_to_string(space.path("runs/desk-0000/inputs/data.txt")).unwrap(),
        "First packet"
    );
    space.write(&format!("runs/desk-0000/{output}"), "First review");
    let first = space.ok(&["complete", "desk-0000", "01-review", "--attempt", "1"]);
    space.fails(&["refresh", "desk", "--submit"], "finish the source run");
    let pending: Value =
        serde_json::from_str(&space.ok(&["queue", "--sources", "--json"])).unwrap();
    let task = &pending["tasks"][0];
    assert_eq!(task["state"], "awaiting review");
    assert_eq!(task["source_changes"], json!(["data.txt"]));
    assert_eq!(
        task["reviews"][0]["files"],
        json!([format!("runs/desk-0000/{output}")])
    );
    space.ok(&[
        "approve",
        "desk-0000",
        "01-review",
        "--revision",
        revision(&first),
    ]);
    let exact_state = fs::read(space.path("runs/desk-0000/.anydock/state.json")).unwrap();
    let head = ".anydock/recurring/desk/head.json";
    let exact_head = fs::read(space.path(head)).unwrap();
    assert!(space.ok(&["queue"]).contains("No recurring tasks"));
    let ready = space.ok(&["queue", "--sources"]);
    assert!(ready.contains("desk | source changed | desk-0000"));
    assert!(ready.contains("Changed in saved source: data.txt"));
    assert!(ready.contains("Next: anydock refresh desk --submit"));
    assert!(!space.path("runs/desk-0001").exists());
    assert_eq!(
        fs::read(space.path("runs/desk-0000/.anydock/state.json")).unwrap(),
        exact_state
    );
    assert_eq!(fs::read(space.path(head)).unwrap(), exact_head);

    let stopped = space
        .command(&["refresh", "desk", "--submit"])
        .env("ANYDOCK_TEST_STOP", "after-refresh-publish")
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(71));
    let queue = space.ok(&["queue", "--sources"]);
    assert!(queue.contains("desk | in progress | desk-0001"));
    assert!(!queue.contains("Changed in saved source:"));
    assert_eq!(fs::read(space.path(head)).unwrap(), exact_head);
    let resumed = space.ok(&["refresh", "desk", "--submit"]);
    assert!(resumed.contains("Recovered published refresh desk-0001"));
    assert!(resumed.contains("already recorded"));
    assert_eq!(
        fs::read_to_string(space.path("runs/desk-0001/inputs/data.txt")).unwrap(),
        "Second packet"
    );

    source.close().unwrap();
    let queue = space.ok(&["queue", "--sources"]);
    assert!(queue.contains("desk | in progress | desk-0001"));
    assert!(queue.contains("saved source folder has moved or redirects"));
    assert!(space.ok(&["refresh", "desk"]).contains("desk-0001"));
    space.fails(
        &["refresh", "desk", "--submit"],
        "source folder has moved or redirects",
    );
    assert!(!space.path("runs/desk-0002").exists());
    space.write(&format!("runs/desk-0001/{output}"), "Second review");
    assert!(
        space
            .ok(&["complete", "desk-0001", "01-review", "--attempt", "1"])
            .contains("Approve the source values?")
    );
    assert!(
        space
            .ok(&["review", "desk", "--task"])
            .contains("Second review")
    );

    space.ok(&["pack", "desk-0001", "portable"]);
    let receiving = Space::new();
    fs::create_dir_all(receiving.path("bundles")).unwrap();
    fs::rename(
        space.path("bundles/portable"),
        receiving.path("bundles/portable"),
    )
    .unwrap();
    receiving.ok(&["unpack", "portable", "imported"]);
    let review = receiving.ok(&["review", "imported"]);
    assert!(review.contains("Approve the source values?"));
    assert!(review.contains(&format!("runs/imported/{output}")));
    assert_eq!(
        fs::read_to_string(receiving.path(&format!("runs/imported/{output}"))).unwrap(),
        "Second review"
    );
    assert!(
        !receiving
            .path(".anydock/recurring/desk/source.json")
            .exists()
    );
}

#[test]
fn daily_brief_preserves_source_values_review_state_and_notes() {
    let space = Space::new();
    space.fails(&["daily"], "configure .anydock/daily.json");
    assert!(!space.path("briefs").exists());
    let producer = tempfile::tempdir().unwrap();
    let metrics = producer.path().join("Metrics (daily).json");
    fs::write(&metrics, "{\"active\":17,\"rate\":\"1.00\"}\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&metrics)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(7200)),
        )
        .unwrap();
    fs::write(
        producer.path().join("failed.txt"),
        "producer returned exit 3",
    )
    .unwrap();
    space.write(
        ".anydock/daily.json",
        json!({"feeds": [
            {"name":"metrics", "folder":producer.path(), "files":["Metrics (daily).json"],
             "stale_after_hours":1, "failure_file":"failed.txt"},
            {"name":"summaries", "folder":"missing-exports", "stale_after_hours":24}
        ]})
        .to_string(),
    );
    space.write("briefs/notes.md", "Personal notes");
    space.brief("review");
    let output = space.stage(
        "review",
        "01-draft",
        &["inputs/source.txt"],
        Some("Accept this exact draft?"),
    );
    space.write("runs/review/inputs/source.txt", "Release source");
    space.ok(&["start", "review"]);
    space.ok(&["next", "review"]);
    space.write(&format!("runs/review/{output}"), "Editable draft");
    let completed = space.ok(&["complete", "review", "01-draft", "--attempt", "1"]);
    let first_token = revision(&completed);
    let state = space.state("review");
    let report = space.path("briefs/daily.md");
    space.ok(&["daily"]);
    let first = fs::read_to_string(&report).unwrap();
    assert!(first.contains("Last source update"));
    assert!(first.contains("{\"active\":17,\"rate\":\"1.00\"}"));
    assert!(first.contains("Stale · no update within 1 hours"));
    assert!(first.contains("Producer reported failure"));
    assert!(first.contains("Missing source folder"));
    assert!(first.contains("Accept this exact draft?"));
    assert!(first.contains(first_token));
    assert!(first.contains(&format!("../runs/review/{output}")));
    assert_eq!(space.state("review"), state);
    let saved = first
        .lines()
        .find(|line| line.contains("New input"))
        .unwrap()
        .split_once("](sources/")
        .unwrap()
        .1
        .trim_end_matches(')');
    let saved = space.path("briefs/sources").join(saved);
    assert_eq!(fs::read(&saved).unwrap(), fs::read(&metrics).unwrap());

    fs::write(&metrics, "{\"active\":19}\n").unwrap();
    space.write(
        &format!("runs/review/{output}"),
        "Edited draft awaiting a new revision",
    );
    space.ok(&["daily"]);
    let second = fs::read_to_string(&report).unwrap();
    assert!(second.contains("{\"active\":19}"));
    assert!(second.contains("unrecorded revision"));
    assert!(!second.contains(first_token));
    assert_eq!(
        fs::read_to_string(&saved).unwrap(),
        "{\"active\":17,\"rate\":\"1.00\"}\n"
    );
    assert_eq!(space.state("review"), state);

    space.ok(&["daily"]);
    assert!(fs::read_to_string(&report).unwrap().contains("New input"));
    let second_review = space.ok(&["review", "review"]);
    space.ok(&[
        "approve",
        "review",
        "01-draft",
        "--revision",
        revision(&second_review),
    ]);
    space.ok(&["daily"]);
    let body = fs::read_to_string(&report).unwrap();
    assert!(body.contains("## Completed work"));
    assert!(body.contains("No pending managed reviews."));
    space.write(
        &format!("runs/review/{output}"),
        "A new approved-baseline delta",
    );
    let third = space.ok(&["review", "review"]);
    let third_token = revision(&third);
    let before = space.state("review");
    space.ok(&["daily"]);
    let revised = fs::read_to_string(&report).unwrap();
    assert!(revised.contains(third_token));
    assert!(revised.contains("Changes since approval"));
    assert!(revised.contains("-Edited draft awaiting a new revision"));
    assert!(revised.contains("+A new approved-baseline delta"));
    assert_eq!(space.state("review"), before);
    let edited = revised + "\nMy morning notes\n";
    fs::write(&report, &edited).unwrap();
    space.fails(&["daily"], "contains edits or is user-owned");
    assert_eq!(fs::read_to_string(&report).unwrap(), edited);
    assert_eq!(
        fs::read_to_string(space.path("briefs/notes.md")).unwrap(),
        "Personal notes"
    );
}

#[test]
fn daily_feed_setup_is_explicit_and_retryable() {
    let space = Space::new();
    space.write("feeds/metrics/current.json", "{\"count\":27}\n");
    space.write("feeds/releases/latest.md", "# New release\n");
    space.fails(&["daily", "--feed", "missing-assignment"], "NAME=FOLDER");
    space.fails(
        &[
            "daily",
            "--feed",
            "desk=feeds/metrics",
            "--feed",
            "desk=feeds/releases",
        ],
        "duplicate feed desk",
    );
    assert!(!space.path(".anydock/daily.json").exists());

    let request = [
        "daily",
        "--feed",
        "metrics=feeds/metrics",
        "--feed",
        "releases=feeds/releases",
    ];
    let stopped = space
        .command(&request)
        .env("ANYDOCK_TEST_STOP", "after-daily-config")
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(71));
    let config = space.path(".anydock/daily.json");
    let original = fs::read(&config).unwrap();
    assert!(!space.path("briefs/daily.md").exists());
    let manifest: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(manifest["feeds"][0]["stale_after_hours"], 24);
    assert_eq!(manifest["feeds"][1]["folder"], "feeds/releases");

    assert!(space.ok(&request).contains("Wrote briefs/daily.md"));
    let report = space.path("briefs/daily.md");
    let before = fs::read_to_string(&report).unwrap();
    assert!(before.contains("{\"count\":27}"));
    assert!(before.contains("# New release"));
    assert!(space.ok(&["check"]).contains("0 runs, 0 workflows"));
    space.fails(
        &["daily", "--feed", "other=feeds/releases"],
        "daily feed configuration already exists",
    );
    assert_eq!(fs::read(&config).unwrap(), original);
    assert_eq!(fs::read_to_string(&report).unwrap(), before);
    assert!(space.ok(&["daily"]).contains("Wrote briefs/daily.md"));
}

#[test]
fn daily_submission_binds_the_full_packet_and_recovers_the_same_run() {
    let space = Space::new();
    let definition = "inbox/desk.json";
    let stage = "01-work";
    let output = "stages/01-work/outputs/report.md";
    let head = ".anydock/recurring/desk/head.json";
    space.write(
        definition,
        json!({"stages":[{"id": stage, "work":"Read both inputs.",
            "inputs":["inputs/metrics.txt","inputs/context.txt"],"outputs":[output],
            "review":"Approve the report?"
        }]})
        .to_string(),
    );
    space.write("producer/metrics.txt", "Morning totals");
    space.write("producer/context.txt", "One source");
    space.ok(&[
        "begin",
        "desk",
        "--definition",
        definition,
        "--brief",
        "Read the packet.",
        "--source",
        space.path("producer").to_str().unwrap(),
    ]);
    space.write(&format!("runs/desk-0000/{output}"), "First report");
    let pending = space.ok(&["complete", "desk-0000", stage, "--attempt", "1"]);
    space.write("producer/metrics.txt", "Updated totals");
    space.ok(&["daily", "--feed", "metrics=producer"]);
    let report = space.path("briefs/daily.md");
    let blocked = fs::read_to_string(&report).unwrap();
    assert!(blocked.contains("incoming changes in metrics.txt"));
    assert!(!blocked.contains("--source-revision"));
    space.ok(&[
        "approve",
        "desk-0000",
        stage,
        "--revision",
        revision(&pending),
    ]);
    let brief_revision = || {
        space.ok(&["daily"]);
        fs::read_to_string(&report)
            .unwrap()
            .split_once("anydock refresh desk --submit --source-revision ")
            .unwrap()
            .1
            .split('`')
            .next()
            .unwrap()
            .to_owned()
    };
    let first = brief_revision();
    let observed = fs::read_to_string(&report).unwrap();
    let snapshot = observed
        .split_once("Observed files · [metrics.txt](sources/desk/")
        .unwrap()
        .1
        .split(')')
        .next()
        .unwrap();
    assert_eq!(
        fs::read_to_string(space.path(&format!("briefs/sources/desk/{snapshot}"))).unwrap(),
        "Updated totals"
    );
    let exact_head = fs::read(space.path(head)).unwrap();
    let exact_state = space.state("desk-0000");
    space.write("producer/context.txt", "Another source");
    space.fails(
        &["refresh", "desk", "--submit", "--source-revision", &first],
        "the observed source revision is no longer current",
    );
    assert!(!space.path("runs/desk-0001").exists());
    assert_eq!(fs::read(space.path(head)).unwrap(), exact_head);
    assert_eq!(space.state("desk-0000"), exact_state);

    let current = brief_revision();
    assert_ne!(current, first);
    let submit = ["refresh", "desk", "--submit", "--source-revision", &current];
    let stopped = space
        .command(&submit)
        .env("ANYDOCK_TEST_STOP", "after-refresh-publish")
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(71));
    assert_eq!(fs::read(space.path(head)).unwrap(), exact_head);
    space.write("producer/metrics.txt", "Later totals");
    let retry = space.ok(&submit);
    assert!(retry.contains("Recovered published refresh desk-0001"));
    assert!(retry.contains("This source revision is already recorded."));
    assert!(
        space
            .ok(&submit)
            .contains("This source revision is already recorded.")
    );
    assert_eq!(
        fs::read_to_string(space.path("runs/desk-0001/inputs/metrics.txt")).unwrap(),
        "Updated totals"
    );
    assert_eq!(
        fs::read_to_string(space.path("runs/desk-0001/inputs/context.txt")).unwrap(),
        "Another source"
    );
    assert!(!space.path("runs/desk-0002").exists());

    space.write(&format!("runs/desk-0001/{output}"), "Second report");
    let result = space.ok(&["complete", "desk-0001", stage, "--attempt", "1"]);
    let review = revision(&result);
    space.ok(&["daily"]);
    let final_brief = fs::read_to_string(&report).unwrap();
    assert!(final_brief.contains(review));
    assert!(final_brief.contains("incoming changes in metrics.txt"));
    assert!(!final_brief.contains("--source-revision"));
    assert!(
        space.state("desk-0001")["stages"][stage]["approvals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn daily_selected_fields_keep_exact_values_and_prior_snapshot() {
    let space = Space::new();
    let source = "producer/Metrics (daily).json";
    let before = "{\"note\":\"skip this raw excerpt\",\"stats\":{\"ratio\":1.00,\"count\":12}}\n";
    let after = "{\"note\":\"skip this raw excerpt\",\"stats\":{\"ratio\":2.00,\"count\":12,\"new\":true}}\n";
    space.write(
        ".anydock/daily.json",
        json!({"feeds":[{"name":"metrics","folder":"producer","stale_after_hours":24,
            "fields":{"Metrics (daily).json":["/stats/ratio","/stats/count","/stats/new","/stats/missing"]}
        }]}).to_string()
    );
    space.write(source, before);
    space.ok(&["daily"]);
    space.write(source, after);
    space.ok(&["daily"]);
    space.ok(&["daily"]);
    let report = fs::read_to_string(space.path("briefs/daily.md")).unwrap();
    assert!(report.contains("/stats/ratio: 1.00 → 2.00"));
    assert!(report.contains("/stats/count: 12"));
    assert!(report.contains("/stats/new: earlier value unavailable → true"));
    assert!(report.contains("/stats/missing") && report.contains("missing key"));
    assert!(!report.contains("skip this raw excerpt"));
    let (link, _) = report
        .split_once("[Earlier snapshot](")
        .unwrap()
        .1
        .split_once(')')
        .unwrap();
    assert_eq!(
        fs::read_to_string(space.path("briefs").join(link)).unwrap(),
        before
    );
    assert_eq!(fs::read_to_string(space.path(source)).unwrap(), after);

    fs::remove_file(space.path(source)).unwrap();
    space.write("producer/Other.json", "{\"status\":\"still present\"}\n");
    space.ok(&["daily"]);
    let missing = fs::read_to_string(space.path("briefs/daily.md")).unwrap();
    assert!(missing.contains("Missing source file Metrics (daily).json"));
    assert!(missing.contains("still present"));
}
