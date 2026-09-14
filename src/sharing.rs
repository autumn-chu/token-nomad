use crate::{
    config::{Agent, Config, Endpoint, Profile, Protocol, state_dir},
    security, storage,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const BUNDLE_VERSION: u32 = 1;
const PLAN_VERSION: u32 = 1;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub agent: Agent,
    pub profiles: Vec<String>,
    pub skills: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub agent: Agent,
    pub skills_dir: PathBuf,
    pub key_env: BTreeMap<String, String>,
    pub replace: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanChange {
    pub kind: String,
    pub id: String,
    pub action: String,
}

impl std::fmt::Display for PlanChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {} {}", self.action, self.kind, self.id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub id: String,
    pub changes: Vec<PlanChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub id: String,
    pub changes: Vec<PlanChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    agent: Agent,
    files: BTreeMap<String, FileDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableConfig {
    version: u32,
    endpoints: BTreeMap<String, PortableEndpoint>,
    profiles: BTreeMap<String, PortableProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "auth", rename_all = "kebab-case", deny_unknown_fields)]
enum PortableEndpoint {
    Native,
    Api {
        protocol: Protocol,
        base_url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableProfile {
    agent: Agent,
    endpoint: String,
    label: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    order: Option<i32>,
    model: Option<String>,
    reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableSkill {
    name: String,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPlan {
    version: u32,
    id: String,
    agent: Agent,
    config_path: PathBuf,
    config_hash: String,
    source: PathBuf,
    source_hash: String,
    skills_dir: PathBuf,
    skill_before_hash: BTreeMap<String, Option<String>>,
    key_env: BTreeMap<String, String>,
    import: PortableConfig,
    skills: Vec<PortableSkill>,
    replace: BTreeSet<String>,
    changes: Vec<PlanChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharingJournal {
    version: u32,
    id: String,
    phase: JournalPhase,
    config_path: PathBuf,
    config_hash_before: String,
    config_hash_after: String,
    source_hash: String,
    changes: Vec<PlanChange>,
    import: PortableConfig,
    import_skills: Vec<PortableSkill>,
    key_env: BTreeMap<String, String>,
    profiles: BTreeMap<String, ManagedProfileBefore>,
    endpoints: BTreeMap<String, ManagedEndpointBefore>,
    skills_dir: PathBuf,
    skills: BTreeMap<String, SkillBefore>,
    skill_hash_before: BTreeMap<String, Option<String>>,
    skill_hash_after: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum JournalPhase {
    Pending,
    Committed,
    Restoring,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum ManagedProfileBefore {
    Missing,
    Present(PortableProfile),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum ManagedEndpointBefore {
    Missing,
    Present {
        endpoint: PortableEndpoint,
        key_env: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
enum SkillBefore {
    Missing,
    Present {
        skill: PortableSkill,
        modes: SkillModes,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillModes {
    root: u32,
    files: BTreeMap<String, u32>,
    directories: BTreeMap<String, u32>,
}

pub fn export(config: &Config, destination: &Path, options: &ExportOptions) -> Result<()> {
    let destination = absolute_local(destination)?;
    if storage::entry_kind(&destination)?.is_some() {
        bail!("Portable export destination must not already exist");
    }
    if options.profiles.is_empty() && options.skills.is_empty() {
        bail!("Select at least one profile or skill for portable export");
    }
    let portable = portable_from_config(config, options.agent, &options.profiles)?;
    let skills = options
        .skills
        .iter()
        .map(|path| read_skill(path))
        .collect::<Result<Vec<_>>>()?;
    let mut names = BTreeSet::new();
    for skill in &skills {
        if !names.insert(skill.name.clone()) {
            bail!("Selected skills have duplicate names");
        }
    }
    storage::create_private_directory(&destination)?;
    let result = (|| {
        let config_text = toml::to_string_pretty(&portable)?;
        security::reject_secret("profiles.toml", &config_text)?;
        write_new_private(&destination.join("profiles.toml"), config_text.as_bytes())?;
        let mut files = BTreeMap::new();
        files.insert("profiles.toml".into(), digest(config_text.as_bytes()));
        for skill in &skills {
            for (relative, text) in &skill.files {
                let item = Path::new("skills").join(&skill.name).join(relative);
                let name = security::display_relative(&item)?;
                write_new_private(&destination.join(&item), text.as_bytes())?;
                files.insert(name, digest(text.as_bytes()));
            }
        }
        let manifest = Manifest {
            version: BUNDLE_VERSION,
            agent: options.agent,
            files,
        };
        let manifest_text = serde_json::to_vec_pretty(&manifest)?;
        write_new_private(&destination.join("manifest.json"), &manifest_text)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = storage::remove_tree(&destination);
    }
    result
}

pub fn plan_import(
    _config: &Config,
    config_path: &Path,
    source: &Path,
    options: &ImportOptions,
) -> Result<Preview> {
    let config_path = absolute_local(config_path)?;
    let source = absolute_local(source)?;
    let skills_dir = absolute_local(&options.skills_dir)?;
    let bundle = read_bundle(&source, options.agent)?;
    let current = read_bounded(&config_path, "configuration")?;
    let mut options = options.clone();
    options.skills_dir = skills_dir.clone();
    let parsed = parse_config(&current)?;
    let changes = check_conflicts(&parsed, &bundle.config, &bundle.skills, &options)?;
    let skill_before_hash = bundle
        .skills
        .iter()
        .map(|skill| {
            Ok((
                skill.name.clone(),
                skill_fingerprint(&skills_dir.join(&skill.name))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let id = next_id()?;
    let plan = StoredPlan {
        version: PLAN_VERSION,
        id: id.clone(),
        agent: bundle.agent,
        config_path,
        config_hash: hash(&current),
        source,
        source_hash: bundle.manifest_hash,
        skills_dir,
        skill_before_hash,
        key_env: options.key_env.clone(),
        import: bundle.config,
        skills: bundle.skills,
        replace: options.replace.clone(),
        changes: changes.clone(),
    };
    write_plan(&id, &plan)?;
    Ok(Preview { id, changes })
}

pub fn apply(id: &str) -> Result<ApplyResult> {
    validate_id(id)?;
    let _lock = PrivateLock::acquire(&sharing_dir()?.join("operations"))?;
    let plan: StoredPlan = read_json(
        &sharing_dir()?.join("plans").join(format!("{id}.json")),
        "Unknown import plan",
    )?;
    validate_plan(&plan, id)?;
    let current = read_bounded(&plan.config_path, "configuration")?;
    if hash(&current) != plan.config_hash {
        bail!("Configuration changed since preview; create a new import plan");
    }
    let bundle = read_bundle(&plan.source, plan.agent)?;
    if bundle.manifest_hash != plan.source_hash
        || bundle.config.profiles != plan.import.profiles
        || bundle.config.endpoints != plan.import.endpoints
        || bundle.skills != plan.skills
    {
        bail!("Portable source changed since preview; create a new import plan");
    }
    let config = parse_config(&current)?;
    let options = ImportOptions {
        agent: plan.agent,
        skills_dir: plan.skills_dir.clone(),
        key_env: plan.key_env.clone(),
        replace: plan.replace.clone(),
    };
    let _ = check_conflicts(&config, &plan.import, &plan.skills, &options)?;
    for (name, expected) in &plan.skill_before_hash {
        if skill_fingerprint(&plan.skills_dir.join(name))? != *expected {
            bail!("Skill target changed since preview; create a new import plan");
        }
    }
    let journal_path = sharing_dir()?.join("operations").join(format!("{id}.json"));
    let mut journal = prepare_journal(&plan, &current)?;
    write_private(&journal_path, &serde_json::to_vec(&journal)?)?;
    if let Err(error) = apply_journal(&journal, &current) {
        let rollback = rollback_journal(&journal);
        return match rollback {
            Ok(()) => Err(error.context("Sharing import failed and was rolled back; the recovery journal is retained")),
            Err(rollback) => Err(error.context(format!("Sharing import failed; rollback also failed: {rollback:#}; use the retained recovery journal"))),
        };
    }
    journal.phase = JournalPhase::Committed;
    if let Err(error) = write_private(&journal_path, &serde_json::to_vec(&journal)?) {
        return Err(error.context("Import completed but its recovery journal could not be marked committed; use restore with this operation ID"));
    }
    let _ =
        storage::remove_file_if_exists(&sharing_dir()?.join("plans").join(format!("{id}.json")));
    Ok(ApplyResult {
        id: id.to_owned(),
        changes: journal.changes,
    })
}

pub fn restore(id: &str) -> Result<()> {
    validate_id(id)?;
    let _lock = PrivateLock::acquire(&sharing_dir()?.join("operations"))?;
    let journal: SharingJournal = read_json(
        &sharing_dir()?.join("operations").join(format!("{id}.json")),
        "Unknown sharing operation",
    )?;
    if journal.id != id || !journal.config_path.is_absolute() || !journal.skills_dir.is_absolute() {
        bail!("Invalid sharing operation");
    }
    validate_journal(&journal)?;
    let current = read_bounded(&journal.config_path, "configuration")?;
    preflight_journal(&journal, &current)?;
    let mut journal = journal;
    journal.phase = JournalPhase::Restoring;
    write_private(
        &sharing_dir()?.join("operations").join(format!("{id}.json")),
        &serde_json::to_vec(&journal)?,
    )?;
    rollback_journal(&journal)?;
    storage::remove_file_if_exists(&sharing_dir()?.join("operations").join(format!("{id}.json")))?;
    Ok(())
}

struct Bundle {
    agent: Agent,
    config: PortableConfig,
    skills: Vec<PortableSkill>,
    manifest_hash: String,
}

fn portable_from_config(
    config: &Config,
    agent: Agent,
    selected: &[String],
) -> Result<PortableConfig> {
    let mut profiles = BTreeMap::new();
    let mut endpoint_names = BTreeSet::new();
    for name in selected {
        if !security::safe_identifier(name) {
            bail!("Invalid selected profile name");
        }
        let source = config
            .profiles
            .get(name)
            .context("Selected profile does not exist")?;
        if source.agent != agent {
            bail!("Portable export only includes profiles for the selected agent");
        }
        let portable = portable_profile(source);
        validate_profile(name, &portable, agent)?;
        endpoint_names.insert(portable.endpoint.clone());
        profiles.insert(name.clone(), portable);
    }
    let mut endpoints = BTreeMap::new();
    for name in endpoint_names {
        let endpoint = config
            .endpoints
            .get(&name)
            .context("Selected profile references an unknown endpoint")?;
        let portable = portable_endpoint(endpoint);
        validate_endpoint(&name, &portable)?;
        endpoints.insert(name, portable);
    }
    Ok(PortableConfig {
        version: BUNDLE_VERSION,
        endpoints,
        profiles,
    })
}

fn portable_profile(value: &Profile) -> PortableProfile {
    PortableProfile {
        agent: value.agent,
        endpoint: value.endpoint.clone(),
        label: value.label.clone(),
        description: value.description.clone(),
        tags: value.tags.clone(),
        order: value.order,
        model: value.model.clone(),
        reasoning: value.reasoning.clone(),
    }
}
fn portable_endpoint(value: &Endpoint) -> PortableEndpoint {
    match value {
        Endpoint::Native => PortableEndpoint::Native,
        Endpoint::ApiKey {
            protocol, base_url, ..
        } => PortableEndpoint::Api {
            protocol: *protocol,
            base_url: base_url.clone(),
        },
    }
}

fn validate_profile(name: &str, value: &PortableProfile, agent: Agent) -> Result<()> {
    if value.agent != agent
        || !security::safe_identifier(name)
        || !security::safe_identifier(&value.endpoint)
    {
        bail!("Portable profile is invalid at profile:{name}");
    }
    for (field, text) in [
        ("label", value.label.as_deref()),
        ("description", value.description.as_deref()),
        ("model", value.model.as_deref()),
        ("reasoning", value.reasoning.as_deref()),
    ] {
        if let Some(text) = text {
            validate_text(&format!("profile:{name}:{field}"), text)?;
        }
    }
    for tag in &value.tags {
        validate_text(&format!("profile:{name}:tags"), tag)?;
    }
    Ok(())
}

fn validate_endpoint(name: &str, value: &PortableEndpoint) -> Result<()> {
    if !security::safe_identifier(name) {
        bail!("Portable endpoint is invalid");
    }
    if let PortableEndpoint::Api { base_url, .. } = value {
        validate_text(&format!("endpoint:{name}:base_url"), base_url)?;
        let parsed = url::Url::parse(base_url)
            .map_err(|_| anyhow::anyhow!("Portable endpoint has an invalid URL"))?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            bail!("Portable endpoint has an unsupported URL scheme");
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!("Portable endpoint URL contains unsupported credentials or URL components");
        }
    }
    Ok(())
}

fn validate_text(label: &str, value: &str) -> Result<()> {
    if value.len() as u64 > security::MAX_TEXT_BYTES {
        bail!("Portable input exceeds the per-file size limit at {label}");
    }
    security::reject_secret(label, value)
}

fn validate_portable_config(config: &PortableConfig, agent: Agent) -> Result<()> {
    if config.version != BUNDLE_VERSION {
        bail!("Portable profiles are invalid");
    }
    for (name, endpoint) in &config.endpoints {
        validate_endpoint(name, endpoint)?;
    }
    for (name, profile) in &config.profiles {
        validate_profile(name, profile, agent)?;
        if !config.endpoints.contains_key(&profile.endpoint) {
            bail!("Portable profile references an unknown endpoint");
        }
    }
    Ok(())
}

fn validate_skill(skill: &PortableSkill) -> Result<()> {
    if !security::safe_identifier(&skill.name) || !skill.files.contains_key("SKILL.md") {
        bail!("Portable skill is invalid");
    }
    let mut total = 0_u64;
    for (path, text) in &skill.files {
        let relative = Path::new(path);
        if security::display_relative(relative).is_err() || !allowed_skill_path(relative) {
            bail!("Portable skill contains an unsupported content path");
        }
        total = total
            .checked_add(text.len() as u64)
            .context("Portable skill exceeds the total size limit")?;
        if total > security::MAX_TOTAL_TEXT_BYTES {
            bail!("Portable skill exceeds the total size limit");
        }
        validate_text(&format!("skill:{}:{path}", skill.name), text)?;
    }
    Ok(())
}

fn read_bundle(source: &Path, agent: Agent) -> Result<Bundle> {
    ensure_directory(source, "Portable source")?;
    let actual_files = scan_bundle_tree(source)?;
    let manifest_text = checked_read(&source.join("manifest.json"), "manifest.json")?;
    let manifest: Manifest = serde_json::from_str(&manifest_text)
        .map_err(|_| anyhow::anyhow!("Invalid portable manifest"))?;
    if manifest.version != BUNDLE_VERSION || manifest.agent != agent {
        bail!("Portable bundle is not compatible with the selected agent");
    }
    let config_text = checked_read(&source.join("profiles.toml"), "profiles.toml")?;
    verify_digest(&manifest, "profiles.toml", config_text.as_bytes())?;
    let config: PortableConfig =
        toml::from_str(&config_text).map_err(|_| anyhow::anyhow!("Invalid portable profiles"))?;
    if config.version != BUNDLE_VERSION {
        bail!("Portable profiles are invalid");
    }
    for (name, profile) in &config.profiles {
        validate_profile(name, profile, agent)?;
        if !config.endpoints.contains_key(&profile.endpoint) {
            bail!("Portable profile references an unknown endpoint");
        }
    }
    for (name, endpoint) in &config.endpoints {
        validate_endpoint(name, endpoint)?;
    }
    let skills = read_bundle_skills(source, &manifest)?;
    let total = manifest_text.len() as u64
        + config_text.len() as u64
        + skills
            .iter()
            .flat_map(|skill| skill.files.values())
            .map(|text| text.len() as u64)
            .sum::<u64>();
    if total > security::MAX_TOTAL_TEXT_BYTES {
        bail!("Portable bundle exceeds the total size limit");
    }
    let expected: BTreeSet<_> = manifest.files.keys().cloned().collect();
    let mut actual = BTreeSet::from(["profiles.toml".to_owned()]);
    for skill in &skills {
        for relative in skill.files.keys() {
            actual.insert(format!("skills/{}/{relative}", skill.name));
        }
    }
    if actual != expected {
        bail!("Portable manifest content paths do not match the bundle");
    }
    let mut expected_files = expected;
    expected_files.insert("manifest.json".to_owned());
    if actual_files != expected_files {
        bail!("Portable bundle contains unlisted or unsupported content");
    }
    if config.profiles.is_empty() && skills.is_empty() {
        bail!("Portable bundle has no selected content");
    }
    Ok(Bundle {
        agent: manifest.agent,
        config,
        skills,
        manifest_hash: hash(manifest_text.as_bytes()),
    })
}

fn scan_bundle_tree(root: &Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    scan_bundle_directory(root, root, &mut files, 0)?;
    Ok(files)
}

fn scan_bundle_directory(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<String>,
    depth: usize,
) -> Result<()> {
    if depth > security::MAX_DEPTH {
        bail!("Portable bundle exceeds the directory depth limit");
    }
    for entry in storage::read_directory(current)? {
        let path = current.join(&entry.name);
        if entry.kind == storage::EntryKind::Symlink {
            bail!("Portable bundle contains a symbolic link");
        }
        if entry.kind == storage::EntryKind::Directory {
            let count_before = files.len();
            scan_bundle_directory(root, &path, files, depth + 1)?;
            if files.len() == count_before {
                bail!("Portable bundle contains an empty or unsupported directory");
            }
            continue;
        }
        if entry.kind != storage::EntryKind::File {
            bail!("Portable bundle contains an unsupported file");
        }
        if files.len() >= security::MAX_FILES {
            bail!("Portable bundle exceeds the file limit");
        }
        let relative = security::display_relative(
            path.strip_prefix(root)
                .context("Portable bundle path is invalid")?,
        )?;
        files.insert(relative);
    }
    Ok(())
}

fn read_bundle_skills(source: &Path, manifest: &Manifest) -> Result<Vec<PortableSkill>> {
    let mut by_name: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (path, record) in &manifest.files {
        if path == "profiles.toml" {
            continue;
        }
        let relative = Path::new(path);
        if !path.starts_with("skills/") || !allowed_skill_path(relative) {
            bail!("Portable manifest contains an unsupported content path");
        }
        let mut parts = relative.components();
        let _ = parts.next();
        let name = component_text(parts.next().context("Portable skill path is invalid")?)?;
        if !security::safe_identifier(name) {
            bail!("Portable skill name is invalid");
        }
        let remainder = parts.as_path();
        let text = checked_read(&source.join(relative), path)?;
        verify_record(record, text.as_bytes())?;
        by_name
            .entry(name.to_owned())
            .or_default()
            .insert(security::display_relative(remainder)?, text);
    }
    let mut skills = Vec::new();
    for (name, files) in by_name {
        if !files.contains_key("SKILL.md") {
            bail!("Portable skill is missing SKILL.md");
        }
        skills.push(PortableSkill { name, files });
    }
    Ok(skills)
}

fn read_skill(root: &Path) -> Result<PortableSkill> {
    let root = absolute_local(root)?;
    ensure_directory(&root, "Selected skill")?;
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| security::safe_identifier(name))
        .context("Selected skill needs a safe directory name")?
        .to_owned();
    let mut files = BTreeMap::new();
    visit_skill(&root, &root, &mut files, &mut 0, 0)?;
    if !files.contains_key("SKILL.md") {
        bail!("Selected skill is missing SKILL.md");
    }
    Ok(PortableSkill { name, files })
}

fn skill_modes(root: &Path, skill: &PortableSkill) -> Result<SkillModes> {
    let mut files = BTreeMap::new();
    let mut directories = BTreeMap::new();
    for relative in skill.files.keys() {
        files.insert(
            relative.clone(),
            storage::entry_mode(&root.join(relative), storage::EntryKind::File)?,
        );
    }
    for directory in skill_directory_paths(skill)? {
        directories.insert(
            directory.clone(),
            storage::entry_mode(&root.join(directory), storage::EntryKind::Directory)?,
        );
    }
    Ok(SkillModes {
        root: storage::entry_mode(root, storage::EntryKind::Directory)?,
        files,
        directories,
    })
}

fn skill_directory_paths(skill: &PortableSkill) -> Result<BTreeSet<String>> {
    let mut directories = BTreeSet::new();
    for relative in skill.files.keys() {
        let mut parent = Path::new(relative).parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            directories.insert(security::display_relative(directory)?);
            parent = directory.parent();
        }
    }
    Ok(directories)
}

fn visit_skill(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, String>,
    total: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > security::MAX_DEPTH {
        bail!("Selected skill exceeds the directory depth limit");
    }
    for entry in storage::read_directory(current)? {
        let path = current.join(&entry.name);
        if entry.kind == storage::EntryKind::Symlink {
            bail!("Selected skill contains a symbolic link");
        }
        if entry.kind == storage::EntryKind::Directory {
            let file_count = files.len();
            visit_skill(root, &path, files, total, depth + 1)?;
            if files.len() == file_count {
                bail!("Selected skill contains an empty or unsupported directory");
            }
            continue;
        }
        if entry.kind != storage::EntryKind::File {
            bail!("Selected skill contains an unsupported file");
        }
        let relative = path
            .strip_prefix(root)
            .context("Invalid selected skill path")?;
        if !allowed_skill_path(relative) {
            bail!("Selected skill contains a script, binary, or unsupported file");
        }
        if files.len() >= security::MAX_FILES {
            bail!("Selected skill exceeds the file limit");
        }
        let label = security::display_relative(relative)?;
        let text = checked_read(&path, &label)?;
        *total = total
            .checked_add(text.len() as u64)
            .context("Selected skill is too large")?;
        if *total > security::MAX_TOTAL_TEXT_BYTES {
            bail!("Selected skill exceeds the total size limit");
        }
        files.insert(label, text);
    }
    Ok(())
}

fn allowed_skill_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == "SKILL.md" || name.ends_with(".md") || name.ends_with(".txt")
}

fn check_conflicts(
    config: &Config,
    import: &PortableConfig,
    skills: &[PortableSkill],
    options: &ImportOptions,
) -> Result<Vec<PlanChange>> {
    let mut changes = Vec::new();
    for (name, endpoint) in &import.endpoints {
        if let Some(existing) = config.endpoints.get(name) {
            let changed = portable_endpoint(existing) != *endpoint;
            if changed && !options.replace.contains(&format!("endpoint:{name}")) {
                bail!("Conflict endpoint:{name}; explicitly replace this item to continue");
            }
            if changed
                && matches!(endpoint, PortableEndpoint::Api { .. })
                && !options
                    .key_env
                    .get(name)
                    .is_some_and(|value| security::safe_environment_name(value))
            {
                bail!("Portable API endpoint requires a local --key-env binding");
            }
            changes.push(PlanChange {
                kind: "endpoint".into(),
                id: name.clone(),
                action: if portable_endpoint(existing) == *endpoint {
                    "unchanged"
                } else {
                    "replace"
                }
                .into(),
            });
        } else {
            if matches!(endpoint, PortableEndpoint::Api { .. })
                && !options
                    .key_env
                    .get(name)
                    .is_some_and(|value| security::safe_environment_name(value))
            {
                bail!("Portable API endpoint requires a local --key-env binding");
            }
            changes.push(PlanChange {
                kind: "endpoint".into(),
                id: name.clone(),
                action: "create".into(),
            });
        }
    }
    for (name, profile) in &import.profiles {
        if let Some(existing) = config.profiles.get(name) {
            if portable_profile(existing) != *profile
                && !options.replace.contains(&format!("profile:{name}"))
            {
                bail!("Conflict profile:{name}; explicitly replace this item to continue");
            }
            changes.push(PlanChange {
                kind: "profile".into(),
                id: name.clone(),
                action: if portable_profile(existing) == *profile {
                    "unchanged"
                } else {
                    "replace"
                }
                .into(),
            });
        } else {
            changes.push(PlanChange {
                kind: "profile".into(),
                id: name.clone(),
                action: "create".into(),
            });
        }
    }
    for skill in skills {
        let target = options.skills_dir.join(&skill.name);
        if storage::entry_kind(&target)?.is_some() {
            let existing = read_skill(&target).map_err(|_| {
                anyhow::anyhow!("Existing skill:{} cannot be safely journaled", skill.name)
            })?;
            if existing != *skill && !options.replace.contains(&format!("skill:{}", skill.name)) {
                bail!(
                    "Conflict skill:{}; explicitly replace this item to continue",
                    skill.name
                );
            }
            changes.push(PlanChange {
                kind: "skill".into(),
                id: skill.name.clone(),
                action: if existing == *skill {
                    "unchanged"
                } else {
                    "replace"
                }
                .into(),
            });
        } else {
            changes.push(PlanChange {
                kind: "skill".into(),
                id: skill.name.clone(),
                action: "create".into(),
            });
        }
    }
    Ok(changes)
}

fn prepare_journal(plan: &StoredPlan, current: &[u8]) -> Result<SharingJournal> {
    let before_config = parse_config(current)?;
    let mut profiles = BTreeMap::new();
    let mut endpoints = BTreeMap::new();
    for name in plan.import.profiles.keys() {
        profiles.insert(
            name.clone(),
            before_config
                .profiles
                .get(name)
                .map(portable_profile)
                .map(ManagedProfileBefore::Present)
                .unwrap_or(ManagedProfileBefore::Missing),
        );
    }
    for name in plan.import.endpoints.keys() {
        endpoints.insert(
            name.clone(),
            before_config
                .endpoints
                .get(name)
                .map(|endpoint| ManagedEndpointBefore::Present {
                    endpoint: portable_endpoint(endpoint),
                    key_env: match endpoint {
                        Endpoint::ApiKey { key_env, .. } => Some(key_env.clone()),
                        Endpoint::Native => None,
                    },
                })
                .unwrap_or(ManagedEndpointBefore::Missing),
        );
    }
    let mut skills_before = BTreeMap::new();
    for skill in &plan.skills {
        let target = plan.skills_dir.join(&skill.name);
        skills_before.insert(
            skill.name.clone(),
            match storage::entry_kind(&target)? {
                None => SkillBefore::Missing,
                Some(storage::EntryKind::Directory) => {
                    let skill = read_skill(&target)?;
                    SkillBefore::Present {
                        modes: skill_modes(&target, &skill)?,
                        skill,
                    }
                }
                Some(_) => bail!("Existing skill cannot be safely journaled"),
            },
        );
    }
    let changed = render_config(current, &plan.import, &plan.key_env)?;
    let skill_hash_before = plan
        .skills
        .iter()
        .map(|skill| {
            Ok((
                skill.name.clone(),
                skill_fingerprint(&plan.skills_dir.join(&skill.name))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let skill_hash_after = plan
        .skills
        .iter()
        .map(|skill| Ok((skill.name.clone(), hash(&serde_json::to_vec(skill)?))))
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(SharingJournal {
        version: PLAN_VERSION,
        id: plan.id.clone(),
        phase: JournalPhase::Pending,
        config_path: plan.config_path.clone(),
        config_hash_before: hash(current),
        config_hash_after: hash(&changed),
        source_hash: plan.source_hash.clone(),
        changes: plan.changes.clone(),
        import: plan.import.clone(),
        import_skills: plan.skills.clone(),
        key_env: plan.key_env.clone(),
        profiles,
        endpoints,
        skills_dir: plan.skills_dir.clone(),
        skills: skills_before,
        skill_hash_before,
        skill_hash_after,
    })
}

fn apply_journal(journal: &SharingJournal, current: &[u8]) -> Result<()> {
    let changed = render_config(current, &journal.import, &journal.key_env)?;
    if hash(&changed) != journal.config_hash_after {
        bail!("Import journal invariants are invalid");
    }
    atomic_write(&journal.config_path, &changed)?;
    apply_skills(&journal.skills_dir, &journal.import_skills)
}

fn render_config(
    current: &[u8],
    import: &PortableConfig,
    key_env: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let mut doc = std::str::from_utf8(current)?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Invalid Nomad TOML configuration"))?;
    let endpoints = doc["endpoints"].or_insert(toml_edit::table());
    let endpoints = endpoints
        .as_table_like_mut()
        .context("Nomad endpoints must be a table")?;
    for (name, endpoint) in &import.endpoints {
        let changed = !endpoints.contains_key(name)
            || portable_endpoint_from_item(endpoints.get(name))? != *endpoint;
        if changed {
            let binding = key_env.get(name).map(String::as_str);
            endpoints.insert(name, endpoint_item(endpoint, binding)?);
        }
    }
    let profiles = doc["profiles"].or_insert(toml_edit::table());
    let profiles = profiles
        .as_table_like_mut()
        .context("Nomad profiles must be a table")?;
    for (name, profile) in &import.profiles {
        set_profile(profiles, name, profile)?;
    }
    Ok(doc.to_string().into_bytes())
}

fn endpoint_item(value: &PortableEndpoint, key_env: Option<&str>) -> Result<toml_edit::Item> {
    let mut table = toml_edit::Table::new();
    match value {
        PortableEndpoint::Native => {
            table.insert("auth", toml_edit::value("native"));
        }
        PortableEndpoint::Api { protocol, base_url } => {
            let key_env =
                key_env.context("Portable API endpoint requires a local --key-env binding")?;
            table.insert("auth", toml_edit::value("api-key"));
            table.insert(
                "protocol",
                toml_edit::value(match protocol {
                    Protocol::AnthropicMessages => "anthropic-messages",
                    Protocol::OpenaiResponses => "openai-responses",
                }),
            );
            table.insert("base_url", toml_edit::value(base_url));
            table.insert("key_env", toml_edit::value(key_env));
        }
    }
    Ok(toml_edit::Item::Table(table))
}

fn portable_endpoint_from_item(item: Option<&toml_edit::Item>) -> Result<PortableEndpoint> {
    let item = item.context("Nomad endpoint is missing")?;
    let table = item
        .as_table_like()
        .context("Nomad endpoint must be a table")?;
    match table.get("auth").and_then(toml_edit::Item::as_str) {
        Some("native") => Ok(PortableEndpoint::Native),
        Some("api-key") => {
            let protocol = match table.get("protocol").and_then(toml_edit::Item::as_str) {
                Some("anthropic-messages") => Protocol::AnthropicMessages,
                Some("openai-responses") => Protocol::OpenaiResponses,
                _ => bail!("Nomad endpoint protocol is invalid"),
            };
            let base_url = table
                .get("base_url")
                .and_then(toml_edit::Item::as_str)
                .context("Nomad endpoint base URL is missing")?
                .to_owned();
            Ok(PortableEndpoint::Api { protocol, base_url })
        }
        _ => bail!("Nomad endpoint auth is invalid"),
    }
}

fn set_profile(
    table: &mut dyn toml_edit::TableLike,
    name: &str,
    profile: &PortableProfile,
) -> Result<()> {
    if !table.contains_key(name) {
        table.insert(name, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let profile_table = table
        .get_mut(name)
        .and_then(toml_edit::Item::as_table_like_mut)
        .context("Nomad profile must be a table")?;
    profile_table.insert("agent", toml_edit::value(agent_name(profile.agent)));
    profile_table.insert("endpoint", toml_edit::value(&profile.endpoint));
    set_optional(profile_table, "label", profile.label.as_deref());
    set_optional(profile_table, "description", profile.description.as_deref());
    set_optional(profile_table, "model", profile.model.as_deref());
    set_optional(profile_table, "reasoning", profile.reasoning.as_deref());
    let mut tags = toml_edit::Array::new();
    for tag in &profile.tags {
        tags.push(tag.as_str());
    }
    profile_table.insert("tags", toml_edit::value(tags));
    match profile.order {
        Some(order) => profile_table.insert("order", toml_edit::value(i64::from(order))),
        None => profile_table.remove("order"),
    };
    Ok(())
}

fn set_optional(table: &mut dyn toml_edit::TableLike, key: &str, value: Option<&str>) {
    match value {
        Some(value) => table.insert(key, toml_edit::value(value)),
        None => table.remove(key),
    };
}
fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    }
}

fn apply_skills(root: &Path, skills: &[PortableSkill]) -> Result<()> {
    ensure_skill_root(root)?;
    for skill in skills {
        install_skill(&root.join(&skill.name), skill, None)?;
    }
    Ok(())
}

fn install_skill(target: &Path, skill: &PortableSkill, modes: Option<&SkillModes>) -> Result<()> {
    let parent = target.parent().context("Skill target has no parent")?;
    ensure_skill_root(parent)?;
    let temporary = parent.join(format!(".nomad-share-{}", unique_name()?));
    create_private_dir(&temporary)?;
    let build = (|| {
        for (relative, text) in &skill.files {
            let mode = modes
                .and_then(|modes| modes.files.get(relative).copied())
                .unwrap_or(0o600);
            write_new_private_mode(&temporary.join(relative), text.as_bytes(), mode)?;
        }
        if let Some(modes) = modes {
            for (relative, mode) in &modes.directories {
                storage::set_entry_mode(
                    &temporary.join(relative),
                    *mode,
                    storage::EntryKind::Directory,
                )?;
            }
            storage::set_entry_mode(&temporary, modes.root, storage::EntryKind::Directory)?;
        }
        Ok(())
    })();
    if let Err(error) = build {
        let _ = storage::remove_tree(&temporary);
        return Err(error);
    }
    match storage::entry_kind(target)? {
        None => storage::rename_new(&temporary, target)?,
        Some(storage::EntryKind::Directory) => {
            storage::exchange_entries(&temporary, target)?;
            storage::remove_tree(&temporary)?;
        }
        Some(_) => {
            let _ = storage::remove_tree(&temporary);
            bail!("Existing skill cannot be safely replaced");
        }
    }
    Ok(())
}

fn preflight_journal(journal: &SharingJournal, current: &[u8]) -> Result<()> {
    let config_hash = hash(current);
    if config_hash != journal.config_hash_before && config_hash != journal.config_hash_after {
        bail!("Configuration changed after import; restore refused");
    }
    for name in journal.skills.keys() {
        let actual = skill_fingerprint(&journal.skills_dir.join(name))?;
        let before = journal
            .skill_hash_before
            .get(name)
            .context("Import journal is invalid")?;
        let after = journal
            .skill_hash_after
            .get(name)
            .context("Import journal is invalid")?;
        if actual != *before && actual.as_deref() != Some(after) {
            bail!("Skill target changed after import; restore refused");
        }
    }
    Ok(())
}

fn validate_journal(journal: &SharingJournal) -> Result<()> {
    if journal.version != PLAN_VERSION {
        bail!("Unsupported sharing operation");
    }
    for value in [
        &journal.config_hash_before,
        &journal.config_hash_after,
        &journal.source_hash,
    ] {
        validate_hash(value)?;
    }
    validate_portable_config(
        &journal.import,
        journal
            .import
            .profiles
            .values()
            .next()
            .map_or(Agent::Codex, |profile| profile.agent),
    )?;
    let imported = journal
        .import_skills
        .iter()
        .map(|skill| (skill.name.clone(), skill))
        .collect::<BTreeMap<_, _>>();
    if imported.len() != journal.import_skills.len()
        || journal.skills.len() != imported.len()
        || journal.skill_hash_before.len() != imported.len()
        || journal.skill_hash_after.len() != imported.len()
    {
        bail!("Invalid sharing operation");
    }
    for (name, before) in &journal.skills {
        if !security::safe_identifier(name) {
            bail!("Invalid sharing operation");
        }
        let skill = imported.get(name).context("Invalid sharing operation")?;
        let after = journal
            .skill_hash_after
            .get(name)
            .context("Invalid sharing operation")?;
        validate_hash(after)?;
        if after != &hash(&serde_json::to_vec(skill)?) {
            bail!("Invalid sharing operation");
        }
        let expected_before = journal
            .skill_hash_before
            .get(name)
            .context("Invalid sharing operation")?;
        if let Some(value) = expected_before {
            validate_hash(value)?;
        }
        match before {
            SkillBefore::Missing => {}
            SkillBefore::Present { skill, modes } => {
                validate_skill(skill)?;
                let expected_directories = skill_directory_paths(skill)?;
                if modes.root > 0o777
                    || modes.files.len() != skill.files.len()
                    || !modes.files.keys().eq(skill.files.keys())
                    || modes.files.values().any(|mode| *mode > 0o777)
                    || modes.directories.len() != expected_directories.len()
                    || !modes.directories.keys().eq(expected_directories.iter())
                    || modes.directories.values().any(|mode| *mode > 0o777)
                {
                    bail!("Invalid sharing operation");
                }
                let fingerprint = hash(&serde_json::to_vec(skill)?);
                if skill.name != *name || expected_before.as_deref() != Some(fingerprint.as_str()) {
                    bail!("Invalid sharing operation");
                }
            }
        }
    }
    for skill in &journal.import_skills {
        validate_skill(skill)?;
    }
    if journal.profiles.len() != journal.import.profiles.len()
        || journal.endpoints.len() != journal.import.endpoints.len()
        || !journal.profiles.keys().eq(journal.import.profiles.keys())
        || !journal.endpoints.keys().eq(journal.import.endpoints.keys())
    {
        bail!("Invalid sharing operation");
    }
    for (name, before) in &journal.profiles {
        if let ManagedProfileBefore::Present(profile) = before {
            validate_profile(name, profile, profile.agent)?;
        }
    }
    for (name, before) in &journal.endpoints {
        if let ManagedEndpointBefore::Present { endpoint, key_env } = before {
            validate_endpoint(name, endpoint)?;
            if key_env
                .as_deref()
                .is_some_and(|value| !security::safe_environment_name(value))
            {
                bail!("Invalid sharing operation");
            }
        }
    }
    for (name, value) in &journal.key_env {
        if !journal.import.endpoints.contains_key(name) || !security::safe_environment_name(value) {
            bail!("Invalid sharing operation");
        }
    }
    Ok(())
}

fn validate_plan(plan: &StoredPlan, id: &str) -> Result<()> {
    if plan.version != PLAN_VERSION {
        bail!("Unsupported import plan");
    }
    if plan.id != id
        || !plan.config_path.is_absolute()
        || !plan.source.is_absolute()
        || !plan.skills_dir.is_absolute()
    {
        bail!("Invalid import plan");
    }
    validate_hash(&plan.config_hash)?;
    validate_hash(&plan.source_hash)?;
    validate_portable_config(&plan.import, plan.agent)?;
    let names = plan
        .skills
        .iter()
        .map(|skill| {
            validate_skill(skill)?;
            Ok(skill.name.clone())
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if names.len() != plan.skills.len() || plan.skill_before_hash.len() != names.len() {
        bail!("Invalid import plan");
    }
    for name in &names {
        match plan.skill_before_hash.get(name) {
            Some(Some(value)) => validate_hash(value)?,
            Some(None) => {}
            None => bail!("Invalid import plan"),
        }
    }
    for (name, value) in &plan.key_env {
        if !plan.import.endpoints.contains_key(name) || !security::safe_environment_name(value) {
            bail!("Invalid import plan");
        }
    }
    for replacement in &plan.replace {
        let Some((kind, name)) = replacement.split_once(':') else {
            bail!("Invalid import plan");
        };
        let valid = match kind {
            "profile" => plan.import.profiles.contains_key(name),
            "endpoint" => plan.import.endpoints.contains_key(name),
            "skill" => names.contains(name),
            _ => false,
        };
        if !valid {
            bail!("Invalid import plan");
        }
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid sharing operation");
    }
    Ok(())
}

fn rollback_journal(journal: &SharingJournal) -> Result<()> {
    let current = read_bounded(&journal.config_path, "configuration")?;
    let config_hash = hash(&current);
    if config_hash == journal.config_hash_after {
        restore_config_values(
            &journal.config_path,
            &current,
            &journal.profiles,
            &journal.endpoints,
        )?;
    } else if config_hash != journal.config_hash_before {
        bail!("Configuration changed while recovering an import");
    }
    for (name, before) in &journal.skills {
        let target = journal.skills_dir.join(name);
        let actual = skill_fingerprint(&target)?;
        let expected_before = journal
            .skill_hash_before
            .get(name)
            .context("Import journal is invalid")?;
        let expected_after = journal
            .skill_hash_after
            .get(name)
            .context("Import journal is invalid")?;
        if actual == *expected_before {
            continue;
        }
        if actual.as_deref() != Some(expected_after) {
            bail!("Skill target changed while recovering an import");
        }
        restore_skill(&target, before)?;
    }
    Ok(())
}
fn restore_config_values(
    path: &Path,
    current: &[u8],
    profiles_before: &BTreeMap<String, ManagedProfileBefore>,
    endpoints_before: &BTreeMap<String, ManagedEndpointBefore>,
) -> Result<()> {
    let mut doc = std::str::from_utf8(current)?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Invalid Nomad TOML configuration"))?;
    let profiles = doc["profiles"]
        .as_table_like_mut()
        .context("Nomad profiles must be a table")?;
    for (name, before) in profiles_before {
        match before {
            ManagedProfileBefore::Missing => {
                profiles.remove(name);
            }
            ManagedProfileBefore::Present(profile) => set_profile(profiles, name, profile)?,
        }
    }
    let endpoints = doc["endpoints"]
        .as_table_like_mut()
        .context("Nomad endpoints must be a table")?;
    for (name, before) in endpoints_before {
        match before {
            ManagedEndpointBefore::Missing => {
                endpoints.remove(name);
            }
            ManagedEndpointBefore::Present { endpoint, key_env } => {
                endpoints.insert(name, endpoint_item(endpoint, key_env.as_deref())?);
            }
        }
    }
    atomic_write(path, doc.to_string().as_bytes())
}

fn restore_skill(target: &Path, before: &SkillBefore) -> Result<()> {
    match before {
        SkillBefore::Missing => {
            if storage::entry_kind(target)?.is_some() {
                remove_safe_skill(target)?;
            }
        }
        SkillBefore::Present { skill, modes } => install_skill(target, skill, Some(modes))?,
    }
    Ok(())
}

fn remove_safe_skill(path: &Path) -> Result<()> {
    let _ = read_skill(path)?;
    storage::remove_tree(path)
}

fn sharing_dir() -> Result<PathBuf> {
    let path = absolute_local(&state_dir()?.join("sharing"))?;
    ensure_or_create_private_dir(&path)?;
    Ok(path)
}
fn write_plan(id: &str, plan: &StoredPlan) -> Result<()> {
    let root = sharing_dir()?.join("plans");
    ensure_or_create_private_dir(&root)?;
    write_private(&root.join(format!("{id}.json")), &serde_json::to_vec(plan)?)
}
fn next_id() -> Result<String> {
    let serial = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("{stamp}:{}:{serial}", std::process::id()).as_bytes())
    ))
}
fn unique_name() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
}
fn validate_id(id: &str) -> Result<()> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid sharing operation ID");
    }
    Ok(())
}

fn ensure_directory(path: &Path, label: &str) -> Result<()> {
    match storage::entry_kind(path)? {
        Some(storage::EntryKind::Directory) => Ok(()),
        _ => bail!("{label} must be a directory and not a symbolic link"),
    }
}
fn absolute_local(path: &Path) -> Result<PathBuf> {
    if path.is_relative() {
        return resolve_from_trusted_root(&std::env::current_dir()?, path);
    }
    let temporary = std::env::temp_dir();
    if let Ok(relative) = path.strip_prefix(&temporary) {
        return resolve_from_trusted_root(&fs::canonicalize(temporary)?, relative);
    }
    #[cfg(target_os = "macos")]
    if let Ok(relative) = path.strip_prefix("/var") {
        return resolve_from_trusted_root(Path::new("/private/var"), relative);
    }
    #[cfg(target_os = "macos")]
    if let Ok(relative) = path.strip_prefix("/tmp") {
        return resolve_from_trusted_root(Path::new("/private/tmp"), relative);
    }
    resolve_from_trusted_root(
        Path::new("/"),
        path.strip_prefix("/").context("Portable path is invalid")?,
    )
}

fn resolve_from_trusted_root(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut result = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("Portable path is invalid");
        };
        result.push(part);
    }
    Ok(result)
}
fn skill_fingerprint(path: &Path) -> Result<Option<String>> {
    match storage::entry_kind(path)? {
        None => Ok(None),
        Some(storage::EntryKind::Directory) => {
            let skill = read_skill(path)?;
            Ok(Some(hash(&serde_json::to_vec(&skill)?)))
        }
        Some(_) => bail!("Skill target is not a safe directory"),
    }
}
fn ensure_or_create_private_dir(path: &Path) -> Result<()> {
    storage::ensure_private_dir(path)
}
fn ensure_skill_root(path: &Path) -> Result<()> {
    match storage::entry_kind(path)? {
        Some(storage::EntryKind::Directory) => Ok(()),
        Some(_) => bail!("Skills directory must be a directory and not a symbolic link"),
        None => storage::create_private_directory(path),
    }
}
fn create_private_dir(path: &Path) -> Result<()> {
    storage::create_private_directory(path)
}
fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    write_new_private_mode(path, bytes, 0o600)
}
fn write_new_private_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("Portable output has no parent")?;
    storage::ensure_private_dir(parent)?;
    storage::create_new_file(path, bytes, mode)
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Private output has no parent")?;
    ensure_or_create_private_dir(parent)?;
    atomic_write(path, bytes)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    storage::atomic_write(path, bytes, 0o600)
}
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, message: &str) -> Result<T> {
    let data = read_bounded(path, message)?;
    serde_json::from_slice(&data).map_err(|_| anyhow::anyhow!(message.to_owned()))
}

fn checked_read(path: &Path, label: &str) -> Result<String> {
    security::checked_text(label, read_bounded(path, label)?)
}
fn read_bounded(path: &Path, label: &str) -> Result<Vec<u8>> {
    storage::read_regular(path, security::MAX_TOTAL_TEXT_BYTES)?
        .map(|file| file.bytes)
        .context(format!("Cannot read {label}"))
}
fn parse_config(bytes: &[u8]) -> Result<Config> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid Nomad TOML configuration"))?;
    toml::from_str(text).map_err(|_| anyhow::anyhow!("Invalid Nomad TOML configuration"))
}
fn digest(bytes: &[u8]) -> FileDigest {
    FileDigest {
        sha256: hash(bytes),
        bytes: bytes.len() as u64,
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn verify_digest(manifest: &Manifest, path: &str, bytes: &[u8]) -> Result<()> {
    let record = manifest
        .files
        .get(path)
        .context("Portable manifest is missing a content path")?;
    verify_record(record, bytes)
}
fn verify_record(record: &FileDigest, bytes: &[u8]) -> Result<()> {
    if record.bytes != bytes.len() as u64 || record.sha256 != hash(bytes) {
        bail!("Portable content hash does not match its manifest");
    }
    Ok(())
}
fn component_text(component: Component<'_>) -> Result<&str> {
    match component {
        Component::Normal(value) => value.to_str().context("Portable path is not UTF-8"),
        _ => bail!("Portable path is invalid"),
    }
}

struct PrivateLock(PathBuf);
impl PrivateLock {
    fn acquire(root: &Path) -> Result<Self> {
        ensure_or_create_private_dir(root)?;
        let path = root.join(".lock");
        storage::create_lock(&path).context("Another sharing operation is active")?;
        Ok(Self(path))
    }
}
impl Drop for PrivateLock {
    fn drop(&mut self) {
        let _ = storage::remove_file_if_exists(&self.0);
    }
}
