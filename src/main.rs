mod alternatives;
mod approval;
mod bundle;
mod daily;
mod delivery;
mod engine;
mod handoff;
mod input_scope;
mod learning;
mod mcp;
mod policy;
mod portfolio;
mod quality;
mod queue;
mod quick;
mod recurring;
mod reference;
mod review;
mod session;
mod stage_context;
mod storage;
mod transfer;
mod update;
mod upgrade;
mod workflow;
mod workspace;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use engine::{Phase, StageView, View, evaluate};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};
use storage::{LockedRun, State, live_hash};
use workspace::{
    PROGRESS, Plan, Workspace, file, invocation, names, read_brief, run_invocation, shell_word,
};

#[derive(Parser)]
#[command(version, about = "Manage a local AnyDock workspace")]
struct Cli {
    /// Start in this directory before discovering the workspace.
    #[arg(short = 'C', long, value_name = "DIRECTORY")]
    directory: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set up the current folder as an idle AnyDock workspace.
    #[command(display_order = 1)]
    Init,
    /// Set up this project and teach new agent sessions the AnyDock workflow.
    #[command(display_order = 2)]
    Setup {
        /// Configure projects directly inside this folder and future projects in the same folder.
        #[arg(long)]
        projects: bool,
    },
    /// Verify workspace compatibility or bring an older local layout forward in place.
    #[command(display_order = 17)]
    Upgrade {
        #[arg(long)]
        check: bool,
        /// Filename of the previous workspace's version marker.
        #[arg(long)]
        from_marker: Option<String>,
        /// Backup directory outside the workspace. Defaults to a sibling folder.
        #[arg(long, requires = "from_marker", conflicts_with = "check")]
        backup: Option<PathBuf>,
    },
    /// Write the opt-in local Markdown brief from saved feeds and run records.
    #[command(display_order = 20)]
    Daily {
        /// Set up a feed from a folder. Repeat for each initial feed.
        #[arg(long, value_name = "NAME=FOLDER")]
        feed: Vec<String>,
        /// Add a new feed to an existing configuration. Repeat for additional feeds.
        #[arg(long, value_name = "NAME=FOLDER", conflicts_with = "feed")]
        add_feed: Vec<String>,
        /// Remove a feed by name. The producer's files and existing snapshots stay available.
        #[arg(long, value_name = "NAME", conflicts_with = "feed")]
        remove_feed: Vec<String>,
    },
    /// Validate this workspace and its run and workflow definitions.
    #[command(display_order = 16)]
    Check,
    /// Process local CLI requests over standard input.
    #[command(hide = true)]
    Session,
    /// Serve one workspace over local MCP stdio.
    Mcp {
        /// Expose authorized work transitions in addition to read-only tools.
        #[arg(long)]
        allow_work: bool,
    },
    /// Register a run with an existing brief and stage definitions.
    #[command(display_order = 3)]
    Start {
        run: String,
        /// Copy definitions from this saved workflow before registering.
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Register an agent-only run, preserving its progress as an editable note.
    Adopt { run: String },
    /// Begin or resume the first unresolved stage.
    #[command(display_order = 4)]
    Next {
        run: String,
        /// Open a new attempt after inspecting an interruption or stale result.
        #[arg(long)]
        retry: bool,
        /// Print bounded excerpts of the working contract and declared context.
        #[arg(long)]
        context: bool,
    },
    /// Print the selected JSON values from a stage's external input.
    #[command(hide = true)]
    Input {
        run: String,
        stage: String,
        path: String,
    },
    /// Register and begin a single-stage task from an explicit brief and inputs.
    #[command(name = "run", visible_alias = "quick", display_order = 2)]
    Quick {
        run: String,
        #[arg(long)]
        brief: String,
        #[arg(long)]
        work: String,
        #[arg(long)]
        input: Vec<String>,
        /// Snapshot a selected local file without moving or editing the original.
        #[arg(long, value_name = "FILE")]
        source: Vec<PathBuf>,
        #[arg(long)]
        output: String,
        #[arg(long)]
        review: Option<String>,
    },
    /// Record the required handoff from the current attempt.
    #[command(display_order = 5)]
    Complete {
        run: String,
        stage: String,
        #[arg(long)]
        attempt: u64,
        /// Skip the final display scan when automation will fetch the next state.
        #[arg(long)]
        quiet: bool,
    },
    /// List current recurring tasks and their next actions without changing state.
    #[command(display_order = 13)]
    Queue {
        /// Include completed current runs.
        #[arg(long)]
        all: bool,
        /// Emit the task list as JSON.
        #[arg(long)]
        json: bool,
        /// Compare saved source files against the last submitted packet.
        #[arg(long)]
        sources: bool,
    },
    /// Show current status and the next action. Omit RUN to see all runs.
    #[command(display_order = 6)]
    Status {
        run: Option<String>,
        /// Record observed edits before showing status.
        #[arg(long, conflicts_with = "inspect")]
        reconcile: bool,
        #[arg(long, hide = true)]
        inspect: bool,
        /// Show a bounded preview of unrecorded changes for the selected run.
        #[arg(long, requires = "run", conflicts_with = "reconcile")]
        diff: bool,
    },
    #[command(flatten)]
    Review(ReviewCommand),
    /// List archived revisions and the saved copies of their outputs.
    History { run: String, stage: String },
    /// Compare the current recorded handoff with the prior reviewed revision.
    Diff { run: String, stage: String },
    /// Recover an archived output to a separate file under recovered/.
    Restore {
        run: String,
        stage: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        output: String,
        #[arg(long)]
        to: String,
    },
    /// Begin a new run from a saved workflow and fresh inbox files.
    Repeat {
        workflow: String,
        run: String,
        #[arg(long)]
        brief: String,
        /// Map inbox/FILE to inputs/FILE, or use inbox/FILE=inputs/NAME.
        #[arg(long)]
        input: Vec<String>,
    },
    /// Preview or confirm guidance from an approved edit for future workflow runs.
    Learn {
        run: String,
        stage: String,
        #[arg(long)]
        workflow: String,
        #[arg(
            long,
            required_unless_present = "from_rejection",
            conflicts_with = "from_rejection"
        )]
        rule: Option<String>,
        /// Reuse the most recent changes-requested note after the revised output is approved.
        #[arg(long)]
        from_rejection: bool,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Build saved Markdown stage contracts from one inbox definition.
    Define {
        name: String,
        #[arg(long)]
        from: String,
    },
    /// Save a run's stage definitions as a reusable workflow.
    SaveWorkflow { run: String, name: String },
    /// Adopt one declared reference from this run’s saved workflow.
    AdoptReference { run: String, path: String },
    #[command(flatten)]
    Transfer(TransferCommand),
    /// Update a completed run with new inputs and keep verified current work.
    Update {
        from: String,
        run: String,
        #[arg(long)]
        input: Vec<String>,
        /// Use a complete, exact-name input set from inbox/ or a subfolder.
        #[arg(long, num_args = 0..=1, default_missing_value = "inbox", conflicts_with = "input")]
        inbox: Option<String>,
        /// Keep matching recorded drafts while the source waits for review.
        #[arg(long)]
        pending: bool,
        /// Retain this exact earlier batch presentation in the updated run.
        #[arg(long, requires = "pending")]
        since: Option<String>,
    },
    #[command(flatten)]
    Recurring(RecurringCommand),
    /// Start an alternative with the same declared inputs and a fresh run identity.
    Fork { run: String, alternative: String },
    /// Compare current recorded handoffs from two runs.
    Compare { left: String, right: String },
    /// Preview or write a local packet containing explicitly selected handoffs.
    Delivery {
        run: String,
        #[arg(long)]
        name: String,
        #[arg(long, required = true)]
        include: Vec<String>,
        #[arg(long)]
        guidance: bool,
        #[arg(long)]
        decisions: bool,
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Subcommand)]
enum ReviewCommand {
    /// Record the human’s decision to change a registered review requirement.
    #[command(hide = true)]
    AuthorizeReviewChange {
        run: String,
        stage: String,
        #[arg(long)]
        change: String,
    },
    /// Record the human's approval of a specific handoff revision.
    #[command(display_order = 9)]
    Approve {
        run: String,
        stage: String,
        #[arg(long)]
        revision: String,
        /// Interpret RUN as a recurring task name.
        #[arg(long)]
        task: bool,
    },
    /// Request changes on an exact pending stage revision and keep its prior draft.
    #[command(display_order = 11)]
    Reject {
        run: String,
        stage: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        note: String,
        #[arg(long)]
        task: bool,
    },
    /// Group independent review gates for this run and its updates.
    #[command(hide = true)]
    BatchReview { run: String },
    /// Approve the exact revision group presented by review.
    #[command(display_order = 10)]
    ApproveBatch {
        run: String,
        #[arg(long)]
        revision: String,
        /// Interpret RUN as a recurring task name.
        #[arg(long)]
        task: bool,
    },
    /// Show the active decision, previous approved diff, and exact revision.
    #[command(display_order = 8)]
    Review {
        run: String,
        /// Compare with an exact previously shown batch.
        #[arg(long)]
        since: Option<String>,
        /// Use the retained comparison after the reviewer has seen the earlier batch.
        #[arg(long, conflicts_with = "since")]
        retained: bool,
        /// Interpret RUN as a recurring task name.
        #[arg(long)]
        task: bool,
        /// Review and choose a decision in this terminal.
        #[arg(long)]
        interactive: bool,
    },
}

#[derive(Subcommand)]
enum RecurringCommand {
    /// Start and track the first run of a recurring task.
    Begin {
        name: String,
        /// Read stage definitions from this JSON file under inbox/.
        #[arg(
            long,
            required_unless_present = "workflow",
            conflicts_with = "workflow"
        )]
        definition: Option<String>,
        /// Use a saved workflow and its approved guidance for future submissions.
        #[arg(long)]
        workflow: Option<String>,
        #[arg(long)]
        brief: String,
        /// Use a complete, exact-name input set from inbox/ or a subfolder.
        #[arg(long, num_args = 0..=1, default_missing_value = "inbox")]
        inbox: Option<String>,
        /// Use an existing local folder and remember it for explicit future submissions.
        #[arg(long, conflicts_with = "inbox")]
        source: Option<PathBuf>,
        /// Group pending independent reviews.
        #[arg(long)]
        batch_review: bool,
    },
    /// Resume a named recurring task or submit its next complete input packet.
    #[command(display_order = 12)]
    Refresh {
        name: String,
        /// Attach the task to an existing completed run.
        #[arg(long)]
        from: Option<String>,
        /// Use a complete, exact-name input set from inbox/ or a subfolder.
        #[arg(long, num_args = 0..=1, default_missing_value = "inbox")]
        inbox: Option<String>,
        /// Replace an inbox file in the current input set. Repeat to replace more files.
        #[arg(long, conflicts_with = "inbox")]
        input: Vec<String>,
        /// Submit the current packet from the source folder configured at begin.
        #[arg(long, conflicts_with_all = ["inbox", "input", "from"])]
        submit: bool,
        /// Require the exact saved source packet shown in the daily brief.
        #[arg(long, requires = "submit", conflicts_with = "pending")]
        source_revision: Option<String>,
        /// Correct a packet while its current run is waiting for review.
        #[arg(long, conflicts_with = "from")]
        pending: bool,
        /// Retain this exact earlier batch presentation in the corrected run.
        #[arg(long, requires = "pending")]
        since: Option<String>,
    },
}

#[derive(Subcommand)]
enum TransferCommand {
    /// Read the current task and safe next action from any agent harness.
    #[command(display_order = 7)]
    Handoff {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a one-time bootstrap for a receiving agent.
    #[command(hide = true)]
    Offer {
        run: String,
        #[arg(long)]
        attempt: Option<u64>,
    },
    /// Claim a handoff and fence the previous attempt if one was active.
    #[command(hide = true)]
    Claim {
        run: String,
        #[arg(long)]
        ticket: String,
    },
    /// Launch a local harness with a one-time bootstrap as one argument.
    #[command(hide = true)]
    Switch {
        run: String,
        #[arg(long)]
        attempt: Option<u64>,
        #[arg(last = true, required = true, num_args = 1..)]
        harness: Vec<std::ffi::OsString>,
    },
    /// Record a short continuation note on an active attempt.
    Checkpoint {
        run: String,
        #[arg(long)]
        attempt: u64,
        #[arg(long)]
        note: String,
    },
    /// Fence the departing harness’s active attempt and preserve its working files.
    Takeover {
        run: String,
        #[arg(long)]
        attempt: u64,
    },
    /// Save a verified local continuation bundle of a managed run.
    Pack { run: String, bundle: String },
    /// Verify and install a run bundle in this workspace.
    Unpack { bundle: String, run: String },
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("anydock: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<()> {
    let start = cli.directory.map_or_else(std::env::current_dir, Ok)?;
    let command = match cli.command {
        Command::Init => return Workspace::init(&start),
        Command::Setup { projects: true } => return portfolio::setup(&start),
        Command::Setup { projects: false } => return Workspace::setup(&start),
        Command::Upgrade {
            check,
            from_marker,
            backup,
        } => return upgrade::run(&start, from_marker.as_deref(), backup.as_deref(), check),
        command => command,
    };
    let space = Workspace::open(&start)?;
    match command {
        Command::Session => session::serve(&space),
        Command::Mcp { allow_work } => mcp::serve(&space, allow_work),
        command => dispatch(&space, command),
    }
}

fn dispatch(space: &Workspace, command: Command) -> Result<()> {
    match command {
        Command::Check => check(space),
        Command::Daily {
            feed,
            add_feed,
            remove_feed,
        } => daily::write(space, &feed, &add_feed, &remove_feed),
        Command::Queue { all, json, sources } => queue::show(space, all, json, sources),
        Command::Init
        | Command::Setup { .. }
        | Command::Upgrade { .. }
        | Command::Session
        | Command::Mcp { .. } => {
            unreachable!("handled by the CLI entry point")
        }
        Command::Transfer(transfer) => dispatch_transfer(space, transfer),
        Command::Update {
            from,
            run,
            input,
            inbox,
            pending,
            since,
        } => with_run(space, &from, |path, _, state| {
            update::create(
                space,
                path,
                state,
                update::Request {
                    from: &from,
                    id: &run,
                    inputs: inbox.as_deref().map_or(
                        update::InputSelection::Mapped(&input),
                        update::InputSelection::Inbox,
                    ),
                    pending,
                    since: since.as_deref(),
                    submission: None,
                    source_plan: None,
                },
            )
            .map(|_| ())
        }),
        Command::Recurring(RecurringCommand::Begin {
            name,
            definition,
            workflow,
            brief,
            inbox,
            source,
            batch_review,
        }) => recurring::begin(
            space,
            &name,
            match (definition.as_deref(), workflow.as_deref()) {
                (Some(path), None) => recurring::Definition::Inbox(path),
                (None, Some(name)) => recurring::Definition::Saved(name),
                _ => unreachable!("clap requires exactly one definition source"),
            },
            &brief,
            source
                .as_ref()
                .map(|path| space.start.join(path))
                .as_deref()
                .map_or(
                    update::InputSelection::Inbox(inbox.as_deref().unwrap_or("inbox")),
                    update::InputSelection::Source,
                ),
            batch_review,
        ),
        Command::Recurring(RecurringCommand::Refresh {
            name,
            from,
            inbox,
            input,
            submit,
            source_revision,
            pending,
            since,
        }) => recurring::run(
            space,
            recurring::Request {
                name: &name,
                from: from.as_deref(),
                inputs: inbox
                    .as_deref()
                    .map(update::InputSelection::Inbox)
                    .or_else(|| {
                        (!input.is_empty()).then_some(update::InputSelection::Mapped(&input))
                    }),
                submit_saved: submit,
                source_revision: source_revision.as_deref(),
                pending,
                since: since.as_deref(),
            },
        ),
        Command::Fork { run, alternative } => with_run(space, &run, |path, _, state| {
            alternatives::fork(space, path, &run, &alternative, state)
        }),
        Command::Compare { left, right } => alternatives::compare(space, &left, &right),
        Command::Delivery {
            run,
            name,
            include,
            guidance,
            decisions,
            confirm,
        } => {
            let options = delivery::Request {
                name: &name,
                include: &include,
                guidance,
                decisions,
                confirm: confirm.as_deref(),
            };
            if confirm.is_some() {
                with_run(space, &run, |path, _, state| {
                    delivery::run(path, &run, state, options)
                })
            } else {
                let path = space.run(&run)?;
                let mut state = LockedRun::inspect(&path)?.context("run is not registered")?;
                delivery::run(&path, &run, &mut state, options)
            }
        }
        Command::Start { run, workflow } => start(space, &run, workflow.as_deref(), false),
        Command::Adopt { run } => start(space, &run, None, true),
        Command::Status {
            run,
            reconcile,
            diff,
            ..
        } => {
            let runs = match run {
                Some(id) => vec![id],
                None => names(&space.root, "runs")?,
            };
            if runs.is_empty() {
                println!("Workspace is idle. No runs.");
            }
            let mut errors = 0;
            for run in &runs {
                if let Err(err) = status(space, run, !reconcile, diff) {
                    errors += 1;
                    println!("Run {run} | unavailable: {err:#}");
                }
            }
            ensure!(errors == 0, "{errors} run(s) need attention");
            Ok(())
        }
        Command::Quick {
            run,
            brief,
            work,
            input,
            source,
            output,
            review,
        } => quick::create(
            space,
            quick::Request {
                run: &run,
                brief: &brief,
                work: &work,
                input: &input,
                sources: &source,
                output: &output,
                review: review.as_deref(),
            },
        ),
        Command::Input { run, stage, path } => {
            let root = space.run(&run)?;
            let plan = Plan::load(&root)?;
            let item = plan
                .stages
                .iter()
                .find(|item| item.id == stage)
                .with_context(|| format!("unknown stage {stage}"))?;
            let fields = item
                .fields
                .input_fields
                .get(&path)
                .with_context(|| format!("{path} has no selected JSON fields in {stage}"))?;
            let state = LockedRun::inspect(&root)?;
            let format = state.as_ref().map_or_else(
                || item.fields.input_value_format.unwrap_or_default(),
                |state| state.input_format(item),
            );
            let data = input_scope::read(&root, &path, fields, format)?
                .with_context(|| format!("missing input {path}"))?;
            if let Some(state) = state
                && let Some(attempt) = state
                    .stages
                    .get(&stage)
                    .and_then(|entry| entry.attempt.as_ref())
            {
                ensure!(
                    attempt
                        .basis
                        .get(&path)
                        .is_some_and(|input| input.hash == storage::hash(&data))
                        && attempt
                            .basis
                            .get(&item.contract_path())
                            .is_some_and(|version| version.hash == storage::hash(&item.contract)),
                    "{path} or its stage contract changed during attempt {}; inspect status before continuing",
                    attempt.id
                );
            }
            println!("{}", String::from_utf8(data)?);
            Ok(())
        }
        Command::Next {
            run,
            retry,
            context,
        } => with_run(space, &run, |path, lock, state| {
            let command = invocation(&space.root);
            let plan = Plan::load(path)?;
            let view = refresh(path, lock, &plan, state)?;
            if let Some(item) = view.action(state.batch_review) {
                let prior_sequence = state.sequence;
                let attempt = engine::begin(state, item, retry)?;
                if state.sequence != prior_sequence {
                    lock.save(state)?;
                }
                println!("Stage {} | attempt {attempt}", item.stage.id);
                for reason in &item.reasons {
                    println!("{reason}");
                }
                println!("Contract: runs/{run}/{}", item.stage.contract_path());
                for name in item
                    .basis
                    .as_ref()
                    .context("stage basis is missing")?
                    .keys()
                {
                    if item.stage.fields.input_fields.contains_key(name) {
                        println!(
                            "Read selected values: {command} input {run} {} {}",
                            item.stage.id,
                            shell_word(name)
                        );
                    } else if !name.starts_with("references/") {
                        println!("Read: runs/{run}/{name}");
                    }
                }
                for reference in &item.stage.fields.references {
                    println!("Reference: runs/{run}/{}", reference.label());
                }
                for name in &item.stage.fields.outputs {
                    println!("Write: runs/{run}/{name}");
                }
                for check in &item.stage.fields.checks {
                    println!("Required check: {}", check.label());
                }
                if let Some(decision) = &item.stage.fields.review {
                    println!("Human review: {decision}");
                }
                if context {
                    stage_context::show(
                        path,
                        &run,
                        &plan,
                        item.stage,
                        state.input_format(item.stage),
                    )?;
                }
                println!(
                    "After writing and verifying the handoff: {command} complete {run} {} --attempt {attempt}",
                    item.stage.id
                );
            } else {
                println!("Run {run} is complete.");
            }
            Ok(())
        }),
        Command::Complete {
            run,
            stage,
            attempt,
            quiet,
        } => with_run(space, &run, |path, lock, state| {
            complete(path, lock, state, &run, &stage, attempt, quiet)
        }),
        Command::Review(review) => dispatch_review(space, review),
        Command::History { run, stage } => with_run(space, &run, |path, _, state| {
            let entry = state.stages.get(&stage).context("no archived stage")?;
            ensure!(!entry.revisions.is_empty(), "no archived revisions");
            for id in entry.ordered_ids()? {
                let revision = &entry.revisions[&id];
                let label = if entry.head.as_ref() == Some(&id) {
                    " | current record"
                } else {
                    ""
                };
                let reviewed = if entry.approvals.contains(&id) {
                    " | approved"
                } else {
                    ""
                };
                println!("{id}{label}{reviewed}");
                for check in &revision.checks {
                    println!("  Check passed: {check}");
                }
                for (name, hash) in &revision.outputs {
                    storage::verified_blob(path, hash)?;
                    println!("  {name} -> runs/{run}/.anydock/blobs/{hash}");
                }
            }
            Ok(())
        }),
        Command::Diff { run, stage } => {
            let path = space.run(&run)?;
            let state = LockedRun::inspect(&path)?.context("run is not registered")?;
            review::diff(
                &path,
                state.stages.get(&stage).context("no archived stage")?,
                &stage,
            )
        }
        Command::Restore {
            run,
            stage,
            revision,
            output,
            to,
        } => with_run(space, &run, |path, _, state| {
            review::restore(
                path,
                state.stages.get(&stage).context("no archived stage")?,
                &revision,
                &output,
                &to,
            )
        }),
        Command::Repeat {
            workflow,
            run,
            brief,
            input,
        } => quick::repeat(space, &workflow, &run, &brief, &input),
        Command::Learn {
            run,
            stage,
            workflow,
            rule,
            from_rejection: _,
            confirm,
        } => with_run(space, &run, |path, _, state| {
            learning::save(
                space,
                path,
                state,
                learning::Request {
                    run: &run,
                    stage: &stage,
                    workflow: &workflow,
                    rule: rule.as_deref(),
                    confirm: confirm.as_deref(),
                },
            )
        }),
        Command::Define { name, from } => workflow::define(space, &name, &from),
        Command::SaveWorkflow { run, name } => workflow::save(space, &run, &name),
        Command::AdoptReference { run, path: name } => with_run(space, &run, |path, _, state| {
            let plan = Plan::load(path)?;
            policy::require_current(state, &plan)?;
            state.verify_archives(path, false)?;
            let workflow = state
                .workflow
                .as_deref()
                .context("this run has no saved workflow")?;
            workflow::adopt_reference(space, path, &plan, workflow, &name)?;
            println!(
                "Pinned {name}. Use {} status {run} to see affected stages.",
                invocation(&space.root)
            );
            Ok(())
        }),
    }
}

fn dispatch_review(space: &Workspace, command: ReviewCommand) -> Result<()> {
    match command {
        ReviewCommand::Approve {
            run,
            stage,
            revision,
            task,
        } => {
            let terminal = approval::configured_terminal(space)?;
            with_review_target(space, &run, task, |target, path, lock, state| {
                approve(
                    path,
                    lock,
                    state,
                    StageDecision {
                        run: target.run,
                        stage: &stage,
                        revision: &revision,
                    },
                    terminal,
                )
            })
        }
        ReviewCommand::Reject {
            run,
            stage,
            revision,
            note,
            task,
        } => {
            ensure!(
                review::valid_note(&note),
                "provide one line of feedback, up to 1024 UTF-8 bytes"
            );
            let terminal = approval::configured_terminal(space)?;
            with_review_target(space, &run, task, |target, path, lock, state| {
                reject(
                    path,
                    lock,
                    state,
                    StageDecision {
                        run: target.run,
                        stage: &stage,
                        revision: &revision,
                    },
                    note.trim(),
                    terminal,
                )
            })
        }
        ReviewCommand::AuthorizeReviewChange { run, stage, change } => {
            let terminal = approval::configured_terminal(space)?;
            with_run(space, &run, |path, lock, state| {
                let mut plan = Plan::load(path)?;
                state.verify_archives(path, false)?;
                if let Some(terminal) = terminal {
                    let pending = policy::changes(state, &plan)?
                        .into_iter()
                        .find(|pending| {
                            pending.stage == stage && pending.token.as_deref() == Some(&change)
                        })
                        .context("review policy change no longer matches; run status")?;
                    let contract = format!("stages/{stage}/stage.md");
                    terminal.subject(
                        &run,
                        &change,
                        [(stage.as_str(), std::slice::from_ref(&contract))],
                    );
                    eprintln!("Previous decision: {}", pending.previous);
                    eprintln!(
                        "Proposed decision: {}",
                        pending.proposed.unwrap_or("Remove this review requirement")
                    );
                    terminal.confirm("change")?;
                    plan = Plan::load(path)?;
                }
                policy::authorize(state, &plan, &stage, &change)?;
                lock.save(state)?;
                println!(
                    "Recorded the review policy change for {stage}. Use {} status {run}.",
                    invocation(&space.root)
                );
                Ok(())
            })
        }
        ReviewCommand::BatchReview { run } => with_run(space, &run, |_, lock, state| {
            if !state.batch_review {
                state.batch_review = true;
                lock.save(state)?;
            }
            println!(
                "Batch review enabled for {run} and its updates. Dependent work still waits for approval."
            );
            Ok(())
        }),
        ReviewCommand::ApproveBatch {
            run,
            revision,
            task,
        } => {
            let terminal = approval::configured_terminal(space)?;
            with_review_target(space, &run, task, |target, path, lock, state| {
                approve_batch(path, lock, state, target, &revision, terminal)
            })
        }
        ReviewCommand::Review {
            run,
            since,
            retained,
            task,
            interactive,
        } => {
            let terminal = interactive.then(approval::Terminal::require).transpose()?;
            with_review_target(space, &run, task, |target, path, lock, state| {
                let run = target.run;
                let since = if retained {
                    Some(
                        state
                            .update
                            .as_ref()
                            .and_then(|update| update.presentation.as_ref())
                            .context("this run has no retained review comparison")?
                            .token
                            .clone(),
                    )
                } else {
                    since
                };
                ensure!(
                    since.is_none() || state.batch_review,
                    "--since requires batch review"
                );
                let plan = Plan::load(path)?;
                let view = refresh(path, lock, &plan, state)?;
                if state.batch_review && view.stages.iter().any(|item| item.phase == Phase::Review)
                {
                    review::batch(space, path, lock, state, target, &view, since.as_deref())?;
                    if let Some(terminal) = terminal {
                        interactive_review(path, lock, state, target, &view, terminal)?;
                    }
                    Ok(())
                } else if let Some(item) = view.front().filter(|item| item.phase == Phase::Review) {
                    review::gate(space, path, state, target, item, true, None)?;
                    if let Some(next) = view.first_gated_by(item.stage, false) {
                        println!("Gates: {next}");
                    }
                    if let Some(terminal) = terminal {
                        interactive_review(path, lock, state, target, &view, terminal)?;
                    }
                    Ok(())
                } else {
                    println!("No active review in {run}.");
                    print_status(path, run, &view, state);
                    Ok(())
                }
            })
        }
    }
}

fn dispatch_transfer(space: &Workspace, command: TransferCommand) -> Result<()> {
    match command {
        TransferCommand::Handoff { run, json } => handoff::show(&space.run(&run)?, &run, json),
        TransferCommand::Offer { run, attempt } => {
            let executable = std::env::current_exe()?;
            let ticket = with_run(space, &run, |path, lock, state| {
                transfer::issue(path, lock, state, attempt)
            })?;
            println!("{}", transfer::bootstrap(&run, &ticket, &executable));
            Ok(())
        }
        TransferCommand::Claim { run, ticket } => {
            let attempt = with_run(space, &run, |path, lock, state| {
                transfer::claim(path, lock, state, &ticket)
            })?;
            match attempt {
                Some(attempt) => println!("Claimed handoff as attempt {attempt}."),
                None => println!("Claimed handoff."),
            }
            handoff::show(&space.run(&run)?, &run, false)
        }
        TransferCommand::Switch {
            run,
            attempt,
            harness,
        } => {
            let launcher = transfer::Launcher::parse(harness)?;
            let executable = std::env::current_exe()?;
            let ticket = with_run(space, &run, |path, lock, state| {
                transfer::issue(path, lock, state, attempt)
            })?;
            launcher.run(
                &space.root,
                &transfer::bootstrap(&run, &ticket, &executable),
            )?;
            let state = LockedRun::inspect(&space.run(&run)?)?
                .context("run state disappeared after launching the harness")?;
            ensure!(
                transfer::was_claimed(&state, &ticket),
                "receiving harness exited without a confirmed claim. Inspect handoff before retrying"
            );
            Ok(())
        }
        TransferCommand::Checkpoint { run, attempt, note } => {
            with_run(space, &run, |_, lock, state| {
                handoff::checkpoint(state, attempt, &note)?;
                lock.save(state)?;
                println!("Saved progress note for attempt {attempt}.");
                Ok(())
            })
        }
        TransferCommand::Takeover { run, attempt } => with_run(space, &run, |path, lock, state| {
            state.verify_archives(path, false)?;
            let replacement = handoff::takeover(state, attempt)?;
            lock.save(state)?;
            println!("Replaced attempt {attempt} with {replacement}.");
            handoff::show(path, &run, false)
        }),
        TransferCommand::Pack { run, bundle } => with_run(space, &run, |path, _, state| {
            bundle::pack(space, path, &run, state, &bundle)
        }),
        TransferCommand::Unpack { bundle, run } => bundle::unpack(space, &bundle, &run),
    }
}

fn check(space: &Workspace) -> Result<()> {
    let runs = names(&space.root, "runs")?;
    for id in &runs {
        let run = space.run(id)?;
        Plan::load(&run).with_context(|| format!("validate run {id}"))?;
        if let Some(state) = LockedRun::inspect(&run)? {
            state.verify_archives(&run, true)?;
        }
    }
    let workflows = names(&space.root, "workflows")?;
    for id in &workflows {
        let folder = space.workflow(id)?;
        workflow::validate(&folder).with_context(|| format!("validate workflow {id}"))?;
    }
    recurring::check(space)?;
    println!(
        "Workspace v1 is valid ({} runs, {} workflows).",
        runs.len(),
        workflows.len()
    );
    Ok(())
}

fn start(space: &Workspace, id: &str, saved: Option<&str>, adopt: bool) -> Result<()> {
    let run = space.run(id)?;
    let command = invocation(&space.root);
    read_brief(&run)?;
    let has_progress = file(&run, PROGRESS)?.try_exists()?;
    if adopt {
        ensure!(has_progress, "run {id} has no agent-only progress to adopt");
    } else if has_progress {
        ensure!(
            LockedRun::inspect(&run)?.is_some(),
            "run {id} uses agent-only progress. Use {command} adopt {id} when managed execution is authorized"
        );
    }
    if adopt {
        Plan::load(&run)?;
    }
    let lock = LockedRun::acquire(&run, true)?.context("failed to initialize run lock")?;
    if let Some(existing) = lock.read()? {
        ensure!(
            saved.is_none() || existing.workflow.as_deref() == saved,
            "run {id} is registered; its workflow cannot be replaced"
        );
        Plan::load(&run)?;
        existing.verify_archives(&run, false)?;
        lock.ensure_registered_marker()?;
        println!("Run {id} is already registered. Use {command} status {id}.");
        return Ok(());
    }
    if let Some(name) = saved {
        workflow::instantiate(space, &run, name)?;
    }
    let plan = Plan::load(&run)?;
    let mut state = State::new(saved.map(str::to_owned))?;
    policy::capture(&mut state, &plan);
    lock.save(&state)?;
    lock.ensure_registered_marker()?;
    if adopt {
        println!(
            "Registered {id}. Its progress and outputs are intact. Managed stages require their own completion and approval."
        );
    } else {
        println!("Registered run {id}.");
    }
    println!("Use {command} status {id} or {command} next {id}.");
    Ok(())
}

fn with_review_target<T>(
    space: &Workspace,
    name: &str,
    task: bool,
    operation: impl FnOnce(review::Target<'_>, &Path, &LockedRun, &mut State) -> Result<T>,
) -> Result<T> {
    let execute = |run: &str| {
        with_run(space, run, |path, lock, state| {
            operation(
                review::Target {
                    run,
                    task: task.then_some(name),
                },
                path,
                lock,
                state,
            )
        })
    };
    if task {
        recurring::with_current(space, name, execute)
    } else {
        execute(name)
    }
}

fn with_run<T>(
    space: &Workspace,
    id: &str,
    operation: impl FnOnce(&Path, &LockedRun, &mut State) -> Result<T>,
) -> Result<T> {
    let path = space.run(id)?;
    let lock =
        LockedRun::acquire(&path, false)?.with_context(|| format!("run {id} is not registered"))?;
    let mut state = lock
        .read()?
        .with_context(|| format!("run {id} is not registered"))?;
    if state.upgrade()? {
        let plan = Plan::load(&path)?;
        policy::capture(&mut state, &plan);
        state.verify_archives(&path, true)?;
        lock.save(&state)?;
    }
    lock.ensure_registered_marker()?;
    operation(&path, &lock, &mut state)
}

fn refresh<'a>(
    path: &Path,
    lock: &LockedRun,
    plan: &'a Plan,
    state: &mut State,
) -> Result<View<'a>> {
    state.verify_archives(path, false)?;
    policy::require_current(state, plan)?;
    let captured = policy::capture(state, plan);
    let view = evaluate(path, plan, state, true)?;
    if view.changed || captured {
        lock.save(state)?;
    }
    Ok(view)
}

fn status(space: &Workspace, id: &str, inspect: bool, diff: bool) -> Result<()> {
    let path = space.run(id)?;
    ensure!(path.try_exists()?, "run {id} does not exist");
    if inspect {
        if let Some(mut state) = LockedRun::inspect(&path)? {
            let plan = Plan::load(&path)?;
            state.verify_archives(&path, false)?;
            if !print_policy(&path, id, &state, &plan)? {
                let view = evaluate(&path, &plan, &mut state, false)?;
                print_status(&path, id, &view, &state);
                if diff
                    && let Some(item) = view.action(state.batch_review)
                    && item.phase == Phase::Review
                    && item.revision.is_none()
                    && let Some(entry) = state.stages.get(&item.stage.id)
                {
                    review::working_diff(&path, entry, &item.stage.id)?;
                }
            }
            return Ok(());
        }
    } else if let Some(lock) = LockedRun::acquire(&path, false)?
        && let Some(mut state) = lock.read()?
    {
        let plan = Plan::load(&path)?;
        state.verify_archives(&path, false)?;
        if state.upgrade()? {
            policy::capture(&mut state, &plan);
            lock.save(&state)?;
        }
        lock.ensure_registered_marker()?;
        if !print_policy(&path, id, &state, &plan)? {
            let view = refresh(&path, &lock, &plan, &mut state)?;
            print_status(&path, id, &view, &state);
        }
        return Ok(());
    }
    if file(&path, PROGRESS)?.try_exists()? {
        println!(
            "Run {id} | agent-only. Resume from runs/{id}/brief.md and runs/{id}/progress.md."
        );
    } else {
        println!(
            "Run {id} | unregistered. Inspect runs/{id}/brief.md and stage contracts before starting."
        );
    }
    Ok(())
}

fn print_policy(path: &Path, run: &str, state: &State, plan: &Plan) -> Result<bool> {
    let changes = policy::changes(state, plan)?;
    if !changes.is_empty() {
        let command = run_invocation(path);
        println!("Run {run} | blocked by changes to its review policy");
        for change in &changes {
            println!(
                "  {} previously required: {}",
                change.stage, change.previous
            );
            println!(
                "  Proposed decision: {}",
                change.proposed.unwrap_or("Remove this review requirement")
            );
            if let Some(token) = &change.token {
                println!(
                    "  If the human authorizes the policy change: {command} authorize-review-change {run} {} --change {token}",
                    change.stage
                );
            }
        }
        println!(
            "Next: restore the previous review requirement or obtain explicit authorization for the change."
        );
    }
    Ok(!changes.is_empty())
}

fn print_status(path: &Path, run: &str, view: &View<'_>, state: &State) {
    let command = run_invocation(path);
    println!("Run {run} | brief: runs/{run}/brief.md");
    if let Some(update) = &state.update {
        println!("Updated from: runs/{}", update.from);
        for path in &update.inputs {
            println!("Changed at update: runs/{run}/{path}");
        }
    }

    if let Some(presentation) = state
        .update
        .as_ref()
        .and_then(|update| update.presentation.as_ref())
        && view.stages.iter().any(|item| item.phase == Phase::Review)
    {
        println!(
            "To compare with the retained presentation: {command} review {run} --since {}",
            presentation.token
        );
    }
    let front = view.action(state.batch_review);
    for item in &view.stages {
        if item.phase == Phase::Ready
            && let Some(first) = front
            && first.stage.id != item.stage.id
        {
            println!(
                "{} | blocked by earlier stage {}",
                item.stage.id, first.stage.id
            );
            continue;
        }
        println!("{} | {}", item.stage.id, item.phase.label());
        if let Some(update) = &state.update
            && let Some(reused) = update.reused.get(&item.stage.id)
            && item.revision.as_deref() == Some(&reused.revision)
        {
            println!("  Reused from runs/{}", update.from);
        }
        for reason in &item.reasons {
            println!("  {reason}");
        }
        for check in &item.checks {
            println!("  Check passed: {check}");
        }
        if let Some(attempt) = item.attempt {
            println!("  Attempt: {attempt}");
            for output in &item.stage.fields.outputs {
                println!("  Working file: runs/{run}/{output}");
            }
        }
        for reference in &item.stage.fields.references {
            println!("  Reference: runs/{run}/{}", reference.label());
        }
        if item.phase == Phase::Done {
            for output in &item.stage.fields.outputs {
                println!("  Output: runs/{run}/{output}");
            }
        }
        if item.phase == Phase::Review {
            for output in &item.stage.fields.outputs {
                println!("  Review file: runs/{run}/{output}");
            }
            if let Some(decision) = &item.stage.fields.review {
                println!("  Decision: {decision}");
            }
            if let Some(revision) = &item.revision {
                println!("  Compare: {command} diff {run} {}", item.stage.id);
                if !state.batch_review
                    && let Some(token) = state.review_token(&item.stage.id, revision)
                {
                    println!("  Revision: {token}");
                }
            }
            if let Some(next) = view.first_gated_by(item.stage, state.batch_review) {
                println!("  Gates: {next}");
            }
        }
    }
    if state.batch_review
        && let Ok((token, _)) = review::pending_batch(view, state)
    {
        println!("Batch revision: {token}");
    }
    for prior in state.stages.keys() {
        if !view.stages.iter().any(|item| &item.stage.id == prior) {
            println!("{prior} | removed from the plan; archived revisions remain available");
        }
    }
    match front {
        None => println!("Run {run} is complete."),
        Some(item) => match item.phase {
            Phase::Running => println!(
                "Next: resume {} from runs/{run}/{} with attempt {}.",
                item.stage.id,
                item.stage.contract_path(),
                item.attempt.unwrap_or_default()
            ),
            Phase::Review if state.batch_review => println!("Next: {command} review {run}"),
            Phase::Review => {
                let token = item
                    .revision
                    .as_deref()
                    .and_then(|id| state.review_token(&item.stage.id, id));
                match token {
                    Some(token) => println!(
                        "Next: human review. If approved, record: {command} approve {run} {} --revision {token}",
                        item.stage.id
                    ),
                    None => println!(
                        "Next: use {command} review {run} to record and present the current revision."
                    ),
                }
            }
            Phase::Ready => println!("Next: {command} next {run}"),
            Phase::Stale if item.basis.is_some() => {
                println!("Next: inspect the changes, then {command} next {run} --retry")
            }
            _ => println!(
                "Next: resolve the listed files or decisions blocking {}.",
                item.stage.id
            ),
        },
    }
}

fn require_action(view: &View<'_>, stage: &str, batch: bool) -> Result<()> {
    let front = view.action(batch).context("all stages are complete")?;
    ensure!(
        front.stage.id == stage,
        "{} is first unresolved ({})",
        front.stage.id,
        front.phase.label()
    );
    Ok(())
}

fn find_stage<'a>(view: &'a View<'_>, stage: &str) -> Result<&'a StageView<'a>> {
    view.stages
        .iter()
        .find(|item| item.stage.id == stage)
        .with_context(|| format!("unknown stage {stage}"))
}

fn complete(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    run: &str,
    stage: &str,
    attempt: u64,
    quiet: bool,
) -> Result<()> {
    let plan = Plan::load(path)?;
    let view = refresh(path, lock, &plan, state)?;
    let item = find_stage(&view, stage)?;
    if state.stages.get(stage).is_some_and(|entry| {
        entry.attempt.is_none() && entry.last_completed_attempt == Some(attempt)
    }) {
        println!("Attempt {attempt} was already recorded.");
        if !quiet {
            print_status(path, run, &view, state);
        }
        return Ok(());
    }
    require_action(&view, stage, state.batch_review)?;
    let revision = engine::prepare_completion(path, item, attempt)?;

    let plan = Plan::load(path)?;
    let latest = refresh(path, lock, &plan, state)?;
    require_action(&latest, stage, state.batch_review)?;
    let item = find_stage(&latest, stage)?;
    ensure!(
        item.phase == Phase::Running && item.basis.as_ref() == Some(&revision.basis),
        "stage inputs changed during completion; inspect status"
    );
    for (name, hash) in &revision.outputs {
        ensure!(
            live_hash(path, name)?.as_ref() == Some(hash),
            "{name} changed during completion; inspect status"
        );
    }
    let entry = state
        .stages
        .get_mut(stage)
        .context("missing active stage")?;
    entry.record(revision)?;
    entry.mark_finalized(item.stage.fields.review.is_none());
    entry.attempt = None;
    entry.last_completed_attempt = Some(attempt);
    lock.save(state)?;
    if quiet {
        println!("Completed {stage} at attempt {attempt}.");
    } else {
        let view = refresh(path, lock, &plan, state)?;
        print_status(path, run, &view, state);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StageDecision<'a> {
    run: &'a str,
    stage: &'a str,
    revision: &'a str,
}

fn approve(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    decision: StageDecision<'_>,
    terminal: Option<approval::Terminal>,
) -> Result<()> {
    let StageDecision {
        run,
        stage,
        revision,
    } = decision;
    let plan = Plan::load(path)?;
    let view = refresh(path, lock, &plan, state)?;
    let item = find_stage(&view, stage)?;
    let archive_id = item
        .revision
        .as_deref()
        .context("stage has no recorded revision; use status")?;
    let current = state
        .review_token(stage, archive_id)
        .context("missing review identity")?;
    ensure!(
        review::matches_token(revision, &current, review::stage_tokens(state, stage)),
        "revision changed or is ambiguous for {stage}; use review to see the current files"
    );
    if item.phase == Phase::Done
        && state
            .stages
            .get(stage)
            .is_some_and(|entry| entry.approvals.contains(archive_id))
    {
        println!("Revision {revision} is already approved.");
        return Ok(());
    }
    if !state.batch_review {
        require_action(&view, stage, false)?;
    }
    if item.phase != Phase::Review {
        bail!(
            "{stage} is {}; use status to inspect the review gate",
            item.phase.label()
        );
    }
    if let Some(terminal) = terminal {
        terminal.subject(
            run,
            &current,
            [(stage, item.stage.fields.outputs.as_slice())],
        );
        terminal.confirm("approve")?;
        return approve(
            path,
            lock,
            state,
            StageDecision {
                revision: &current,
                ..decision
            },
            None,
        );
    }
    let entry = state.stages.get_mut(stage).context("missing stage")?;
    entry.approvals.insert(archive_id.into());
    entry.mark_finalized(true);
    lock.save(state)?;
    println!("Approved revision {current} for {stage}.");
    let view = refresh(path, lock, &plan, state)?;
    print_status(path, run, &view, state);
    Ok(())
}

fn reject(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    decision: StageDecision<'_>,
    note: &str,
    terminal: Option<approval::Terminal>,
) -> Result<()> {
    let StageDecision {
        run,
        stage,
        revision: supplied,
    } = decision;
    ensure!(
        review::valid_note(note),
        "provide one line of feedback, up to 1024 UTF-8 bytes"
    );
    let plan = Plan::load(path)?;
    let view = refresh(path, lock, &plan, state)?;
    let item = find_stage(&view, stage)?;
    let entry = state
        .stages
        .get(stage)
        .context("stage has no recorded review")?;
    let archive = entry
        .head
        .as_deref()
        .context("stage has no recorded review")?;
    let token = state
        .review_token(stage, archive)
        .context("missing review identity")?;
    ensure!(
        review::matches_token(supplied, &token, review::stage_tokens(state, stage)),
        "revision changed or is ambiguous for {stage}; use review to see the current files"
    );
    if let Some(previous) = entry.rejections.get(archive) {
        ensure!(
            previous == note,
            "changes were already requested for this revision with a different note"
        );
        println!("Changes are already requested for {stage}.");
        return Ok(());
    }
    ensure!(
        item.phase == Phase::Review && item.revision.as_deref() == Some(archive),
        "{stage} is not waiting for this review"
    );
    if !state.batch_review {
        require_action(&view, stage, false)?;
    }
    if let Some(terminal) = terminal {
        terminal.subject(run, &token, [(stage, item.stage.fields.outputs.as_slice())]);
        terminal.confirm("reject")?;
        return reject(
            path,
            lock,
            state,
            StageDecision {
                revision: &token,
                ..decision
            },
            note,
            None,
        );
    }
    let archive = archive.to_owned();
    state.record_rejection(stage, archive, note)?;
    lock.save(state)?;
    println!("Changes requested for {stage}. Its draft and revision are preserved.");
    println!(
        "When rework is authorized, run {} next {run}.",
        run_invocation(path)
    );
    Ok(())
}

fn approve_batch(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    target: review::Target<'_>,
    revision: &str,
    terminal: Option<approval::Terminal>,
) -> Result<()> {
    let run = target.run;
    ensure!(state.batch_review, "batch review is disabled for {run}");
    let plan = Plan::load(path)?;
    let view = refresh(path, lock, &plan, state)?;
    if let Some(last) = &state.last_batch
        && review::matches_token(revision, last, review::batch_tokens(state))
    {
        println!("Batch {revision} is already approved.");
        return Ok(());
    }
    let (token, pending) = review::pending_batch(&view, state)?;
    ensure!(
        review::matches_token(revision, &token, review::batch_tokens(state)),
        "review batch changed. Use {} review {}.",
        run_invocation(path),
        target.selector()
    );
    if let Some(terminal) = terminal {
        terminal.subject(
            run,
            &token,
            view.stages
                .iter()
                .filter(|item| item.phase == Phase::Review)
                .map(|item| (item.stage.id.as_str(), item.stage.fields.outputs.as_slice())),
        );
        terminal.confirm("approve")?;
        return approve_batch(path, lock, state, target, &token, None);
    }
    for (stage, archive) in &pending {
        let entry = state.stages.get_mut(stage).context("missing stage")?;
        entry.approvals.insert(archive.clone());
        entry.mark_finalized(true);
    }
    state.last_batch = Some(token);
    lock.save(state)?;
    println!("Approved {} exact revisions for {run}.", pending.len());
    Ok(())
}

fn interactive_review(
    path: &Path,
    lock: &LockedRun,
    state: &mut State,
    target: review::Target<'_>,
    view: &View<'_>,
    terminal: approval::Terminal,
) -> Result<()> {
    let stages: Vec<_> = if state.batch_review {
        view.stages
            .iter()
            .filter(|item| item.phase == Phase::Review)
            .collect()
    } else {
        view.front().into_iter().collect()
    };
    let revision = |item: &StageView<'_>| {
        item.revision
            .as_deref()
            .and_then(|archive| state.review_token(&item.stage.id, archive))
            .context("review revision is unavailable")
    };
    let first = stages.first().context("no pending reviews")?;
    let token = if state.batch_review {
        review::pending_batch(view, state)?.0
    } else {
        revision(first)?
    };
    terminal.subject(
        target.run,
        &token,
        stages
            .iter()
            .map(|item| (item.stage.id.as_str(), item.stage.fields.outputs.as_slice())),
    );
    match terminal.choose()? {
        approval::Choice::Skip => {
            println!("Review skipped. No decision recorded.");
            Ok(())
        }
        approval::Choice::Approve if state.batch_review => {
            approve_batch(path, lock, state, target, &token, None)
        }
        approval::Choice::Approve => approve(
            path,
            lock,
            state,
            StageDecision {
                run: target.run,
                stage: &first.stage.id,
                revision: &token,
            },
            None,
        ),
        approval::Choice::Reject => {
            let selected = if stages.len() == 1 {
                first
            } else {
                let stage =
                    terminal.read("Which pending stage needs changes? Enter the stage ID.")?;
                stages
                    .iter()
                    .find(|item| item.stage.id == stage)
                    .context("that stage is not pending review")?
            };
            let selected_token = revision(selected)?;
            let note = terminal.read("What should change? Enter one line of feedback.")?;
            reject(
                path,
                lock,
                state,
                StageDecision {
                    run: target.run,
                    stage: &selected.stage.id,
                    revision: &selected_token,
                },
                &note,
                None,
            )
        }
    }
}
