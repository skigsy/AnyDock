# Review a budget note

From an empty workspace containing your source file, use the installed CLI.

```powershell
anydock run budget-note --brief "Summarize the two approved Q4 budget lines." --work "Report hosting, hardware, and the total in USD." --source "Q4 Budget (Final).md" --output summary.md --review "Are these budget figures correct?"
```

`runs/budget-note/brief.md`

```text
Summarize the two approved Q4 budget lines.
```

`runs/budget-note/stages/01-work/stage.md`

~~~markdown
# Work

```anydock
{"inputs":["inputs/Q4 Budget (Final).md"],"outputs":["stages/01-work/outputs/summary.md"],"review":"Are these budget figures correct?"}
```

## Work

Report hosting, hardware, and the total in USD.
~~~

After writing the summary and completing attempt 1, run `anydock review budget-note`. A fresh session can open `anydock handoff budget-note`. Excerpts from the same run follow.

```text
Review budget-note | 01-work
Decision: Are these budget figures correct?
Review full file: runs/budget-note/stages/01-work/outputs/summary.md
Hosting $180, hardware $260, total $440.
Revision: 5e3de310de14a8279e30b91c6daf31ebdc52d4ca04eef466cb254d24e598da54

AnyDock handoff | budget-note | managed
Current 01-work | awaiting review | runs/budget-note/stages/01-work/stage.md
Human review: Are these budget figures correct?
Revision: 5e3de310de14a8279e30b91c6daf31ebdc52d4ca04eef466cb254d24e598da54
```

Use `review budget-note --interactive` to decide in your terminal. Each run generates its own revision.
