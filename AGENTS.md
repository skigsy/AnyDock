# AnyDock

This checkout is idle. Work on the user's request. Planning, discussion, and inspection stay read-only. Create or advance a run only with authorization. Maintenance of AnyDock itself is ordinary repository work.

## Start and resume

- Read the relevant user files. Reuse a matching run. For a new one-off, use `anydock run --help` or create `runs/<run>/brief.md` and the staged contracts below. Record the task, constraints, deliverable and authorization. Snapshot selected inputs and keep originals intact.
- In a release archive, run `.\bin\anydock.exe` on Windows or `./bin/anydock` on Linux and macOS. For a source checkout, use `anydock` on PATH or the executable you built. State whether the run is managed. Register a new managed run with `start`. For an existing run, begin with `handoff <run>`. Read its brief and named files. Summarize what is done, what changed, any blocker, and the next authorized action.
- For authorized work, use `next <run>`, optionally with `--context`. Use the returned attempt and declared files. When a contract selects JSON fields, use the printed input command to read the selection. Read the named reference or section when required. Write and verify the declared outputs, then use the printed completion command.
- Keep a short checkpoint when stopping partway through an attempt. Stop the outgoing writer before an authorized transfer. Use `takeover` with the active attempt to fence it, or follow an explicit `claim` bootstrap. A transfer does not approve a review.
- Treat managed state, registration markers, and archives as authoritative. If they are incomplete or the CLI is unavailable, surface the problem. Restore a complete copy before advancing.

## Human decisions

- At a gate, use `review <run>`. Show the editable files, the decision, the exact current revision, and what it gates. Open the full files when a preview is incomplete. Record approval or rejection only on explicit authorization for that revision. Show the prior and proposed decisions for a review-policy change and wait for exact authorization. An edit reopens review where required.
- For recurring work, use `queue --sources` or the optional daily brief to inspect changes. Only submit the packet the user authorizes. Group independent deliverables into separate stages so unchanged approvals carry forward. Promote a correction into a saved workflow only after the user explicitly authorizes the proposed guidance. Read USAGE.md for command details when needed.

## Agent-only operation

With no executable, keep the same file layout and state that gates are agent-maintained. Record the active stage, authorization and digests of the brief, contracts, and declared inputs in `progress.md`. Verify the digests before finishing, archive each output revision, and bind every decision to the exact output digest. On resume, compare live files and reopen changed reviews or declared consumers. Keep progress beside the run so a new session can continue.

## File contracts

Use lowercase path-safe run IDs and unique stage numbers `01` through `99`. Give each stage one `stages/<number>-<label>/stage.md`. In a fenced `anydock` JSON section, set `inputs` to required run-relative inputs or earlier outputs. Set `outputs` to at least one file beneath this stage's `outputs/`. Set `review` to a decision question or `null`. Follow the fence with `## Work` and the task-specific instructions. Use forward slashes and declare references and checks only when the stage needs them. Store task facts in the brief and input files.

Keep the workspace empty until the user provides work. Set up the daily brief only when they ask. Follow its exact submission links and ordinary review gates.
