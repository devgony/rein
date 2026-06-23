use anyhow::{bail, Context, Result};

pub const AGENT_LOG_HEADING: &str = "## Agent Log";

/// Machine sentinel marking a checklist item as resolved-failed (as opposed to
/// resolved-done). It lives in an HTML comment so it stays invisible in
/// rendered Markdown and in the projected GitHub issue.
pub const FAILED_SENTINEL: &str = "<!-- failed -->";
/// Visible failure mark appended to a failed item's text (renders on GitHub).
pub const FAIL_MARK: &str = "❌";

/// Parsed task document frontmatter. Unknown lines are preserved in `extra`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub github_issue: Option<u64>,
    pub github_pr: Option<u64>,
    pub branch: Option<String>,
    pub tags: Vec<String>,
    pub shared: bool,
    pub extra: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskDoc {
    pub front: Frontmatter,
    pub body: String,
}

impl TaskDoc {
    pub fn parse(content: &str) -> Result<TaskDoc> {
        let rest = content
            .strip_prefix("---\n")
            .context("task document must start with '---' frontmatter")?;
        let (fm_text, body) = rest
            .split_once("\n---\n")
            .or_else(|| rest.split_once("\n---"))
            .context("unterminated frontmatter")?;
        let mut front = Frontmatter::default();
        for line in fm_text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((key, val)) = line.split_once(':') else {
                front.extra.push(line.to_string());
                continue;
            };
            let key = key.trim();
            let val = val.trim();
            match key {
                "id" => front.id = val.to_string(),
                "title" => front.title = val.to_string(),
                "status" => front.status = val.to_string(),
                "created_at" => front.created_at = val.to_string(),
                "updated_at" => front.updated_at = val.to_string(),
                "github_issue" => front.github_issue = val.parse().ok(),
                "github_pr" => front.github_pr = val.parse().ok(),
                "branch" => {
                    front.branch = if val.is_empty() { None } else { Some(val.to_string()) }
                }
                "tags" => {
                    let inner = val.trim_start_matches('[').trim_end_matches(']');
                    front.tags = inner
                        .split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                }
                "shared" => front.shared = val == "true",
                _ => front.extra.push(line.to_string()),
            }
        }
        let body = body.trim_start_matches('\n').to_string();
        Ok(TaskDoc { front, body })
    }

    pub fn serialize(&self) -> String {
        let f = &self.front;
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("id: {}\n", f.id));
        out.push_str(&format!("title: {}\n", f.title));
        out.push_str(&format!("status: {}\n", f.status));
        out.push_str(&format!("created_at: {}\n", f.created_at));
        out.push_str(&format!("updated_at: {}\n", f.updated_at));
        match f.github_issue {
            Some(n) => out.push_str(&format!("github_issue: {}\n", n)),
            None => out.push_str("github_issue:\n"),
        }
        match f.github_pr {
            Some(n) => out.push_str(&format!("github_pr: {}\n", n)),
            None => out.push_str("github_pr:\n"),
        }
        match &f.branch {
            Some(b) => out.push_str(&format!("branch: {}\n", b)),
            None => out.push_str("branch:\n"),
        }
        out.push_str(&format!("tags: [{}]\n", f.tags.join(", ")));
        out.push_str(&format!("shared: {}\n", f.shared));
        for line in &f.extra {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("---\n\n");
        out.push_str(self.body.trim_end_matches('\n'));
        out.push('\n');
        out
    }

    pub fn template(id: &str, title: &str, status: &str, now: &str, shared: bool) -> TaskDoc {
        let front = Frontmatter {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
            created_at: now.to_string(),
            updated_at: now.to_string(),
            shared,
            ..Default::default()
        };
        let body = format!(
            "## Goal\n\n{}\n\n## Tasks\n\n## Validation\n\n## Notes\n\n{}\n\n<!-- append-only -->\n",
            title, AGENT_LOG_HEADING
        );
        TaskDoc { front, body }
    }

    pub fn touch(&mut self) {
        self.front.updated_at = crate::util::now_iso();
    }
}

// ---------------------------------------------------------------------------
// Checklist items
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Item {
    pub id: Option<String>,
    pub checked: bool,
    /// Resolved as failed (box is checked, text struck through, sentinel set).
    pub failed: bool,
    pub text: String,
    pub line: usize,
}

fn checkbox_prefix(line: &str) -> Option<(bool, usize)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    for (pat, checked) in [("- [ ]", false), ("- [x]", true), ("- [X]", true)] {
        if trimmed.starts_with(pat) {
            return Some((checked, indent + pat.len()));
        }
    }
    None
}

fn extract_item_id(rest: &str) -> Option<String> {
    let start = rest.find("<!-- task:")?;
    let after = &rest[start + "<!-- task:".len()..];
    let end = after.find("-->")?;
    Some(after[..end].trim().to_string())
}

pub fn scan_items(body: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if let Some((checked, after)) = checkbox_prefix(line) {
            let rest = &line[after..];
            let id = extract_item_id(rest);
            let raw_text = match rest.find("-->") {
                Some(p) if id.is_some() => rest[p + 3..].trim().to_string(),
                _ => rest.trim().to_string(),
            };
            let (failed, text) = decode_failed(&raw_text);
            items.push(Item {
                id,
                checked,
                failed,
                text,
                line: i,
            });
        }
    }
    items
}

/// The `## ` section heading each checklist item falls under, indexed parallel
/// to `scan_items` (same order, one entry per item). Empty for items before the
/// first heading. Lets `rein todo` group items the way the document does.
pub fn item_sections(body: &str) -> Vec<String> {
    let mut current = String::new();
    let mut out = Vec::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            current = rest.trim().to_string();
        }
        if checkbox_prefix(line).is_some() {
            out.push(current.clone());
        }
    }
    out
}

/// Assign stable, monotonic integer IDs to checklist items lacking one. A
/// single sequence spans Tasks and Validation. Existing IDs (any form) are kept
/// and never renumbered, so reordering, rewording, or inserting lines never
/// moves an item's ID — it is a serial number, not a line number. New items get
/// `max(existing integer ids) + 1`. Returns the new body and whether it changed.
pub fn ensure_item_ids(body: &str) -> (String, bool) {
    let items = scan_items(body);
    let mut max_n: u32 = items
        .iter()
        .filter_map(|i| i.id.as_deref())
        .filter_map(|s| s.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let mut assignments: Vec<(usize, String)> = Vec::new();
    for item in items.iter().filter(|i| i.id.is_none()) {
        max_n += 1;
        assignments.push((item.line, max_n.to_string()));
    }
    if assignments.is_empty() {
        return (body.to_string(), false);
    }
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    for (line_no, id) in assignments {
        let line = &lines[line_no];
        if let Some((_, after)) = checkbox_prefix(line) {
            let head = &line[..after];
            let rest = line[after..].trim_start();
            lines[line_no] = if rest.is_empty() {
                format!("{} <!-- task:{} -->", head, id)
            } else {
                format!("{} <!-- task:{} --> {}", head, id, rest)
            };
        }
    }
    (lines.join("\n") + "\n", true)
}

/// Toggle the checkbox of the item with the given ID.
pub fn set_checked(body: &str, item_id: &str, checked: bool) -> Result<String> {
    let marker = format!("<!-- task:{} -->", item_id);
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    for line in lines.iter_mut() {
        if line.contains(&marker) && checkbox_prefix(line).is_some() {
            let from = if checked { "- [ ]" } else { "- [x]" };
            let from_alt = if checked { "- [ ]" } else { "- [X]" };
            let to = if checked { "- [x]" } else { "- [ ]" };
            if line.contains(from) {
                *line = line.replacen(from, to, 1);
            } else if line.contains(from_alt) {
                *line = line.replacen(from_alt, to, 1);
            }
            // already in desired state is fine (idempotent)
            found = true;
            break;
        }
    }
    if !found {
        bail!("item '{}' not found", item_id);
    }
    Ok(lines.join("\n") + "\n")
}

/// Append a new unchecked checklist item to the `## Tasks` section (created at
/// the end of the document if absent), landing at the bottom of the existing
/// list. The item carries no id here; the next `ensure_item_ids` heal pass
/// assigns one, exactly as for an item typed by hand in `$EDITOR`.
pub fn append_item(body: &str, text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        bail!("item text is empty");
    }
    let line = format!("- [ ] {}", text);
    let Some((_, end)) = section_range(body, "## Tasks") else {
        // no Tasks section yet: start one at the end of the document
        let mut out = body.trim_end().to_string();
        out.push_str(&format!("\n\n## Tasks\n\n{}\n", line));
        return Ok(out);
    };
    // insert after the last non-blank line of the section so the item lands at
    // the bottom of the list, not past the blank that separates it from the
    // next `## ` heading.
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    let mut at = end;
    while at > 0 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    lines.insert(at, line);
    Ok(lines.join("\n") + "\n")
}

/// Replace the text of a checklist item, preserving its checkbox state and id
/// marker. A failed item keeps its `~~strikethrough~~ ❌` decorations around the
/// new text so the failure stays visible. Empty replacement text is rejected.
pub fn edit_item(body: &str, item_id: &str, new_text: &str) -> Result<String> {
    let new_text = new_text.trim();
    if new_text.is_empty() {
        bail!("item text is empty");
    }
    let marker = format!("<!-- task:{} -->", item_id);
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    for line in lines.iter_mut() {
        let Some((checked, after)) = checkbox_prefix(line) else {
            continue;
        };
        if !line.contains(&marker) {
            continue;
        }
        let indent_len = line.len() - line.trim_start().len();
        let indent = line[..indent_len].to_string();
        let rest = &line[after..];
        let raw_text = match rest.find("-->") {
            Some(p) => rest[p + 3..].trim().to_string(),
            None => rest.trim().to_string(),
        };
        let (was_failed, _) = decode_failed(&raw_text);
        *line = if was_failed {
            // a failed item is always a checked box; keep it struck + marked
            format!(
                "{}- [x] {} {} ~~{}~~ {}",
                indent, marker, FAILED_SENTINEL, new_text, FAIL_MARK
            )
        } else {
            let box_str = if checked { "- [x]" } else { "- [ ]" };
            format!("{}{} {} {}", indent, box_str, marker, new_text)
        };
        found = true;
        break;
    }
    if !found {
        bail!("item '{}' not found", item_id);
    }
    Ok(lines.join("\n") + "\n")
}

/// Remove a checklist item's line by id. Errors if no item carries that id.
pub fn delete_item(body: &str, item_id: &str) -> Result<String> {
    let marker = format!("<!-- task:{} -->", item_id);
    let lines: Vec<&str> = body.lines().collect();
    let Some(pos) = lines
        .iter()
        .position(|l| l.contains(&marker) && checkbox_prefix(l).is_some())
    else {
        bail!("item '{}' not found", item_id);
    };
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != pos)
        .map(|(_, l)| *l)
        .collect();
    Ok(kept.join("\n") + "\n")
}

/// Split a failed item's decorations off its text. A failed item carries the
/// `FAILED_SENTINEL` comment, its text wrapped in `~~strikethrough~~`, and a
/// trailing `FAIL_MARK`. Returns `(was_failed, clean_text)`; decorations are
/// only stripped when the sentinel is present, so plain text is never mangled.
fn decode_failed(text: &str) -> (bool, String) {
    let Some(rest) = text.strip_prefix(FAILED_SENTINEL) else {
        return (false, text.to_string());
    };
    let mut t = rest.trim();
    if let Some(s) = t.strip_suffix(FAIL_MARK) {
        t = s.trim_end();
    }
    if let Some(inner) = t.strip_prefix("~~").and_then(|x| x.strip_suffix("~~")) {
        t = inner;
    }
    (true, t.trim().to_string())
}

/// Mark an item resolved-failed: a checked box (it is terminal — no longer open
/// work), the failed sentinel, and the text as `~~strikethrough~~ ❌` so the
/// failure is visible wherever the Tasks block renders (including the projected
/// GitHub issue). Idempotent.
pub fn set_failed(body: &str, item_id: &str) -> Result<String> {
    rewrite_item_line(body, item_id, true)
}

/// Reopen a failed item: restore `- [ ]` and strip the failed decorations.
/// Errors if the item is not currently failed, so it can never silently uncheck
/// a genuinely-done item.
pub fn clear_failed(body: &str, item_id: &str) -> Result<String> {
    rewrite_item_line(body, item_id, false)
}

fn rewrite_item_line(body: &str, item_id: &str, failed: bool) -> Result<String> {
    let marker = format!("<!-- task:{} -->", item_id);
    let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    for line in lines.iter_mut() {
        let Some((_, after)) = checkbox_prefix(line) else {
            continue;
        };
        if !line.contains(&marker) {
            continue;
        }
        let indent_len = line.len() - line.trim_start().len();
        let indent = line[..indent_len].to_string();
        let rest = &line[after..];
        let raw_text = match rest.find("-->") {
            Some(p) => rest[p + 3..].trim().to_string(),
            None => rest.trim().to_string(),
        };
        let (was_failed, clean) = decode_failed(&raw_text);
        if !failed && !was_failed {
            bail!("item '{}' is not failed (nothing to reopen)", item_id);
        }
        *line = match (failed, clean.is_empty()) {
            (true, true) => format!("{}- [x] {} {} {}", indent, marker, FAILED_SENTINEL, FAIL_MARK),
            (true, false) => format!(
                "{}- [x] {} {} ~~{}~~ {}",
                indent, marker, FAILED_SENTINEL, clean, FAIL_MARK
            ),
            (false, true) => format!("{}- [ ] {}", indent, marker),
            (false, false) => format!("{}- [ ] {} {}", indent, marker, clean),
        };
        found = true;
        break;
    }
    if !found {
        bail!("item '{}' not found", item_id);
    }
    Ok(lines.join("\n") + "\n")
}

// ---------------------------------------------------------------------------
// Sections & Agent Log
// ---------------------------------------------------------------------------

/// Line range [start, end) of a `## ` section including its heading.
fn section_range(body: &str, heading: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.iter().position(|l| l.trim() == heading)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map(|p| start + 1 + p)
        .unwrap_or(lines.len());
    Some((start, end))
}

pub fn append_log(body: &str, entry: &str) -> String {
    let line = format!("- {}", entry);
    if let Some((_, end)) = section_range(body, AGENT_LOG_HEADING) {
        let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        lines.insert(end, line);
        lines.join("\n") + "\n"
    } else {
        let mut out = body.trim_end().to_string();
        out.push_str(&format!(
            "\n\n{}\n\n<!-- append-only -->\n{}\n",
            AGENT_LOG_HEADING, line
        ));
        out
    }
}

/// Body without the Agent Log section (the shareable part).
pub fn body_without_log(body: &str) -> String {
    match section_range(body, AGENT_LOG_HEADING) {
        Some((start, end)) => {
            let lines: Vec<&str> = body.lines().collect();
            let kept: Vec<&str> = lines[..start]
                .iter()
                .chain(lines[end..].iter())
                .copied()
                .collect();
            kept.join("\n").trim().to_string()
        }
        None => body.trim().to_string(),
    }
}

/// The Agent Log section content (lines after heading), if present.
pub fn log_section(body: &str) -> Option<String> {
    let (start, end) = section_range(body, AGENT_LOG_HEADING)?;
    let lines: Vec<&str> = body.lines().collect();
    Some(lines[start + 1..end].join("\n").trim().to_string())
}

/// The trimmed content of a `## ` section (the lines after its heading, up to the
/// next `## `). `None` when the heading is absent. Used by `rein summary` to pull
/// the Goal text out of the document.
pub fn section_content(body: &str, heading: &str) -> Option<String> {
    let (start, end) = section_range(body, heading)?;
    let lines: Vec<&str> = body.lines().collect();
    Some(lines[start + 1..end].join("\n").trim().to_string())
}

// ---------------------------------------------------------------------------
// Managed sections (issue / PR projections)
// ---------------------------------------------------------------------------

pub fn wrap_managed(id: &str, inner: &str) -> String {
    format!(
        "<!-- rein:begin {} -->\n\n{}\n\n<!-- rein:end -->",
        id,
        inner.trim()
    )
}

/// Issue projection: full body minus frontmatter minus Agent Log, marker-wrapped.
pub fn issue_projection(doc: &TaskDoc) -> String {
    wrap_managed(&doc.front.id, &body_without_log(&doc.body))
}

/// PR projection: shareable body + Agent Log folded into <details>.
pub fn pr_projection(doc: &TaskDoc) -> String {
    let mut inner = body_without_log(&doc.body);
    if let Some(log) = log_section(&doc.body) {
        let log = log.trim();
        if !log.is_empty() && log != "<!-- append-only -->" {
            let cleaned = log.replace("<!-- append-only -->", "");
            inner.push_str(&format!(
                "\n\n<details>\n<summary>Agent Log</summary>\n\n{}\n\n</details>",
                cleaned.trim()
            ));
        }
    }
    wrap_managed(&doc.front.id, &inner)
}

/// Find a managed block in a remote body. Returns (task_id, full block, inner).
pub fn extract_managed(text: &str) -> Option<(String, String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.trim().starts_with("<!-- rein:begin"))?;
    let begin_line = lines[begin].trim();
    let id = begin_line
        .strip_prefix("<!-- rein:begin")?
        .trim()
        .strip_suffix("-->")?
        .trim()
        .to_string();
    let end = lines[begin..]
        .iter()
        .position(|l| l.trim() == "<!-- rein:end -->")
        .map(|p| begin + p)?;
    let block = lines[begin..=end].join("\n");
    let inner = lines[begin + 1..end].join("\n").trim().to_string();
    Some((id, block, inner))
}

/// Replace (or append) the managed block in a remote body, preserving
/// everything humans wrote outside the markers.
pub fn replace_managed(remote_body: &str, new_block: &str) -> String {
    let lines: Vec<&str> = remote_body.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.trim().starts_with("<!-- rein:begin"));
    if let Some(b) = begin {
        if let Some(e) = lines[b..]
            .iter()
            .position(|l| l.trim() == "<!-- rein:end -->")
            .map(|p| b + p)
        {
            let mut out: Vec<&str> = Vec::new();
            out.extend(&lines[..b]);
            out.extend(new_block.lines());
            out.extend(&lines[e + 1..]);
            return out.join("\n").trim().to_string() + "\n";
        }
    }
    if remote_body.trim().is_empty() {
        return new_block.trim().to_string() + "\n";
    }
    format!("{}\n\n{}\n", remote_body.trim_end(), new_block.trim())
}

/// Rebuild a local body from a pulled managed inner, keeping the local Agent Log.
pub fn body_from_remote(inner: &str, local_body: &str) -> String {
    let mut out = inner.trim().to_string();
    if let Some((start, end)) = section_range(local_body, AGENT_LOG_HEADING) {
        let lines: Vec<&str> = local_body.lines().collect();
        out.push_str("\n\n");
        out.push_str(lines[start..end].join("\n").trim_end());
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TaskDoc {
        let mut doc = TaskDoc::template("task-20260613-demo", "Demo", "inbox", "2026-06-13T10:00:00+09:00", false);
        doc.body = doc.body.replace(
            "## Tasks\n",
            "## Tasks\n\n- [ ] <!-- task:one --> First thing\n- [ ] Second thing\n",
        );
        doc
    }

    #[test]
    fn roundtrip() {
        let doc = sample();
        let text = doc.serialize();
        let parsed = TaskDoc::parse(&text).unwrap();
        assert_eq!(parsed.front.id, "task-20260613-demo");
        assert_eq!(parsed.serialize(), text);
    }

    #[test]
    fn ids_assigned() {
        let doc = sample();
        let (body, changed) = ensure_item_ids(&doc.body);
        assert!(changed);
        // "one" is non-numeric so the next item gets the first integer
        assert!(body.contains("- [ ] <!-- task:1 --> Second thing"));
    }

    #[test]
    fn sections_align_with_items() {
        let mut doc = sample(); // adds two items under ## Tasks
        doc.body = doc.body.replace(
            "## Validation\n",
            "## Validation\n\n- [ ] <!-- task:v --> Tests pass\n",
        );
        let items = scan_items(&doc.body);
        let sections = item_sections(&doc.body);
        assert_eq!(items.len(), sections.len());
        assert_eq!(sections[0], "Tasks");
        assert_eq!(sections[1], "Tasks");
        assert_eq!(sections[2], "Validation");
    }

    #[test]
    fn check_toggle() {
        let doc = sample();
        let body = set_checked(&doc.body, "one", true).unwrap();
        assert!(body.contains("- [x] <!-- task:one -->"));
        let body = set_checked(&body, "one", false).unwrap();
        assert!(body.contains("- [ ] <!-- task:one -->"));
        assert!(set_checked(&body, "nope", true).is_err());
    }

    #[test]
    fn append_item_adds_to_tasks_section() {
        let doc = sample(); // two items under ## Tasks, then Validation/Notes/Agent Log
        let body = append_item(&doc.body, "Third thing").unwrap();
        let items = scan_items(&body);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].text, "Third thing");
        assert!(!items[2].checked && !items[2].failed);
        // it landed inside ## Tasks (its section), not past the section break
        let sections = item_sections(&body);
        assert_eq!(sections[2], "Tasks");
        // empty / whitespace text is rejected
        assert!(append_item(&body, "   ").is_err());
        // a following id heal gives the new item a stable integer id
        let (healed, changed) = ensure_item_ids(&body);
        assert!(changed);
        let last = scan_items(&healed).into_iter().last().unwrap();
        assert!(last.id.is_some());
        assert_eq!(last.text, "Third thing");
    }

    #[test]
    fn append_item_creates_tasks_section_when_missing() {
        let body = "## Goal\n\nNo tasks heading here\n";
        let out = append_item(body, "First item").unwrap();
        assert!(out.contains("## Tasks"));
        let items = scan_items(&out);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "First item");
    }

    #[test]
    fn edit_item_replaces_text_keeping_state_and_id() {
        let doc = sample(); // item "one" open: "First thing"
        // edit an open item: text changes, box + id marker preserved
        let body = edit_item(&doc.body, "one", "Reworded thing").unwrap();
        assert!(body.contains("- [ ] <!-- task:one --> Reworded thing"));
        let it = scan_items(&body)
            .into_iter()
            .find(|i| i.id.as_deref() == Some("one"))
            .unwrap();
        assert!(!it.checked && !it.failed);
        assert_eq!(it.text, "Reworded thing");
        // editing a checked item keeps it checked
        let checked = set_checked(&doc.body, "one", true).unwrap();
        let body = edit_item(&checked, "one", "Still done").unwrap();
        assert!(body.contains("- [x] <!-- task:one --> Still done"));
        // editing a failed item keeps the strikethrough + ❌
        let failed = set_failed(&doc.body, "one").unwrap();
        let body = edit_item(&failed, "one", "Nope but reworded").unwrap();
        assert!(body.contains("~~Nope but reworded~~"));
        let it = scan_items(&body)
            .into_iter()
            .find(|i| i.id.as_deref() == Some("one"))
            .unwrap();
        assert!(it.failed);
        assert_eq!(it.text, "Nope but reworded");
        // empty text and unknown id are errors
        assert!(edit_item(&doc.body, "one", "   ").is_err());
        assert!(edit_item(&doc.body, "nope", "x").is_err());
    }

    #[test]
    fn delete_item_removes_the_line() {
        let mut doc = sample(); // item "one" + an unnumbered "Second thing"
        let (with_ids, _) = ensure_item_ids(&doc.body);
        doc.body = with_ids; // "Second thing" → task:1
        assert_eq!(scan_items(&doc.body).len(), 2);
        let body = delete_item(&doc.body, "one").unwrap();
        let items = scan_items(&body);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Second thing");
        // deleting a missing id is an error
        assert!(delete_item(&body, "one").is_err());
    }

    #[test]
    fn fail_roundtrip() {
        let doc = sample(); // item "one": "First thing"
        let body = set_failed(&doc.body, "one").unwrap();
        assert!(body.contains("- [x] <!-- task:one --> <!-- failed --> ~~First thing~~ ❌"));
        // scan reads it as both checked and failed, with the decorations stripped
        let it = scan_items(&body)
            .into_iter()
            .find(|i| i.id.as_deref() == Some("one"))
            .unwrap();
        assert!(it.checked && it.failed);
        assert_eq!(it.text, "First thing");
        // marking again is idempotent
        assert_eq!(set_failed(&body, "one").unwrap(), body);
        // clearing restores the original open item
        let reopened = clear_failed(&body, "one").unwrap();
        assert!(reopened.contains("- [ ] <!-- task:one --> First thing"));
        let it = scan_items(&reopened)
            .into_iter()
            .find(|i| i.id.as_deref() == Some("one"))
            .unwrap();
        assert!(!it.checked && !it.failed);
        // reopening a non-failed item is an error; unknown id is too
        assert!(clear_failed(&reopened, "one").is_err());
        assert!(set_failed(&doc.body, "nope").is_err());
    }

    #[test]
    fn projection_excludes_log() {
        let mut doc = sample();
        doc.body = append_log(&doc.body, "2026-06-13 did something");
        let proj = issue_projection(&doc);
        assert!(proj.contains("rein:begin task-20260613-demo"));
        assert!(proj.contains("First thing"));
        assert!(!proj.contains("did something"));
        let (id, block, inner) = extract_managed(&proj).unwrap();
        assert_eq!(id, "task-20260613-demo");
        assert_eq!(block.trim(), proj.trim());
        assert!(inner.contains("## Goal"));
    }

    #[test]
    fn managed_replace_preserves_outside() {
        let remote = "human intro\n\n<!-- rein:begin t1 -->\n\nold\n\n<!-- rein:end -->\n\nhuman outro";
        let updated = replace_managed(remote, &wrap_managed("t1", "new content"));
        assert!(updated.contains("human intro"));
        assert!(updated.contains("human outro"));
        assert!(updated.contains("new content"));
        assert!(!updated.contains("old"));
    }

    #[test]
    fn pull_preserves_log() {
        let mut doc = sample();
        doc.body = append_log(&doc.body, "kept entry");
        let new_body = body_from_remote("## Goal\n\nUpdated goal", &doc.body);
        assert!(new_body.contains("Updated goal"));
        assert!(new_body.contains("kept entry"));
    }
}
