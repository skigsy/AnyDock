# AnyDock CLI and workspace guide

[Start here](README.md) for one-prompt setup.

## Install in a new workspace

Open the extracted `AnyDock-0.2.0/` folder for a blank workspace. To use an existing project, run the executable with `-C <project> init`. Initialization preserves the project's instructions and adapters. Ask your agent to read the AnyDock instructions from the extracted package when the project has its own instruction file.

To use a newer binary with your existing workspace, run `anydock -C <workspace> upgrade --check`. A compatible workspace keeps its runs, workflow files, and approvals. For an earlier layout with a different `.version` marker, use `upgrade --check --from-marker <filename>` while the workspace is stopped. Use the same command without `--check` to publish the upgrade in place and preserve a verified sibling backup. Open that backup with the prior executable if you need to resume from the pre-upgrade state.

## Managed runs

The source workspace works immediately through `AGENTS.md`. Its agent-only mode keeps a Markdown progress record. The Rust executable adds file validation, input version tracking, archived revisions, and enforced review gates for managed runs. The agent tells you which mode it is using.

The brief defines the task and authorization. Add free-form working notes in `runs/<run>/notes.md` to keep them available at handoff without changing the review basis. Declare any notes that a stage must consume under `inputs/` instead.

The Windows executable package includes `bin\anydock.exe`. The Linux x64 and macOS ARM64 packages include `bin/anydock`. For a source download, from this folder with Rust 1.89 or later and your platform's linker installed, run `cargo build --locked --release`. On Windows, the executable is `target\release\anydock.exe`. Elsewhere it is `target/release/anydock`. Run it from the workspace root, or put it on your PATH. `cargo run --locked -- <command>` also works from the root.

- `anydock check` validates the workspace and its definitions. A blank checkout passes.
- `anydock status` lists local runs without writing state. Add a run name for its next action, and `--diff` to preview unrecorded edits. Use `review <run>` to record and present the current revision, or `status <run> --reconcile` to record edits explicitly.
- `anydock init` makes an existing project folder ready for managed work. It preserves a project’s `AGENTS.md` and existing ignore rules. If a managed folder name collides, initialize in a dedicated folder. Commands discover the nearest workspace from child folders. Use `anydock -C <directory> status` to select a different workspace.
- `anydock adopt <run>` registers an existing agent-only run when managed work is authorized. It keeps the brief, progress notes, stage contracts and output files. Existing files still need managed completion and any required review.
- `anydock review <run>` shows the exact revision and a copyable approval command. `approve` accepts the full ID or the printed unique prefix. `reject <run> <stage> --revision <id> --note <text>` preserves the draft and opens rework. After the revised result is approved, `learn --from-rejection` can preview that feedback as reusable guidance.
- Use `anydock review <run> --interactive` to preview and approve, reject, or skip in one terminal. A batch approval applies to every listed stage. A batch rejection asks which stage needs changes.
- To require terminal confirmation for approvals, rejections, and review-policy changes, create `.anydock/approval.json` with `{"mode":"human-tty"}`. A direct approval shows the files, stage, and revision prefix before asking for `approve` or `y`. Terminal gating assumes ordinary access controls for the workspace and OS account.
- `anydock diff <run> <stage>` compares recorded handoffs. `history` shows archive IDs, and `restore` writes a verified copy to `recovered/`.
- `anydock --help` lists the commands your agent uses to run stages and save reusable definitions.

Inputs, handoffs, run records, saved workflows, and run bundles are ignored by Git.

To move a managed run, stop edits, then run `anydock pack <run> <bundle>`. Copy `bundles/<bundle>/` into the receiving workspace’s `bundles/` and run `anydock unpack <bundle> <run>`. The bundle contains the brief, stage contracts, declared files, archives, and review records. Keep it private as you would the run. After import, use the receiving copy for further work. For a full backup that includes undeclared files, copy the stopped run folder itself.


## Make the next run better

When you want to keep a method, save it with `anydock save-workflow <run> <name>`. After you edit and approve a result, tell the agent which correction should carry forward. It previews the wording and destination with `anydock learn <run> <stage> --workflow <name> --rule <text>`. Confirm that exact proposal to save it as editable Markdown for the matching stage. Review approval and saving guidance are separate decisions.

Next time, tell the agent to use the saved workflow with your new files. It can start in one step with `anydock repeat <name> <new-run> --brief <task> --input inbox/<new-file>=inputs/<expected-file>`. Only the new brief, selected files, saved stage definitions, and their guidance are used. Existing runs keep their copies.

## Set up and review repeat work

Use `anydock define <name> --from inbox/<definition>.json` to create a saved workflow in one step. The JSON contains a `stages` array. Each stage has `id`, `work`, and the same fields as an ordinary stage contract. Referenced files live beside the definition under their declared paths. Start the first job with `repeat` and use `update` when its sources change.

Use `anydock review <run>` at the review gate. It shows the exact decision and revision and compares the new draft with the previous approved output when available. Inline changes show the removed value in `[- -]` and the replacement in `{+ +}`. Review the linked full file if the preview is incomplete.

Review flags local Markdown links whose full bytes are outside the revision. To bind an appendix to the decision, declare it as an output or a whole-file input.

For independent stages, `anydock batch-review <run>` groups their decisions into one review session. Execute one stage at a time with `next` and `complete`. Dependent stages wait for their required approvals. When ready, `review <run>` shows each pending file and one exact batch token. Use `approve-batch <run> --revision <token>` after explicitly approving the displayed group. The setting carries forward when you update the run.

If the files for a recurring task already land in the same local folder, start it with
`anydock begin <task> --workflow <saved-workflow> --brief "<task>" --source "<folder>"`.
Then submit the latest packet with `anydock refresh <task> --submit`. AnyDock reads
the exact filenames declared by the workflow, keeps a verified copy in each run,
and leaves the original files in place. Use `anydock refresh <task>` to resume
the current run. Its files remain usable if the source folder is offline. Run
`anydock queue --sources` to see which tasks have new source files and which
already have a pending decision. The scan does not submit or approve work.

On the next submitted cycle, a saved workflow's approved guidance is copied into the affected stages. Its previous runs keep their pinned material and review records. A one-off JSON definition can still be used with `begin --definition`.

## Refresh a completed run

Ask the agent to update a completed run with your new files. It uses `anydock update <previous-run> <new-run> --input inbox/<new-file>=inputs/<declared-file>`. Pass `--inbox` instead when you have a complete set of files with the same names in inbox/. AnyDock copies forward verified output and approval when all declared inputs still match, including upstream results. It starts the first affected stage, shows what needs work, and keeps the previous run available. Any new approval applies to the new revision.

For local automation, `anydock session` accepts one JSON request per line with a fresh random `id` and an `args` array of ordinary CLI arguments. The command output is followed by an ASCII record separator and a JSON response with that `id`, success status, and any error. Send the next request after receiving its predecessor's response. The run stays on disk across requests, including the usual locks and review gates. Close stdin to exit.

If a source changes during a batch review, use `update <previous-run> <new-run> --pending` with the corrected input mapping. Matching recorded drafts carry forward with their existing approval status. After finishing the changed steps, use `review <new-run> --since <earlier-batch-token>` when the reviewer has already seen that earlier presentation. It shows changed draft content and identifies unchanged pending drafts. Approve only the current batch token. Add `--since <earlier-batch-token>` to the pending update to keep that comparison inside the new run. It stays available through more updates that retain the same token, including after the older folders are archived or the new run is moved in a verified bundle.

## Continue in another harness

Stop the outgoing agent before the incoming agent edits. Open the same folder and use `anydock handoff <run>` to read progress and the next action. For an active stage, record a short note with `checkpoint` before stopping. Use `takeover <run> --attempt <id>` when an authorized transfer should fence the previous attempt. The optional launch helpers below carry the same files and decision gates.

- **Already have the receiving session open?** Open the same workspace and paste the message from `anydock offer <run> --attempt <id>`.
- **Launching from a terminal?** Use `anydock switch <run> --attempt <id> -- <executable> <arguments> "{handoff}"`. Put the placeholder where the receiving CLI takes its prompt. On Windows use its executable or the runtime that loads its entry file. Your usual login and permission settings apply.

Omit `--attempt` when no stage is active, including at human review. The receiving agent runs the printed `claim` command, which gives it the current handoff. An active attempt changes only when that claim commits. If the receiving process exits without claiming, the switch command reports it. The ticket is valid for 30 minutes or until the relevant state changes. For read-only inspection, use `anydock handoff <run>` or add `--json`.

`AGENTS.md` holds the workspace rules. The shipped instruction adapters point to it. If your agent does not discover workspace instructions, ask it to read `AGENTS.md` and run the handoff command.

For an authorized single-stage task, `anydock run` can register and begin in one command. Use `--source <file>` to snapshot a selected document by its original name, or `--input` for files in the workspace inbox. For an existing run, `next --context` returns the current attempt and a bounded preview of the declared material.

### Local MCP

In an MCP client, configure a local stdio server with the absolute executable path and the argument array `["-C", "<workspace>", "mcp"]`. Use `status {}` to discover runs and `handoff` for a named run. Use `queue {"sources":true}` for recurring tasks and changed source files. Add `--allow-work` to the argument array to expose `next`, `complete`, `review`, and exact-source `refresh` for authorized work. The server uses local files and closes with the client. Human decisions remain on the terminal approval path.

## Start or refresh a recurring task

From an empty workspace, use `anydock begin <task> --definition inbox/<definition>.json --brief "<task>"`. The definition uses the same format as `define`. Put the first complete input set in `inbox/` or select a subfolder with `--inbox`. The task enters the queue as soon as its first stage starts. Pass `--batch-review` to group independent reviews.

Run `anydock queue` to see which named tasks need a review or other action. It lists the current run, review files and decision without changing state. Use `--all` to include completed tasks or `--json` for local tools. Use the listed review command to present the exact revision.

Attach a name to a completed run with `anydock refresh <task> --from <completed-run>`. When the next complete set of input files is ready, use `anydock refresh <task> --inbox`, or give an inbox subfolder. AnyDock finds the latest run and opens work for the new input. The same submission returns the existing run with its current attempt or review. A different submission waits until the current run finishes.

Use `anydock refresh <task>` to find the current run after a break. Keep the latest run and the workspace's local tracking record together. A named refresh keeps the seed's task and contracts. Start from a new completed run under a new name when the task changes.

For a one-file update, use `anydock refresh <task> --input inbox/<file>`. Add `=inputs/<name>` when the destination name differs. Repeat `--input` for multiple replacements. AnyDock verifies and carries the other inputs from the current run.

To correct a named task while it waits for review, add `--pending` to an `--input` or `--inbox` refresh. If the reviewer saw an earlier batch, add `--since <earlier-batch-token>` to retain that comparison. Once the corrected work is ready, use `anydock review <task> --task --retained` to see the changes, or omit `--retained` to see the current batch. Use the printed approval command for the exact revision.

## Review the selected fields of an input

An external JSON input uses its whole file by default. A stage can instead declare `input_fields`, a map from a declared input path to the JSON pointers it needs. AnyDock keeps the full source and gives the stage the exact selected values. Use `anydock input <run> <stage> <path>` for the complete selection.

New runs canonicalize whitespace and object member order in selected values. Numeric literals stay exact. Use `"input_value_format": "exact"` when the selected value's original formatting matters. Archived runs and their updates retain the default recorded at registration. Older records without a default keep their exact hashing basis. A missing field, invalid source, or duplicate key in a selected object blocks the stage.

## Alternatives and delivery

- `anydock fork <run> <new-run>` copies the brief, stages, declared inputs, and guidance into an isolated run. Use `anydock compare <left> <right>` to compare current recorded handoffs.
- Stage contracts can require nonempty output, CSV headers and row widths, top-level JSON keys, or run-local Markdown links. The CLI names failed checks before recording a stage.
- `anydock delivery <run> --name <name> --include <output>` previews a packet. Repeat `--include` for each file. Add `--guidance` or `--decisions` to disclose those fields and `--confirm <token>` to write the exact selection locally. Inspect the selected files before sharing them.

## Daily brief

From the workspace root, run `anydock daily --feed "NAME=FOLDER"`. Repeat
`--feed` for each folder. The first run saves `.anydock/daily.json` and
writes `briefs/daily.md`. Each folder uses its top-level files and a 24-hour
freshness interval. You can also ask your agent to set up the feeds from
their paths.

To change the saved feeds, edit `.anydock/daily.json`. Each entry in the
`feeds` array has a short `name`, a local `folder`, and
`stale_after_hours`. Add `files` to select relative paths with forward
slashes. If the producer writes an error marker, name it in `failure_file`.
Its presence reports a failure until the producer removes it. For a JSON
source, `fields` can map its filename to a list of JSON pointers. The brief
shows those values and compares them with an earlier snapshot when available.
It links the full snapshots for values that exceed the preview. Observed
source changes stay in the brief for 24 hours, including past midnight.

Run `anydock daily` from the workspace root and open `briefs/daily.md` in
Obsidian or any Markdown editor. It links to full input snapshots, work in
progress, outputs from run records updated in the last 24 hours, and current
review files and revisions. For a revised draft, the brief shows bounded changes
from its last approved version when the verified archive is available. A fresh checkout stays idle. The brief does not
start work or authorize review.

When new files are ready for a finished recurring task, the brief shows a
submission command for the exact observed packet. Run it when you authorize
work on those files. If the producer has updated them again, regenerate the
brief to see the current packet and its command.

To schedule it on Windows, use the AnyDock executable as the Task Scheduler
action, set the argument to `daily`, and set **Start in** to the workspace
root. Keep personal notes in a separate Markdown file. If you edit the
generated brief, move it to keep your edits before regenerating.
