use crate::domain::{
    validate_id, validate_numbering_start, validate_record_markdown, NumberingStyle, Record,
    RecordState,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::path::{Component, Path};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

const MANAGED_HEADING: &str = "## WakeGPT 速记";
const MANAGED_START: &str = "<!-- wakegpt:managed -->";
const MANAGED_END: &str = "<!-- /wakegpt:managed -->";
const TARGET_PREFIX: &str = "<!-- wakegpt:target id=\"";
const TARGET_SCHEMA: &str = "2";
pub const MANAGED_SCHEMA_VERSION: u32 = 2;
const RECORD_PREFIX: &str = "<!-- wakegpt:record id=\"";
const RECORD_END: &str = "<!-- /wakegpt:record -->";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMarkdownError(String);

impl ManagedMarkdownError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ManagedMarkdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ManagedMarkdownError {}

#[cfg(test)]
pub fn synchronize_notebook(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
) -> Result<String, ManagedMarkdownError> {
    synchronize_notebook_for_path(
        existing,
        target_id,
        records,
        numbering_style,
        crate::domain::DEFAULT_NUMBERING_START,
        "\n",
        None,
    )
}

#[cfg(test)]
pub fn synchronize_notebook_with_line_ending(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    line_ending: &str,
) -> Result<String, ManagedMarkdownError> {
    synchronize_notebook_for_path(
        existing,
        target_id,
        records,
        numbering_style,
        crate::domain::DEFAULT_NUMBERING_START,
        line_ending,
        None,
    )
}

#[cfg(test)]
pub fn synchronize_notebook_with_start(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
) -> Result<String, ManagedMarkdownError> {
    synchronize_notebook_for_path(
        existing,
        target_id,
        records,
        numbering_style,
        numbering_start,
        "\n",
        None,
    )
}

pub fn synchronize_notebook_for_path(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
    line_ending: &str,
    notebook_relative_path: Option<&str>,
) -> Result<String, ManagedMarkdownError> {
    if !matches!(line_ending, "\n" | "\r\n" | "\r") {
        return Err(ManagedMarkdownError::new(
            "unsupported Markdown line ending",
        ));
    }
    validate_id(target_id, "target id")
        .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    let section = render_managed_section_for_path(
        target_id,
        records,
        numbering_style,
        numbering_start,
        notebook_relative_path,
    )?;
    let section = if line_ending == "\n" {
        section
    } else {
        section.replace('\n', line_ending)
    };
    let Some(range) = locate_managed_section(existing, target_id)? else {
        return Ok(append_new_section(existing, &section, line_ending));
    };

    let mut output = String::with_capacity(existing.len() + section.len());
    output.push_str(&existing[..range.target_start]);
    output.push_str(&section);
    output.push_str(&existing[range.replacement_end..]);
    Ok(output)
}

pub fn overwrite_managed_region_for_path(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
    line_ending: &str,
    notebook_relative_path: &str,
) -> Result<String, ManagedMarkdownError> {
    if !matches!(line_ending, "\n" | "\r\n" | "\r") {
        return Err(ManagedMarkdownError::new(
            "unsupported Markdown line ending",
        ));
    }
    let section = render_managed_section_for_path(
        target_id,
        records,
        numbering_style,
        numbering_start,
        Some(notebook_relative_path),
    )?;
    let section = if line_ending == "\n" {
        section
    } else {
        section.replace('\n', line_ending)
    };
    let Some(range) = locate_managed_section_unchecked(existing, target_id)? else {
        return Ok(append_new_section(existing, &section, line_ending));
    };
    let mut output = String::with_capacity(existing.len() + section.len());
    output.push_str(&existing[..range.target_start]);
    output.push_str(&section);
    output.push_str(&existing[range.replacement_end..]);
    Ok(output)
}

#[cfg(test)]
pub fn managed_section_digest(
    existing: &str,
    target_id: &str,
) -> Result<Option<String>, ManagedMarkdownError> {
    Ok(managed_snapshot(existing, target_id)?.map(|snapshot| snapshot.managed_sha256))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRecordSnapshot {
    pub id: String,
    pub revision: u64,
    pub sequence: usize,
    pub visible_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSnapshot {
    pub managed_sha256: String,
    pub records: Vec<ManagedRecordSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptableRecordBody {
    pub id: String,
    pub expected_revision: u64,
    pub body_markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptableFileVersion {
    pub records: Vec<AdoptableRecordBody>,
}

pub fn managed_snapshot(
    existing: &str,
    target_id: &str,
) -> Result<Option<ManagedSnapshot>, ManagedMarkdownError> {
    let Some(range) = locate_managed_section(existing, target_id)? else {
        return Ok(None);
    };
    let records = validate_record_blocks(
        &existing[range.managed_start + MANAGED_START.len()..range.managed_end],
    )?;
    Ok(Some(ManagedSnapshot {
        managed_sha256: sha256_hex(&existing.as_bytes()[range.target_start..range.replacement_end]),
        records,
    }))
}

pub fn managed_region_sha256_unchecked(
    existing: &str,
    target_id: &str,
) -> Result<Option<String>, ManagedMarkdownError> {
    Ok(locate_managed_section_unchecked(existing, target_id)?
        .map(|range| sha256_hex(&existing.as_bytes()[range.target_start..range.replacement_end])))
}

pub fn parse_adoptable_file_version(
    existing: &str,
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
    notebook_relative_path: &str,
) -> Result<AdoptableFileVersion, ManagedMarkdownError> {
    let range = locate_managed_section_unchecked(existing, target_id)?
        .ok_or_else(|| ManagedMarkdownError::new("WakeGPT managed region is missing"))?;
    let managed_body_start = range.managed_start + MANAGED_START.len();
    let blocks = parse_record_blocks(
        &existing[managed_body_start..range.managed_end],
        DigestPolicy::AllowStale,
    )?;
    let mut active_records: Vec<&Record> = records
        .iter()
        .filter(|record| record.state == RecordState::Active)
        .collect();
    active_records.sort_by(|left, right| {
        (left.logical_order, left.created_at_ms, &left.id).cmp(&(
            right.logical_order,
            right.created_at_ms,
            &right.id,
        ))
    });
    if blocks.len() != active_records.len() {
        return Err(ManagedMarkdownError::new(
            "the file version changed the WakeGPT record set",
        ));
    }

    let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let numbering_start = u64::from(
        validate_numbering_start(numbering_start)
            .map_err(|error| ManagedMarkdownError::new(error.to_string()))?,
    );
    let mut current_date = String::new();
    let mut date_sequence = numbering_start;
    let mut adopted = Vec::with_capacity(blocks.len());
    let mut canonical_records = Vec::with_capacity(blocks.len());

    for (index, (block, record)) in blocks.iter().zip(active_records).enumerate() {
        if block.marker.id != record.id
            || block.marker.sequence != index + 1
            || block.marker.revision != record.applied_revision
            || record.applied_revision == 0
        {
            return Err(ManagedMarkdownError::new(
                "the file version changed WakeGPT record identity or order",
            ));
        }
        let (local_date, local_time) = local_date_time(record.created_at_ms, local_offset)?;
        if numbering_style == NumberingStyle::DateHeadingNumeric {
            if current_date != local_date {
                current_date = local_date;
                date_sequence = numbering_start;
            } else {
                date_sequence = date_sequence
                    .checked_add(1)
                    .ok_or_else(|| ManagedMarkdownError::new("visible numbering overflow"))?;
            }
        }
        let visible_sequence = if numbering_style == NumberingStyle::DateHeadingNumeric {
            date_sequence
        } else {
            numbering_start
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| ManagedMarkdownError::new("record count is too large"))?,
                )
                .ok_or_else(|| ManagedMarkdownError::new("visible numbering overflow"))?
        };
        let visible = normalize_line_endings(block.visible);
        let unprefixed = remove_visible_prefix(
            &visible,
            numbering_style,
            visible_sequence,
            local_time.as_str(),
        )?;
        let body_markdown = remove_attachment_suffix(record, notebook_relative_path, &unprefixed)?;
        let body_markdown = validate_record_markdown(&body_markdown, record.attachments.len())
            .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
        let mut canonical = (*record).clone();
        canonical.body_markdown.clone_from(&body_markdown);
        canonical.revision = record.applied_revision;
        canonical_records.push(canonical);
        adopted.push(AdoptableRecordBody {
            id: record.id.clone(),
            expected_revision: record.revision,
            body_markdown,
        });
    }

    let canonical = render_managed_section_for_path(
        target_id,
        &canonical_records,
        numbering_style,
        u32::try_from(numbering_start)
            .map_err(|_| ManagedMarkdownError::new("visible numbering overflow"))?,
        Some(notebook_relative_path),
    )?;
    let normalized_observed = normalize_observed_record_digests(existing, range, &blocks)?;
    if normalize_line_endings(&normalized_observed) != canonical {
        return Err(ManagedMarkdownError::new(
            "the file version changed WakeGPT structure or attachments",
        ));
    }
    Ok(AdoptableFileVersion { records: adopted })
}

pub fn detach_notebook_controls(
    existing: &str,
    target_id: &str,
) -> Result<String, ManagedMarkdownError> {
    let range = locate_managed_section(existing, target_id)?
        .ok_or_else(|| ManagedMarkdownError::new("WakeGPT managed region is missing"))?;
    let managed = &existing[range.target_start..range.replacement_end];
    let mut visible = String::with_capacity(managed.len());
    let mut offset = 0usize;
    while offset < managed.len() {
        let boundary = next_line_boundary(managed, offset);
        let line = &managed[offset..boundary.next];
        let content = &managed[offset..boundary.content_end];
        let control = content == MANAGED_START
            || content == MANAGED_END
            || content == RECORD_END
            || content.starts_with(TARGET_PREFIX)
            || (content.starts_with(RECORD_PREFIX) && content.ends_with(" -->"));
        if !control {
            visible.push_str(line);
        }
        offset = boundary.next;
    }

    let mut output = String::with_capacity(existing.len());
    output.push_str(&existing[..range.target_start]);
    output.push_str(&visible);
    output.push_str(&existing[range.replacement_end..]);
    Ok(output)
}

#[derive(Debug, Clone, Copy)]
struct ManagedSectionRange {
    target_start: usize,
    managed_start: usize,
    managed_end: usize,
    replacement_end: usize,
}

fn locate_managed_section(
    existing: &str,
    target_id: &str,
) -> Result<Option<ManagedSectionRange>, ManagedMarkdownError> {
    let Some(range) = locate_managed_section_unchecked(existing, target_id)? else {
        return Ok(None);
    };
    validate_record_blocks(
        &existing[range.managed_start + MANAGED_START.len()..range.managed_end],
    )?;
    Ok(Some(range))
}

fn locate_managed_section_unchecked(
    existing: &str,
    target_id: &str,
) -> Result<Option<ManagedSectionRange>, ManagedMarkdownError> {
    validate_id(target_id, "target id")
        .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    let start_positions = exact_line_positions(existing, MANAGED_START);
    let end_positions = exact_line_positions(existing, MANAGED_END);
    let target_positions = prefixed_line_positions(existing, TARGET_PREFIX);

    if start_positions.is_empty() && end_positions.is_empty() && target_positions.is_empty() {
        return Ok(None);
    }
    if start_positions.len() != 1 || end_positions.len() != 1 || target_positions.len() != 1 {
        return Err(ManagedMarkdownError::new(
            "notebook contains ambiguous WakeGPT control markers",
        ));
    }

    let managed_start = start_positions[0];
    let managed_end = end_positions[0];
    if managed_end <= managed_start {
        return Err(ManagedMarkdownError::new(
            "WakeGPT managed region markers are out of order",
        ));
    }

    let target_start = target_positions[0];
    let target_line_boundary = next_line_boundary(existing, target_start);
    let target_line_end = target_line_boundary.content_end;
    let target_line = &existing[target_start..target_line_end];
    let (existing_target_id, schema) = parse_target_marker(target_line)?;
    if schema != TARGET_SCHEMA {
        return Err(ManagedMarkdownError::new(format!(
            "unsupported WakeGPT notebook schema: {schema}"
        )));
    }
    if existing_target_id != target_id {
        return Err(ManagedMarkdownError::new(
            "notebook target identity does not match the selected notebook",
        ));
    }
    if target_start >= managed_start
        || !existing[target_line_boundary.next..managed_start]
            .trim()
            .is_empty()
    {
        return Err(ManagedMarkdownError::new(
            "WakeGPT target marker is not attached to its managed region",
        ));
    }

    let replacement_end = managed_end + MANAGED_END.len();
    Ok(Some(ManagedSectionRange {
        target_start,
        managed_start,
        managed_end,
        replacement_end,
    }))
}

#[cfg(test)]
pub fn render_managed_section(
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
) -> Result<String, ManagedMarkdownError> {
    render_managed_section_for_path(
        target_id,
        records,
        numbering_style,
        crate::domain::DEFAULT_NUMBERING_START,
        None,
    )
}

#[cfg(test)]
pub fn render_managed_section_with_start(
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
) -> Result<String, ManagedMarkdownError> {
    render_managed_section_for_path(target_id, records, numbering_style, numbering_start, None)
}

fn render_managed_section_for_path(
    target_id: &str,
    records: &[Record],
    numbering_style: NumberingStyle,
    numbering_start: u32,
    notebook_relative_path: Option<&str>,
) -> Result<String, ManagedMarkdownError> {
    validate_id(target_id, "target id")
        .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    let numbering_start = u64::from(
        validate_numbering_start(numbering_start)
            .map_err(|error| ManagedMarkdownError::new(error.to_string()))?,
    );
    let mut active_records: Vec<&Record> = records
        .iter()
        .filter(|record| record.state == RecordState::Active)
        .collect();
    active_records.sort_by(|left, right| {
        (left.logical_order, left.created_at_ms, &left.id).cmp(&(
            right.logical_order,
            right.created_at_ms,
            &right.id,
        ))
    });

    let mut output = format!(
        "<!-- wakegpt:target id=\"{target_id}\" schema=\"{TARGET_SCHEMA}\" -->\n{MANAGED_START}"
    );
    let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let mut current_date = String::new();
    let mut date_sequence = numbering_start;

    for (index, record) in active_records.into_iter().enumerate() {
        validate_id(&record.id, "record id")
            .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
        validate_record_markdown(&record.body_markdown, record.attachments.len())
            .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
        let (local_date, local_time) = local_date_time(record.created_at_ms, local_offset)?;

        if numbering_style == NumberingStyle::DateHeadingNumeric {
            if current_date != local_date {
                current_date.clone_from(&local_date);
                date_sequence = numbering_start;
                output.push_str(&format!("\n\n### {local_date}"));
            } else {
                date_sequence = date_sequence
                    .checked_add(1)
                    .ok_or_else(|| ManagedMarkdownError::new("visible numbering overflow"))?;
            }
        }

        let visible_sequence = if numbering_style == NumberingStyle::DateHeadingNumeric {
            date_sequence
        } else {
            numbering_start
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| ManagedMarkdownError::new("record count is too large"))?,
                )
                .ok_or_else(|| ManagedMarkdownError::new("visible numbering overflow"))?
        };
        let body = record_visible_markdown(record, notebook_relative_path)?;
        let visible = render_visible_body(
            &body,
            numbering_style,
            visible_sequence,
            local_time.as_str(),
        );
        let digest = sha256_hex(visible.as_bytes());
        output.push_str(&format!(
            "\n\n<!-- wakegpt:record id=\"{}\" revision=\"{}\" sequence=\"{}\" digest=\"sha256:{}\" -->\n{}\n{}",
            record.id,
            record.revision,
            index + 1,
            digest,
            visible,
            RECORD_END
        ));
    }
    output.push_str(&format!("\n{MANAGED_END}"));
    Ok(output)
}

fn append_new_section(existing: &str, section: &str, line_ending: &str) -> String {
    if existing.is_empty() {
        format!("{MANAGED_HEADING}{line_ending}{line_ending}{section}{line_ending}")
    } else if existing.ends_with(&format!("{line_ending}{line_ending}")) {
        format!("{existing}{MANAGED_HEADING}{line_ending}{line_ending}{section}{line_ending}")
    } else if existing.ends_with(line_ending) {
        format!("{existing}{line_ending}{MANAGED_HEADING}{line_ending}{line_ending}{section}{line_ending}")
    } else {
        format!("{existing}{line_ending}{line_ending}{MANAGED_HEADING}{line_ending}{line_ending}{section}{line_ending}")
    }
}

fn record_visible_markdown(
    record: &Record,
    notebook_relative_path: Option<&str>,
) -> Result<String, ManagedMarkdownError> {
    let mut parts = Vec::new();
    if !record.body_markdown.trim().is_empty() {
        parts.push(record.body_markdown.trim_end().to_owned());
    }
    for attachment in &record.attachments {
        let managed_path = if let Some(notebook_path) = notebook_relative_path {
            relative_attachment_link(notebook_path, &attachment.managed_relative_path)?
        } else {
            attachment.managed_relative_path.clone()
        };
        let path = if managed_path.contains(' ') {
            format!("<{managed_path}>")
        } else {
            managed_path
        };
        parts.push(format!("![图片]({path})"));
    }
    Ok(parts.join("\n\n"))
}

fn relative_attachment_link(
    notebook_relative_path: &str,
    attachment_relative_path: &str,
) -> Result<String, ManagedMarkdownError> {
    let notebook = Path::new(notebook_relative_path);
    let attachment = Path::new(attachment_relative_path);
    if notebook.is_absolute() || attachment.is_absolute() {
        return Err(ManagedMarkdownError::new(
            "attachment path must be workspace-relative",
        ));
    }
    let notebook_parent = notebook.parent().unwrap_or_else(|| Path::new(""));
    let base = normal_components(notebook_parent)?;
    let target = normal_components(attachment)?;
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec!["..".to_owned(); base.len().saturating_sub(common)];
    parts.extend(target.into_iter().skip(common));
    if parts.is_empty() {
        return Err(ManagedMarkdownError::new(
            "attachment path cannot equal notebook directory",
        ));
    }
    Ok(parts.join("/"))
}

fn normal_components(path: &Path) -> Result<Vec<String>, ManagedMarkdownError> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| ManagedMarkdownError::new("attachment path is not valid UTF-8")),
            _ => Err(ManagedMarkdownError::new(
                "attachment path contains unsafe components",
            )),
        })
        .collect()
}

fn render_visible_body(
    body: &str,
    style: NumberingStyle,
    sequence: u64,
    local_time: &str,
) -> String {
    match style {
        NumberingStyle::None => body.to_owned(),
        NumberingStyle::Numeric | NumberingStyle::DateHeadingNumeric => {
            with_prefix(body, &format!("{sequence}. "))
        }
        NumberingStyle::Bullet => with_prefix(body, "- "),
        NumberingStyle::Task => with_prefix(body, "- [ ] "),
        NumberingStyle::TimePrefix => with_prefix(body, &format!("[{local_time}] ")),
    }
}

fn with_prefix(body: &str, prefix: &str) -> String {
    let mut lines = body.lines();
    let first = lines.next().unwrap_or_default();
    let indentation = " ".repeat(prefix.chars().count());
    let mut output = format!("{prefix}{first}");
    for line in lines {
        output.push('\n');
        if !line.is_empty() {
            output.push_str(&indentation);
            output.push_str(line);
        }
    }
    output
}

fn parse_target_marker(line: &str) -> Result<(&str, &str), ManagedMarkdownError> {
    let body = line
        .strip_prefix(TARGET_PREFIX)
        .and_then(|value| value.strip_suffix("\" -->"))
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT target marker"))?;
    body.split_once("\" schema=\"")
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT target marker"))
}

#[derive(Debug, Clone, Copy)]
struct ParsedRecordMarker<'a> {
    id: &'a str,
    revision: u64,
    sequence: usize,
    digest: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestPolicy {
    Verify,
    AllowStale,
}

#[derive(Debug)]
struct ParsedRecordBlock<'a> {
    marker: ParsedRecordMarker<'a>,
    marker_start: usize,
    marker_end: usize,
    visible: &'a str,
    visible_sha256: String,
}

fn parse_record_marker(line: &str) -> Result<ParsedRecordMarker<'_>, ManagedMarkdownError> {
    let body = line
        .strip_prefix(RECORD_PREFIX)
        .and_then(|value| value.strip_suffix(" -->"))
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT record marker"))?;
    let (id, remainder) = body
        .split_once("\" revision=\"")
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT record marker"))?;
    let (revision, remainder) = remainder
        .split_once("\" sequence=\"")
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT record marker"))?;
    let (sequence, digest) = remainder
        .split_once("\" digest=\"sha256:")
        .ok_or_else(|| ManagedMarkdownError::new("WakeGPT schema 2 requires record digests"))?;
    let digest = digest
        .strip_suffix('"')
        .ok_or_else(|| ManagedMarkdownError::new("invalid WakeGPT record digest"))?;

    validate_id(id, "record id").map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    let revision = revision
        .parse::<u64>()
        .map_err(|_| ManagedMarkdownError::new("invalid WakeGPT record revision"))?;
    if revision == 0 {
        return Err(ManagedMarkdownError::new(
            "WakeGPT record revision must be positive",
        ));
    }
    let sequence = sequence
        .parse::<usize>()
        .map_err(|_| ManagedMarkdownError::new("invalid WakeGPT record sequence"))?;
    if sequence == 0 {
        return Err(ManagedMarkdownError::new(
            "WakeGPT record sequence must be positive",
        ));
    }
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManagedMarkdownError::new("invalid WakeGPT record digest"));
    }
    Ok(ParsedRecordMarker {
        id,
        revision,
        sequence,
        digest,
    })
}

fn parse_record_blocks(
    body: &str,
    digest_policy: DigestPolicy,
) -> Result<Vec<ParsedRecordBlock<'_>>, ManagedMarkdownError> {
    let starts = prefixed_line_positions(body, RECORD_PREFIX);
    let ends = exact_line_positions(body, RECORD_END);
    if starts.len() != ends.len() {
        return Err(ManagedMarkdownError::new(
            "WakeGPT record markers are unbalanced",
        ));
    }

    let mut previous_end = 0usize;
    let mut ids = HashSet::with_capacity(starts.len());
    let mut blocks = Vec::with_capacity(starts.len());
    for (index, (&start, &end)) in starts.iter().zip(&ends).enumerate() {
        if start < previous_end || end <= start {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record markers are out of order",
            ));
        }
        let line_boundary = next_line_boundary(body, start);
        if end <= line_boundary.content_end {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record markers are out of order",
            ));
        }
        let marker_end = line_boundary.content_end;
        let marker = parse_record_marker(&body[start..marker_end])?;
        if marker.sequence != index + 1 {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record sequence is not contiguous",
            ));
        }
        if !ids.insert(marker.id) {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record id appears more than once",
            ));
        }
        let visible_start = line_boundary.next;
        let visible_end = previous_line_ending_start(body, end);
        if visible_end < visible_start {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record block is malformed",
            ));
        }
        let visible = &body[visible_start..visible_end];
        let actual_digest = sha256_hex(normalize_line_endings(visible).as_bytes());
        if digest_policy == DigestPolicy::Verify
            && !marker.digest.eq_ignore_ascii_case(&actual_digest)
        {
            return Err(ManagedMarkdownError::new(format!(
                "WakeGPT record {} was modified outside WakeGPT at revision {}",
                marker.id, marker.revision
            )));
        }
        blocks.push(ParsedRecordBlock {
            marker,
            marker_start: start,
            marker_end,
            visible,
            visible_sha256: actual_digest,
        });
        previous_end = end + RECORD_END.len();
    }
    Ok(blocks)
}

fn validate_record_blocks(body: &str) -> Result<Vec<ManagedRecordSnapshot>, ManagedMarkdownError> {
    parse_record_blocks(body, DigestPolicy::Verify).map(|blocks| {
        blocks
            .into_iter()
            .map(|block| ManagedRecordSnapshot {
                id: block.marker.id.to_owned(),
                revision: block.marker.revision,
                sequence: block.marker.sequence,
                visible_sha256: block.visible_sha256,
            })
            .collect()
    })
}

fn normalize_observed_record_digests(
    existing: &str,
    range: ManagedSectionRange,
    blocks: &[ParsedRecordBlock<'_>],
) -> Result<String, ManagedMarkdownError> {
    let body_start = range.managed_start + MANAGED_START.len();
    let mut normalized = existing[range.target_start..range.replacement_end].to_owned();
    for block in blocks.iter().rev() {
        let marker_start = body_start
            .checked_add(block.marker_start)
            .and_then(|value| value.checked_sub(range.target_start))
            .ok_or_else(|| ManagedMarkdownError::new("WakeGPT record marker is out of range"))?;
        let marker_end = body_start
            .checked_add(block.marker_end)
            .and_then(|value| value.checked_sub(range.target_start))
            .ok_or_else(|| ManagedMarkdownError::new("WakeGPT record marker is out of range"))?;
        if marker_start > marker_end || marker_end > normalized.len() {
            return Err(ManagedMarkdownError::new(
                "WakeGPT record marker is out of range",
            ));
        }
        let marker = format!(
            "{RECORD_PREFIX}{}\" revision=\"{}\" sequence=\"{}\" digest=\"sha256:{}\" -->",
            block.marker.id, block.marker.revision, block.marker.sequence, block.visible_sha256
        );
        normalized.replace_range(marker_start..marker_end, &marker);
    }
    Ok(normalized)
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn remove_visible_prefix(
    visible: &str,
    style: NumberingStyle,
    sequence: u64,
    local_time: &str,
) -> Result<String, ManagedMarkdownError> {
    let prefix = match style {
        NumberingStyle::None => return Ok(visible.to_owned()),
        NumberingStyle::Numeric | NumberingStyle::DateHeadingNumeric => {
            format!("{sequence}. ")
        }
        NumberingStyle::Bullet => "- ".to_owned(),
        NumberingStyle::Task => "- [ ] ".to_owned(),
        NumberingStyle::TimePrefix => format!("[{local_time}] "),
    };
    let mut lines = visible.split('\n');
    let first = lines
        .next()
        .unwrap_or_default()
        .strip_prefix(&prefix)
        .ok_or_else(|| ManagedMarkdownError::new("the file version changed visible numbering"))?;
    let indentation = " ".repeat(prefix.chars().count());
    let mut output = first.to_owned();
    for line in lines {
        output.push('\n');
        if !line.is_empty() {
            output.push_str(line.strip_prefix(&indentation).ok_or_else(|| {
                ManagedMarkdownError::new("the file version changed visible indentation")
            })?);
        }
    }
    Ok(output)
}

fn remove_attachment_suffix(
    record: &Record,
    notebook_relative_path: &str,
    visible_body: &str,
) -> Result<String, ManagedMarkdownError> {
    if record.attachments.is_empty() {
        return Ok(visible_body.to_owned());
    }
    let mut attachment_only = record.clone();
    attachment_only.body_markdown.clear();
    let suffix = record_visible_markdown(&attachment_only, Some(notebook_relative_path))?;
    if visible_body == suffix {
        return Ok(String::new());
    }
    visible_body
        .strip_suffix(&format!("\n\n{suffix}"))
        .map(str::to_owned)
        .ok_or_else(|| {
            ManagedMarkdownError::new("the file version changed WakeGPT attachment references")
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn positions(haystack: &str, needle: &str) -> Vec<usize> {
    haystack
        .match_indices(needle)
        .map(|(index, _)| index)
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct LineBoundary {
    content_end: usize,
    next: usize,
}

fn next_line_boundary(value: &str, start: usize) -> LineBoundary {
    let bytes = value.as_bytes();
    let mut offset = start;
    while offset < bytes.len() && !matches!(bytes[offset], b'\r' | b'\n') {
        offset += 1;
    }
    let next = if bytes.get(offset) == Some(&b'\r') && bytes.get(offset + 1) == Some(&b'\n') {
        offset + 2
    } else if offset < bytes.len() {
        offset + 1
    } else {
        offset
    };
    LineBoundary {
        content_end: offset,
        next,
    }
}

fn previous_line_ending_start(value: &str, end: usize) -> usize {
    if end > 1 && &value.as_bytes()[end - 2..end] == b"\r\n" {
        end - 2
    } else if end > 0 && matches!(value.as_bytes()[end - 1], b'\r' | b'\n') {
        end - 1
    } else {
        end
    }
}

fn prefixed_line_positions(haystack: &str, prefix: &str) -> Vec<usize> {
    positions(haystack, prefix)
        .into_iter()
        .filter(|position| {
            *position == 0 || matches!(haystack.as_bytes()[position - 1], b'\r' | b'\n')
        })
        .collect()
}

fn exact_line_positions(haystack: &str, line: &str) -> Vec<usize> {
    prefixed_line_positions(haystack, line)
        .into_iter()
        .filter(|position| {
            let boundary = next_line_boundary(haystack, *position);
            &haystack[*position..boundary.content_end] == line
        })
        .collect()
}

fn local_date_time(
    timestamp_ms: i64,
    offset: UtcOffset,
) -> Result<(String, String), ManagedMarkdownError> {
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp_ms) * 1_000_000)
        .map_err(|_| ManagedMarkdownError::new("record timestamp is outside the supported range"))?
        .to_offset(offset);
    let date = timestamp
        .format(&format_description!("[year]-[month]-[day]"))
        .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    let time = timestamp
        .format(&format_description!("[hour]:[minute]"))
        .map_err(|error| ManagedMarkdownError::new(error.to_string()))?;
    Ok((date, time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Attachment, RecordState, SyncState};

    const TARGET_ID: &str = "018f4f88-7642-7d1a-bf31-02c7f7cf47bb";
    const FIRST_ID: &str = "018f4f88-7642-7d1a-bf31-02c7f7cf47bc";
    const SECOND_ID: &str = "018f4f88-7642-7d1a-bf31-02c7f7cf47bd";

    fn record(id: &str, body: &str, order: i64, created_at_ms: i64) -> Record {
        Record {
            id: id.to_owned(),
            workspace_id: "018f4f88-7642-7d1a-bf31-02c7f7cf47be".to_owned(),
            notebook_id: Some("018f4f88-7642-7d1a-bf31-02c7f7cf47bf".to_owned()),
            body_markdown: body.to_owned(),
            created_at_ms,
            updated_at_ms: created_at_ms,
            logical_order: order,
            state: RecordState::Active,
            sync_state: SyncState::Queued,
            revision: 1,
            applied_revision: 0,
            trashed_at_ms: None,
            is_pinned: false,
            attachments: Vec::new(),
        }
    }

    fn synced_record(id: &str, body: &str, order: i64, created_at_ms: i64) -> Record {
        let mut record = record(id, body, order, created_at_ms);
        record.sync_state = SyncState::Synced;
        record.applied_revision = record.revision;
        record
    }

    #[test]
    fn appends_managed_region_without_touching_existing_markdown() {
        let existing = "# Product\n\nUser-owned paragraph.\n";
        let result = synchronize_notebook(
            existing,
            TARGET_ID,
            &[record(FIRST_ID, "First note", 1024, 0)],
            NumberingStyle::Numeric,
        )
        .unwrap();

        assert!(result.starts_with(existing.trim_end()));
        assert!(result.contains("User-owned paragraph."));
        assert!(result.contains("1. First note"));
    }

    #[test]
    fn rewrites_only_the_matching_managed_region() {
        let original = synchronize_notebook(
            "# Product\n\nBefore",
            TARGET_ID,
            &[record(FIRST_ID, "Old", 1024, 0)],
            NumberingStyle::Numeric,
        )
        .unwrap();
        let with_footer = format!("{original}\nAfter\n");
        let updated = synchronize_notebook(
            &with_footer,
            TARGET_ID,
            &[record(FIRST_ID, "New", 1024, 0)],
            NumberingStyle::Numeric,
        )
        .unwrap();

        assert!(updated.starts_with("# Product\n\nBefore"));
        assert!(updated.ends_with("\nAfter\n"));
        assert!(updated.contains("1. New"));
        assert!(!updated.contains("1. Old"));
    }

    #[test]
    fn deletion_reorders_visible_numbers() {
        let initial = synchronize_notebook(
            "",
            TARGET_ID,
            &[
                record(FIRST_ID, "First", 1024, 0),
                record(SECOND_ID, "Second", 2048, 1),
            ],
            NumberingStyle::Numeric,
        )
        .unwrap();
        let updated = synchronize_notebook(
            &initial,
            TARGET_ID,
            &[record(SECOND_ID, "Second", 2048, 1)],
            NumberingStyle::Numeric,
        )
        .unwrap();

        assert!(updated.contains("1. Second"));
        assert!(!updated.contains("2. Second"));
        assert!(!updated.contains(FIRST_ID));
    }

    #[test]
    fn custom_start_reorders_visible_numbers_without_changing_logical_sequence() {
        let initial = synchronize_notebook_with_start(
            "",
            TARGET_ID,
            &[
                record(FIRST_ID, "First", 1024, 0),
                record(SECOND_ID, "Second", 2048, 1),
            ],
            NumberingStyle::Numeric,
            7,
        )
        .unwrap();
        assert!(initial.contains("7. First"));
        assert!(initial.contains("8. Second"));
        assert!(initial.contains("sequence=\"1\""));
        assert!(initial.contains("sequence=\"2\""));

        let updated = synchronize_notebook_with_start(
            &initial,
            TARGET_ID,
            &[record(SECOND_ID, "Second", 2048, 1)],
            NumberingStyle::Numeric,
            7,
        )
        .unwrap();
        assert!(updated.contains("7. Second"));
        assert!(!updated.contains("8. Second"));
    }

    #[test]
    fn multiline_list_content_is_indented_under_its_record() {
        let rendered = render_managed_section(
            TARGET_ID,
            &[record(FIRST_ID, "First line\nsecond line", 1024, 0)],
            NumberingStyle::Numeric,
        )
        .unwrap();
        assert!(rendered.contains("1. First line\n   second line"));
    }

    #[test]
    fn attachment_links_are_relative_to_the_notebook_directory() {
        let mut record = record(FIRST_ID, "With image", 1024, 0);
        record.attachments.push(Attachment {
            id: "018f4f88-7642-7d1a-bf31-02c7f7cf47c1".to_owned(),
            record_id: FIRST_ID.to_owned(),
            media_type: "image/png".to_owned(),
            managed_relative_path: ".wakegpt/attachments/ab/abcdef.png".to_owned(),
            content_sha256: "a".repeat(64),
            byte_size: 8,
            created_at_ms: 0,
            file_state: crate::domain::AttachmentFileState::Ready,
            previous_managed_relative_path: None,
            relocation_state: crate::domain::AttachmentRelocationState::Ready,
            relocation_error_code: None,
        });

        let rendered = synchronize_notebook_for_path(
            "",
            TARGET_ID,
            &[record],
            NumberingStyle::None,
            crate::domain::DEFAULT_NUMBERING_START,
            "\n",
            Some("notes/nested/product.md"),
        )
        .unwrap();

        assert!(rendered.contains("![图片](../../.wakegpt/attachments/ab/abcdef.png)"));
    }

    #[test]
    fn target_mismatch_fails_closed() {
        let existing = synchronize_notebook("", TARGET_ID, &[], NumberingStyle::None).unwrap();
        let error = synchronize_notebook(
            &existing,
            "018f4f88-7642-7d1a-bf31-02c7f7cf47c0",
            &[],
            NumberingStyle::None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("target identity"));
    }

    #[test]
    fn duplicate_managed_markers_fail_closed() {
        let malformed = format!(
            "<!-- wakegpt:target id=\"{TARGET_ID}\" schema=\"1\" -->\n{MANAGED_START}\n{MANAGED_START}\n{MANAGED_END}"
        );
        assert!(synchronize_notebook(&malformed, TARGET_ID, &[], NumberingStyle::None).is_err());
    }

    #[test]
    fn date_heading_style_resets_visible_numbering_to_the_configured_start() {
        let rendered = render_managed_section_with_start(
            TARGET_ID,
            &[
                record(FIRST_ID, "Day one", 1024, 0),
                record(SECOND_ID, "Day three", 2048, 172_800_000),
            ],
            NumberingStyle::DateHeadingNumeric,
            7,
        )
        .unwrap();
        assert_eq!(rendered.matches("\n\n### ").count(), 2);
        assert_eq!(rendered.matches("\n7. ").count(), 2);
    }

    #[test]
    fn record_digest_rejects_external_body_edits() {
        let original = synchronize_notebook(
            "",
            TARGET_ID,
            &[record(FIRST_ID, "Original", 1024, 0)],
            NumberingStyle::None,
        )
        .unwrap();
        let changed = original.replace("Original", "External edit");

        let error = synchronize_notebook(
            &changed,
            TARGET_ID,
            &[record(FIRST_ID, "WakeGPT edit", 1024, 0)],
            NumberingStyle::None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("modified outside WakeGPT"));
        assert!(managed_snapshot(&changed, TARGET_ID).is_err());
        assert_ne!(
            managed_region_sha256_unchecked(&original, TARGET_ID).unwrap(),
            managed_region_sha256_unchecked(&changed, TARGET_ID).unwrap()
        );
    }

    #[test]
    fn explicit_overwrite_replaces_only_the_conflicted_managed_region() {
        let original = synchronize_notebook(
            "# User heading\n\nOutside\n",
            TARGET_ID,
            &[record(FIRST_ID, "Original", 1024, 0)],
            NumberingStyle::Numeric,
        )
        .unwrap();
        let changed = original.replace("Original", "External");
        let overwritten = overwrite_managed_region_for_path(
            &changed,
            TARGET_ID,
            &[record(FIRST_ID, "WakeGPT", 1024, 0)],
            NumberingStyle::Numeric,
            1,
            "\n",
            "notes.md",
        )
        .unwrap();
        assert!(overwritten.starts_with("# User heading\n\nOutside\n"));
        assert!(overwritten.contains("1. WakeGPT"));
        assert!(!overwritten.contains("External"));
        assert!(managed_snapshot(&overwritten, TARGET_ID).unwrap().is_some());
    }

    #[test]
    fn adoptable_file_version_accepts_only_visible_body_edits_for_all_numbering_styles() {
        for style in [
            NumberingStyle::None,
            NumberingStyle::Numeric,
            NumberingStyle::Bullet,
            NumberingStyle::Task,
            NumberingStyle::TimePrefix,
            NumberingStyle::DateHeadingNumeric,
        ] {
            let baseline = synced_record(FIRST_ID, "Original", 1024, 0);
            let rendered = synchronize_notebook_for_path(
                "# User heading\n",
                TARGET_ID,
                std::slice::from_ref(&baseline),
                style,
                7,
                "\n",
                Some("notes.md"),
            )
            .unwrap();
            let changed = rendered.replace("Original", "External edit");
            let mut current = baseline;
            current.body_markdown = "WakeGPT edit".to_owned();
            current.revision = 2;
            let adopted =
                parse_adoptable_file_version(&changed, TARGET_ID, &[current], style, 7, "notes.md")
                    .unwrap();
            assert_eq!(adopted.records[0].body_markdown, "External edit");
            assert_eq!(adopted.records[0].expected_revision, 2);
        }
    }

    #[test]
    fn adoptable_file_version_supports_crlf_and_multiline_content() {
        let baseline = synced_record(FIRST_ID, "Original\nsecond", 1024, 0);
        let rendered = synchronize_notebook_for_path(
            "# User\r\n",
            TARGET_ID,
            std::slice::from_ref(&baseline),
            NumberingStyle::Numeric,
            1,
            "\r\n",
            Some("notes.md"),
        )
        .unwrap();
        let changed = rendered.replace("Original", "External");
        let adopted = parse_adoptable_file_version(
            &changed,
            TARGET_ID,
            &[baseline],
            NumberingStyle::Numeric,
            1,
            "notes.md",
        )
        .unwrap();
        assert_eq!(adopted.records[0].body_markdown, "External\nsecond");
    }

    #[test]
    fn adoptable_file_version_rejects_marker_and_attachment_changes() {
        let mut baseline = synced_record(FIRST_ID, "With image", 1024, 0);
        baseline.attachments.push(Attachment {
            id: "018f4f88-7642-7d1a-bf31-02c7f7cf47c1".to_owned(),
            record_id: FIRST_ID.to_owned(),
            media_type: "image/png".to_owned(),
            managed_relative_path: "attachments/image.png".to_owned(),
            content_sha256: "a".repeat(64),
            byte_size: 8,
            created_at_ms: 0,
            file_state: crate::domain::AttachmentFileState::Ready,
            previous_managed_relative_path: None,
            relocation_state: crate::domain::AttachmentRelocationState::Ready,
            relocation_error_code: None,
        });
        let rendered = synchronize_notebook_for_path(
            "",
            TARGET_ID,
            std::slice::from_ref(&baseline),
            NumberingStyle::None,
            1,
            "\n",
            Some("notes.md"),
        )
        .unwrap();
        assert!(parse_adoptable_file_version(
            &rendered.replace("sequence=\"1\"", "sequence=\"2\""),
            TARGET_ID,
            std::slice::from_ref(&baseline),
            NumberingStyle::None,
            1,
            "notes.md",
        )
        .is_err());
        assert!(parse_adoptable_file_version(
            &rendered.replace("attachments/image.png", "attachments/other.png"),
            TARGET_ID,
            &[baseline],
            NumberingStyle::None,
            1,
            "notes.md",
        )
        .is_err());
    }

    #[test]
    fn schema_two_rejects_a_record_without_a_digest() {
        let rendered = render_managed_section(
            TARGET_ID,
            &[record(FIRST_ID, "Legacy", 1024, 0)],
            NumberingStyle::None,
        )
        .unwrap();
        let legacy = rendered.replace(&format!(" digest=\"sha256:{}\"", sha256_hex(b"Legacy")), "");

        let error = synchronize_notebook(
            &legacy,
            TARGET_ID,
            &[record(FIRST_ID, "Legacy", 1024, 0)],
            NumberingStyle::None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires record digests"));
    }

    #[test]
    fn appending_preserves_user_trailing_whitespace() {
        let existing = "# User content\n\ntrailing spaces   ";
        let rendered =
            synchronize_notebook(existing, TARGET_ID, &[], NumberingStyle::None).unwrap();
        assert!(rendered.starts_with(existing));
    }

    #[test]
    fn crlf_rendering_preserves_user_bytes_and_uses_crlf_in_managed_section() {
        let existing = "# User\r\n\r\nParagraph\r\n";
        let rendered = synchronize_notebook_with_line_ending(
            existing,
            TARGET_ID,
            &[record(FIRST_ID, "First", 1024, 0)],
            NumberingStyle::Numeric,
            "\r\n",
        )
        .unwrap();
        assert!(rendered.starts_with(existing));
        assert!(rendered.as_bytes()[existing.len()..]
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .all(
                |(index, _)| index > 0 && rendered.as_bytes()[existing.len() + index - 1] == b'\r'
            ));
        assert!(rendered.contains("\r\n<!-- wakegpt:record"));
    }

    #[test]
    fn cr_rendering_parses_and_detaches_the_managed_section() {
        let existing = "# User\r\rParagraph\r";
        let baseline = synced_record(FIRST_ID, "First\nsecond", 1024, 0);
        let rendered = synchronize_notebook_with_line_ending(
            existing,
            TARGET_ID,
            std::slice::from_ref(&baseline),
            NumberingStyle::Numeric,
            "\r",
        )
        .unwrap();

        assert!(rendered.starts_with(existing));
        assert!(!rendered.contains('\n'));
        assert!(managed_snapshot(&rendered, TARGET_ID).unwrap().is_some());
        let adopted = parse_adoptable_file_version(
            &rendered.replace("First", "External"),
            TARGET_ID,
            &[baseline],
            NumberingStyle::Numeric,
            crate::domain::DEFAULT_NUMBERING_START,
            "notes.md",
        )
        .unwrap();
        assert_eq!(adopted.records[0].body_markdown, "External\nsecond");
        let detached = detach_notebook_controls(&rendered, TARGET_ID).unwrap();
        assert!(detached.contains("1. First\r   second"));
        assert!(!detached.contains("<!-- wakegpt:"));
        assert!(!detached.contains("<!-- /wakegpt:"));
        assert!(!detached.contains('\n'));
    }

    #[test]
    fn managed_section_digest_changes_only_with_the_managed_section() {
        let rendered = synchronize_notebook(
            "# User content",
            TARGET_ID,
            &[record(FIRST_ID, "First", 1024, 0)],
            NumberingStyle::None,
        )
        .unwrap();
        let digest = managed_section_digest(&rendered, TARGET_ID).unwrap();
        let outside_change = rendered.replace("# User content", "# User content changed");

        assert_eq!(
            managed_section_digest(&outside_change, TARGET_ID).unwrap(),
            digest
        );
    }

    #[test]
    fn detaching_removes_only_control_markers_and_keeps_visible_markdown() {
        let managed = synchronize_notebook(
            "# Product\n\nUser-owned paragraph.\n",
            TARGET_ID,
            &[
                record(FIRST_ID, "First", 1024, 0),
                record(SECOND_ID, "Second", 2048, 1),
            ],
            NumberingStyle::Numeric,
        )
        .unwrap();

        let detached = detach_notebook_controls(&managed, TARGET_ID).unwrap();

        assert!(detached.contains("# Product"));
        assert!(detached.contains("User-owned paragraph."));
        assert!(detached.contains("## WakeGPT 速记"));
        assert!(detached.contains("1. First"));
        assert!(detached.contains("2. Second"));
        assert!(!detached.contains("<!-- wakegpt:"));
        assert!(!detached.contains("<!-- /wakegpt:"));
    }
}
