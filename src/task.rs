use anyhow::{bail, Context, Result};

pub const AGENT_LOG_HEADING: &str = "## Agent Log";

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
            let text = match rest.find("-->") {
                Some(p) if id.is_some() => rest[p + 3..].trim().to_string(),
                _ => rest.trim().to_string(),
            };
            items.push(Item {
                id,
                checked,
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
