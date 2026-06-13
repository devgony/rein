---
description: Run the current LLM task document, implement unchecked tasks, update status via rein commands, and append execution notes.
disable-model-invocation: true
---

Run `rein current --path` to find the active task document, then read it.

Rules:

1. Execute only unchecked tasks.
2. Never edit checkboxes or Agent Log in the Markdown directly. Use:
   - `rein check <item-id>` after a task is implemented and verified
   - `rein log "<text>"` to append a concise entry after each completed task
   - `rein fail <item-id> --reason "<text>"` when blocked
3. Preserve `<!-- task:... -->` ID comments when editing other sections.
4. Run relevant tests before checking validation items.
5. If a PR is attached, run `rein push` when finished.
