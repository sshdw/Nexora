use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};

use super::io::{
    ignore_rules, open_absolute, open_relative, path_label, read_at, read_relative, relative_path,
};
use super::{digest, secret_free, ContextError};
use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::settings::SettingsRepository;

const MAX_SKILLS: usize = 64;
const MAX_ENTRIES: usize = 1024;
const MAX_DEPTH: usize = 5;
const MAX_SKILL_BYTES: u64 = 16 * 1024;
static PREFERENCE_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkillRef {
    source_id: String,
    revision: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum SkillScope {
    Personal,
    Project,
}

#[derive(Debug, Serialize)]
pub(crate) struct SkillSummary {
    reference: SkillRef,
    name: String,
    description: String,
    scope: SkillScope,
    path: String,
    commit: Option<String>,
    sha256: String,
    trusted: bool,
    enabled: bool,
    auto_select: bool,
}

#[derive(Serialize)]
pub(crate) struct SkillView {
    summary: SkillSummary,
    text: String,
}

impl SkillView {
    pub(crate) fn trust_review(&self, russian: bool) -> String {
        let skill = &self.summary;
        let source = match (skill.scope, russian) {
            (SkillScope::Personal, false) => "Personal",
            (SkillScope::Project, false) => "Project",
            (SkillScope::Personal, true) => "Личные",
            (SkillScope::Project, true) => "Проект",
        };
        let body = if russian {
            "Разрешить использовать эту версию навыка в запросах?"
        } else {
            "Allow this exact version of the skill to be included in prompts?"
        };
        format!(
            "{body}\n\n/{}\n{} · {}\nSHA-256 {}",
            skill.name, source, skill.path, skill.sha256
        )
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SkillAction {
    Trust,
    Disable,
    Enable,
    EnableAutoSelection,
    DisableAutoSelection,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Preference {
    trusted_revision: Option<String>,
    disabled: bool,
    auto_revision: Option<String>,
}

struct Candidate {
    reference: SkillRef,
    name: String,
    description: String,
    scope: SkillScope,
    path: String,
    commit: Option<String>,
    sha256: String,
    text: String,
}

impl Candidate {
    fn is_trusted(&self, pref: &Preference) -> bool {
        (matches!(self.scope, SkillScope::Personal) && self.commit.is_none())
            || pref.trusted_revision.as_ref() == Some(&self.reference.revision)
    }

    fn summary(&self, pref: &Preference) -> SkillSummary {
        SkillSummary {
            reference: self.reference.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            scope: self.scope,
            path: self.path.clone(),
            commit: self.commit.clone(),
            sha256: self.sha256.clone(),
            trusted: self.is_trusted(pref),
            enabled: !pref.disabled,
            auto_select: self.is_automatic(pref),
        }
    }

    fn is_automatic(&self, pref: &Preference) -> bool {
        pref.auto_revision.as_ref() == Some(&self.reference.revision)
            && self.is_trusted(pref)
            && !pref.disabled
    }

    fn settings_key(&self) -> String {
        format!(
            "{}{}",
            super::SKILL_SETTING_PREFIX,
            self.reference.source_id
        )
    }

    fn preference(&self, db: &Database) -> Result<Preference, ContextError> {
        let stored = SettingsService::new(db).read(&self.settings_key())?;
        stored.map_or_else(
            || Ok(Preference::default()),
            |raw| serde_json::from_str(&raw).map_err(|_| ContextError::Settings),
        )
    }
}

pub(super) fn list(
    db: &Database,
    app_data: &Path,
    workspace: Option<&Path>,
) -> Result<Vec<SkillSummary>, ContextError> {
    catalog(app_data, workspace)
        .into_iter()
        .map(|skill| Ok(skill.summary(&skill.preference(db)?)))
        .collect()
}

pub(super) fn view(
    db: &Database,
    app_data: &Path,
    workspace: Option<&Path>,
    reference: &SkillRef,
) -> Result<SkillView, ContextError> {
    let skill = find(catalog(app_data, workspace), reference)?;
    Ok(SkillView {
        summary: skill.summary(&skill.preference(db)?),
        text: skill.text,
    })
}

pub(super) fn set_preference(
    db: &Database,
    app_data: &Path,
    workspace: Option<&Path>,
    reference: &SkillRef,
    action: SkillAction,
) -> Result<SkillSummary, ContextError> {
    let _serial = PREFERENCE_WRITE
        .lock()
        .map_err(|_| ContextError::Settings)?;
    let skill = find(catalog(app_data, workspace), reference)?;
    let mut pref = skill.preference(db)?;
    match action {
        SkillAction::Trust => pref.trusted_revision = Some(reference.revision.clone()),
        SkillAction::Disable => pref.disabled = true,
        SkillAction::Enable => pref.disabled = false,
        SkillAction::EnableAutoSelection => {
            if pref.disabled || !skill.is_trusted(&pref) {
                return Err(ContextError::SkillUnavailable);
            }
            pref.auto_revision = Some(reference.revision.clone());
        }
        SkillAction::DisableAutoSelection => pref.auto_revision = None,
    }
    let data = serde_json::to_string(&pref).map_err(|_| ContextError::Settings)?;
    let settings = SettingsRepository::new(db);
    let key = skill.settings_key();
    if settings.exists(&key)? {
        settings.update(&key, Some(&data))?;
    } else {
        settings.create(&key, Some(&data))?;
    }
    Ok(skill.summary(&pref))
}

pub(super) fn automatic(
    db: &Database,
    app_data: &Path,
    workspace: Option<&Path>,
    draft: &str,
) -> Result<Vec<SkillSummary>, ContextError> {
    if draft.starts_with('/') {
        return Ok(Vec::new());
    }
    let words = draft.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    let mut selected = Vec::new();
    for skill in catalog(app_data, workspace) {
        if !words
            .clone()
            .any(|word| word.eq_ignore_ascii_case(&skill.name))
        {
            continue;
        }
        let summary = skill.summary(&skill.preference(db)?);
        if summary.auto_select {
            selected.push(summary);
        }
    }
    // Names are untrusted metadata. Ambiguous matches must be selected explicitly.
    let mut counts = std::collections::HashMap::new();
    for skill in &selected {
        *counts.entry(skill.name.clone()).or_insert(0) += 1;
    }
    selected.retain(|item| counts.get(&item.name) == Some(&1));
    selected.truncate(3);
    Ok(selected)
}

pub(super) fn admit(
    db: &Database,
    app_data: &Path,
    workspace: Option<&Path>,
    explicit: &[SkillRef],
    automatic: &[SkillRef],
) -> Result<Vec<(String, String)>, ContextError> {
    if explicit.is_empty() && automatic.is_empty() {
        return Ok(Vec::new());
    }
    let skills = catalog(app_data, workspace);
    let _serial = PREFERENCE_WRITE
        .lock()
        .map_err(|_| ContextError::Settings)?;
    let mut output = Vec::new();
    let mut sources = std::collections::HashSet::new();
    let references = explicit
        .iter()
        .map(|reference| (reference, false))
        .chain(automatic.iter().map(|reference| (reference, true)));
    for (reference, is_automatic) in references {
        if !sources.insert(&reference.source_id) {
            return Err(ContextError::Invalid);
        }
        let skill = skills
            .iter()
            .find(|skill| skill.reference == *reference)
            .ok_or(ContextError::Stale)?;
        let pref = skill.preference(db)?;
        if pref.disabled || !skill.is_trusted(&pref) {
            return Err(ContextError::SkillUnavailable);
        }
        if is_automatic && !skill.is_automatic(&pref) {
            return Err(ContextError::AutoSelectionChanged);
        }
        output.push((skill.name.clone(), skill.text.clone()));
    }
    Ok(output)
}

fn find(skills: Vec<Candidate>, reference: &SkillRef) -> Result<Candidate, ContextError> {
    skills
        .into_iter()
        .find(|skill| skill.reference == *reference)
        .ok_or(ContextError::Stale)
}

fn catalog(app_data: &Path, workspace: Option<&Path>) -> Vec<Candidate> {
    let mut skills = Vec::new();
    let mut roots = [
        (app_data, Path::new("skills"), SkillScope::Personal),
        (app_data, Path::new(".nexora/skills"), SkillScope::Project),
    ];
    if let Some(workspace) = workspace {
        roots[1].0 = workspace;
    }
    for (base, relative, scope) in roots
        .into_iter()
        .filter(|(_, _, scope)| workspace.is_some() || matches!(scope, SkillScope::Personal))
    {
        let Ok(base_dir) = open_absolute(base) else {
            continue;
        };
        let Ok(skills_dir) = open_relative(&base_dir, relative) else {
            continue;
        };
        let Ok(inherited) = (if matches!(scope, SkillScope::Project) {
            git_pin(&base_dir)
        } else {
            Ok(None)
        }) else {
            continue;
        };
        let rules = if matches!(scope, SkillScope::Project) {
            match ignore_rules(&base_dir) {
                Ok(rules) => Some(rules),
                Err(_) => continue,
            }
        } else {
            None
        };
        let mut scan = SkillScan {
            scope,
            root: base.join(relative),
            skills: Vec::new(),
            entries_left: MAX_ENTRIES,

            rules,
        };
        scan.walk(&skills_dir, Path::new(""), inherited.as_deref(), 0);
        skills.extend(scan.skills);
    }
    skills.sort_by(|left, right| (&left.name, &left.path).cmp(&(&right.name, &right.path)));
    skills
}

struct SkillScan {
    scope: SkillScope,
    root: PathBuf,

    rules: Option<crate::application::project_dir::IgnoreRules>,
    skills: Vec<Candidate>,
    entries_left: usize,
}

impl SkillScan {
    fn walk(&mut self, dir: &Dir, relative: &Path, inherited: Option<&str>, depth: usize) {
        let Ok(here) = git_pin(dir) else { return };
        let commit = here.as_deref().or(inherited);

        let skill_path = relative.join("SKILL.md");
        if self.skills.len() < MAX_SKILLS && self.allowed(&skill_path, true) {
            if let (Some(path), Ok(text)) = (
                path_label(&skill_path),
                read_at(dir, OsStr::new("SKILL.md"), MAX_SKILL_BYTES),
            ) {
                if let Some(skill) = self.candidate(&path, &text, commit) {
                    self.skills.push(skill);
                }
            }
        }
        if depth == MAX_DEPTH || self.entries_left == 0 || self.skills.len() >= MAX_SKILLS {
            return;
        }
        let Ok(entries) = dir.entries() else { return };
        let mut names = Vec::new();
        for entry in entries.take(self.entries_left) {
            self.entries_left -= 1;
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| !name.starts_with('.'))
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                names.push(name);
            }
        }
        names.sort_unstable();
        for name in names {
            let child_path = relative.join(&name);
            if !self.allowed(&child_path, false) || self.skills.len() >= MAX_SKILLS {
                continue;
            }
            if let Ok(child) = dir.open_dir_nofollow(name) {
                self.walk(&child, &child_path, commit, depth + 1);
            }
        }
    }

    fn allowed(&self, relative: &Path, file: bool) -> bool {
        let path = self.root.join(relative);
        // .nexora/skills is explicitly admitted here; all other sensitive
        // segments below it retain the ordinary workspace read floor.
        if nexora_os_exec::private_store_path(&path)
            || nexora_os_exec::sensitive_write_path(relative)
        {
            return false;
        }
        self.rules.as_ref().is_none_or(|rules| {
            let project_relative = Path::new(".nexora/skills").join(relative);
            path_label(&project_relative).is_some_and(|label| {
                !crate::application::project_dir::rules_verdict(rules, &label, !file)
            })
        })
    }
    fn candidate(&self, path: &str, text: &str, commit: Option<&str>) -> Option<Candidate> {
        metadata(text)?;
        let safe_text = secret_free(text);
        let (name, description) = metadata(&safe_text)?;
        let source = self.root.join(path);
        let source_id = digest(source.to_str()?.as_bytes());
        let sha256 = digest(text.as_bytes());
        let revision =
            digest(format!("{source_id}\n{}\n{sha256}", commit.unwrap_or("")).as_bytes());
        Some(Candidate {
            reference: SkillRef {
                source_id,
                revision,
            },
            name,
            description: secret_free(&description),
            scope: self.scope,
            path: secret_free(path),
            commit: commit.map(str::to_owned),
            sha256,
            text: safe_text,
        })
    }
}

// Recognize the two scalar fields used for discovery. Unknown fields and
// instructions never control registration, path handling, or tool access.
fn metadata(document: &str) -> Option<(String, String)> {
    let mut lines = document.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    for line in lines.take(64) {
        if line.trim() == "---" {
            let name: String = name?;
            let description: String = description?;
            if !valid_skill_name(&name) || description.chars().count() > 240 {
                return None;
            }
            return Some((name, description));
        }
        let slot = match line.split_once(':') {
            Some(("name", field)) if name.is_none() => (&mut name, field),
            Some(("description", field)) if description.is_none() => (&mut description, field),
            Some(("name" | "description", _)) => return None,
            _ => continue,
        };
        let value = slot.1.trim().trim_matches(['\'', '"']);
        let valid = !value.is_empty() && !value.chars().any(char::is_control);
        if !valid {
            return None;
        }
        *slot.0 = Some(value.to_owned());
    }
    None
}

pub(super) fn valid_skill_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// Pin against a regular checked-out repository without opening its config,
// object database, or external worktree pointer. A malformed marker fails shut.
pub(super) fn git_pin(dir: &Dir) -> io::Result<Option<String>> {
    let git = match dir.open_dir_nofollow(".git") {
        Ok(dir) => dir,
        Err(error) => match dir.symlink_metadata(".git") {
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => return Ok(None),
            _ => return Err(error),
        },
    };
    let head = read_at(&git, OsStr::new("HEAD"), 512)?;
    let head = head.trim();
    if commit_id(head) {
        return Ok(Some(head.to_owned()));
    }
    let reference = head
        .strip_prefix("ref: ")
        .filter(|s| s.starts_with("refs/heads/"))
        .ok_or_else(|| io::Error::other("unavailable git pin"))?;
    let relative = relative_path(reference).map_err(|_| io::Error::other("invalid git ref"))?;
    match read_relative(&git, &relative, 512) {
        Ok(value) if commit_id(value.trim()) => Ok(Some(value.trim().to_owned())),
        Ok(_) => Err(io::Error::other("invalid git ref")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match read_at(&git, OsStr::new("packed-refs"), 1024 * 1024) {
                Ok(packed) => Ok(packed
                    .lines()
                    .filter_map(|line| line.split_once(' '))
                    .find(|(hash, path)| *path == reference && commit_id(hash))
                    .map(|(hash, _)| hash.to_owned())
                    .or_else(|| Some("unborn".into()))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some("unborn".into())),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
