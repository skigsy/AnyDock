use crate::{
    storage::{State, hash},
    workspace::Plan,
};
use anyhow::{Context, Result, ensure};

pub struct Change<'a> {
    pub stage: &'a str,
    pub previous: &'a str,
    pub proposed: Option<&'a str>,
    pub token: Option<String>,
}

pub fn capture(state: &mut State, plan: &Plan) -> bool {
    let count = state.review_requirements.len();
    for stage in &plan.stages {
        if let Some(decision) = &stage.fields.review {
            state
                .review_requirements
                .entry(stage.id.clone())
                .or_insert_with(|| decision.clone());
        }
    }
    state.review_requirements.len() != count
}

pub fn changes<'a>(state: &'a State, plan: &'a Plan) -> Result<Vec<Change<'a>>> {
    let mut changes = Vec::new();
    for (name, previous) in &state.review_requirements {
        let stage = plan.stages.iter().find(|stage| &stage.id == name);
        let proposed = stage.and_then(|stage| stage.fields.review.as_deref());
        if proposed == Some(previous.as_str()) {
            continue;
        }
        let basis = serde_json::to_vec(&(
            "review-policy",
            state.identity.as_deref(),
            name,
            previous,
            proposed,
            hash(&plan.brief),
            stage.map(|stage| hash(&stage.contract)),
        ))?;
        changes.push(Change {
            stage: name,
            previous,
            proposed,
            token: state.identity.as_ref().map(|_| hash(&basis)),
        });
    }
    Ok(changes)
}

pub fn require_current(state: &State, plan: &Plan) -> Result<()> {
    let changes = changes(state, plan)?;
    ensure!(
        changes.is_empty(),
        "pending review policy change for {}; run status to see the decision",
        changes
            .iter()
            .map(|change| change.stage)
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

pub fn authorize(state: &mut State, plan: &Plan, stage: &str, token: &str) -> Result<()> {
    let changes = changes(state, plan)?;
    let change = changes
        .into_iter()
        .find(|change| change.stage == stage)
        .context("no pending review policy change for this stage")?;
    ensure!(
        change.token.as_deref() == Some(token),
        "review policy change no longer matches; run status"
    );
    let proposed = change.proposed.map(str::to_owned);
    if let Some(decision) = proposed {
        state.review_requirements.insert(stage.to_owned(), decision);
    } else {
        state.review_requirements.remove(stage);
    }
    Ok(())
}
