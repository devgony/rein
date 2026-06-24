---
description: Run the current LLM task document, implement unchecked tasks, update status via rein commands, and append execution notes.
disable-model-invocation: true
---

Run `rein todo` to list the current task's unchecked items. Each line is `<id>` then the item text, grouped under its `## section`. Read the full document with `rein current --path` only when you need the Goal or Notes for context.

Rules:

1. Execute only the unchecked items `rein todo` prints.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `rein check <item-id>` after a task is implemented and verified
   - `rein log "<text>" --task <item-id>` to record progress on a specific item — `--task` is required and the entry is tagged so it shows under that item in `rein ui`
   - `rein note "<text>"` to append an Agent Log entry not tied to any specific item
   - `rein fail <item-id> --reason "<text>"` when blocked — resolves the item (it drops out of `rein todo`, so a re-run won't re-attempt it); `rein retry <item-id>` reopens it
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `rein push` when finished.
