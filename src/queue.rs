use crate::{
    engine::{self, Phase},
    policy, recurring,
    workspace::{Plan, Workspace},
};
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
struct Queue {
    schema: u32,
    tasks: Vec<Task>,
}

#[derive(Serialize)]
struct Task {
    name: String,
    run: Option<String>,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reviews: Vec<Review>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_changes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

#[derive(Serialize)]
struct Review {
    stage: String,
    decision: String,
    files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gates: Option<String>,
}

pub fn show(space: &Workspace, all: bool, json: bool, sources: bool) -> Result<()> {
    let mut tasks = Vec::new();
    for name in recurring::tracked_names(space)? {
        match inspect(space, &name, sources) {
            Ok(Some(task)) if all || task.state != "done" => tasks.push(task),
            Ok(_) => {}
            Err(error) => tasks.push(Task {
                name,
                run: None,
                state: "unavailable",
                stage: None,
                reasons: vec![format!("{error:#}")],
                reviews: Vec::new(),
                source_changes: Vec::new(),
                source_error: None,
                next: None,
            }),
        }
    }
    tasks.sort_by_key(|task| match task.state {
        "awaiting review" | "review policy change" => 0,
        "unavailable" | "blocked" | "stale" | "source unavailable" => 1,
        "done" => 3,
        _ => 2,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Queue { schema: 1, tasks })?
        );
        return Ok(());
    }
    if tasks.is_empty() {
        println!("No recurring tasks need attention.");
    }
    for task in tasks {
        let run = task.run.map_or_else(String::new, |run| format!(" | {run}"));
        let stage = task
            .stage
            .map_or_else(String::new, |stage| format!(" | {stage}"));
        println!("{} | {}{run}{stage}", task.name, task.state);
        for review in task.reviews {
            println!("  {} | {}", review.stage, review.decision);
            for file in review.files {
                println!("  Review file: {file}");
            }
            if let Some(gates) = review.gates {
                println!("  Gates: {gates}");
            }
        }
        for reason in task.reasons {
            println!("  {reason}");
        }
        for path in task.source_changes {
            println!("  Changed in saved source: {path}");
        }
        if let Some(error) = task.source_error {
            println!("  Saved source: {error}");
        }
        if let Some(next) = task.next {
            println!("  Next: {next}");
        }
    }
    Ok(())
}

fn inspect(space: &Workspace, name: &str, sources: bool) -> Result<Option<Task>> {
    let Some(current) = recurring::inspect(space, name)? else {
        return Ok(None);
    };
    let scan = sources.then(|| current.changed_sources(space, name));
    let run = current.run;
    let mut state = current.state;
    let root = space.run(&run)?;
    let plan = Plan::load(&root)?;
    state.verify_archives(&root, false)?;
    let mut task = Task {
        name: name.into(),
        run: Some(run.clone()),
        state: "done",
        stage: None,
        reasons: Vec::new(),
        reviews: Vec::new(),
        source_changes: Vec::new(),
        source_error: None,
        next: None,
    };
    match scan {
        Some(Ok(Some(changes))) => task.source_changes = changes.files.into_keys().collect(),
        Some(Err(error)) => task.source_error = Some(format!("{error:#}")),
        Some(Ok(None)) | None => {}
    };
    let changes = policy::changes(&state, &plan)?;
    if !changes.is_empty() {
        task.state = "review policy change";
        task.reasons = changes
            .iter()
            .map(|change| match change.proposed {
                Some(proposed) => format!(
                    "{} changes {:?} to {proposed:?}.",
                    change.stage, change.previous
                ),
                None => format!("{} removes {:?}.", change.stage, change.previous),
            })
            .collect();
        task.next = Some(format!("anydock status {run}"));
        return Ok(Some(task));
    }

    let view = engine::evaluate(&root, &plan, &mut state, false)?;
    if let Some(item) = view.action(state.batch_review) {
        task.stage = Some(item.stage.id.clone());
        task.state = item.phase.label();
        task.reasons.clone_from(&item.reasons);
        task.next = Some(match item.phase {
            Phase::Review => format!("anydock review {run}"),
            Phase::Ready | Phase::Running => format!("anydock next {run} --context"),
            Phase::Stale if item.basis.is_some() => format!("anydock next {run} --retry"),
            _ => format!("anydock status {run} --inspect"),
        });
        if item.phase == Phase::Review {
            task.reviews = view
                .stages
                .iter()
                .filter(|review| {
                    review.phase == Phase::Review
                        && (state.batch_review || review.stage.id == item.stage.id)
                })
                .filter_map(|review| {
                    Some(Review {
                        stage: review.stage.id.clone(),
                        decision: review.stage.fields.review.as_ref()?.clone(),
                        files: review
                            .stage
                            .fields
                            .outputs
                            .iter()
                            .map(|path| format!("runs/{run}/{path}"))
                            .collect(),
                        gates: view
                            .first_gated_by(review.stage, state.batch_review)
                            .map(str::to_owned),
                    })
                })
                .collect();
        }
    }
    if task.state == "done" {
        if task.source_error.is_some() {
            task.state = "source unavailable";
        } else if !task.source_changes.is_empty() {
            task.state = "source changed";
            task.next = Some(format!("anydock refresh {name} --submit"));
        }
    }
    Ok(Some(task))
}
