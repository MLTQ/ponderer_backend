use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::AgentConfig;
use crate::plugin_contract::{
    is_supported_plugin_protocol_version, PluginKind, PluginManifest,
    RuntimeProcessPluginPackageManifest, CURRENT_PLUGIN_MANIFEST_VERSION,
};
use crate::runtime_process_plugin::RuntimeProcessPluginBundle;

const MAX_DRAFT_FILE_BYTES: usize = 512 * 1024;
const MAX_DRAFT_FILES: usize = 256;
const MAX_DRAFT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKBENCH_DRAFTS: usize = 64;
const MAX_DISPLAY_NAME_BYTES: usize = 160;
const MAX_DESCRIPTION_BYTES: usize = 1_024;
const MAX_PACKAGE_FILES: usize = MAX_DRAFT_FILES;
const MAX_PACKAGE_BYTES: u64 = MAX_DRAFT_BYTES;
const MAX_STAGED_PACKAGES: usize = 128;
const MAX_STORE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginDraftSummary {
    pub id: String,
    pub path: PathBuf,
    pub manifest_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginDraftValidation {
    pub plugin_id: String,
    pub manifest: Option<PluginManifest>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl PluginDraftValidation {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty() && self.manifest.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StagedPluginPackage {
    pub plugin_id: String,
    pub version: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub staged_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PluginWorkbench {
    drafts_root: PathBuf,
    store_root: PathBuf,
    mutation_gate: Arc<Mutex<()>>,
}

impl PluginWorkbench {
    pub fn from_environment() -> Self {
        let config_root = AgentConfig::config_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let drafts_root = nonempty_env_path("PONDERER_PLUGIN_WORKBENCH_DIR")
            .unwrap_or_else(|| config_root.join("plugin-workbench"));
        let store_root = nonempty_env_path("PONDERER_PLUGIN_STORE_DIR")
            .unwrap_or_else(|| config_root.join("plugins").join("store"));
        Self::new(drafts_root, store_root)
    }

    pub fn new(drafts_root: PathBuf, store_root: PathBuf) -> Self {
        Self {
            drafts_root,
            store_root,
            mutation_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn drafts_root(&self) -> &Path {
        &self.drafts_root
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub fn list_drafts(&self) -> Result<Vec<PluginDraftSummary>> {
        ensure_directory(&self.drafts_root)?;
        let mut drafts = fs::read_dir(&self.drafts_root)
            .with_context(|| format!("read plugin workbench {:?}", self.drafts_root))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if !file_type.is_dir() || file_type.is_symlink() {
                    return None;
                }
                let id = entry.file_name().to_string_lossy().to_string();
                Some(PluginDraftSummary {
                    manifest_present: entry.path().join("plugin.toml").is_file(),
                    id,
                    path: entry.path(),
                })
            })
            .collect::<Vec<_>>();
        drafts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(drafts)
    }

    pub fn create_python_draft(
        &self,
        plugin_id: &str,
        display_name: &str,
        description: &str,
    ) -> Result<PluginDraftSummary> {
        let _guard = self.lock_mutations()?;
        validate_plugin_id(plugin_id)?;
        let display_name =
            require_single_line("display name", display_name, MAX_DISPLAY_NAME_BYTES)?;
        let description = require_single_line("description", description, MAX_DESCRIPTION_BYTES)?;
        ensure_directory(&self.drafts_root)?;
        if self.list_drafts()?.len() >= MAX_WORKBENCH_DRAFTS {
            anyhow::bail!(
                "plugin workbench is at its {} draft limit",
                MAX_WORKBENCH_DRAFTS
            );
        }
        let draft_path = self.drafts_root.join(plugin_id);
        if draft_path.exists() {
            anyhow::bail!("plugin draft '{}' already exists", plugin_id);
        }

        let module_name = python_module_name(plugin_id);
        fs::create_dir(&draft_path)
            .with_context(|| format!("create plugin draft {:?}", draft_path))?;
        let create_result = (|| -> Result<()> {
            fs::create_dir(draft_path.join(&module_name))?;
            write_new_file(
                &draft_path.join("plugin.toml"),
                &render_manifest(plugin_id, display_name, description, &module_name),
            )?;
            write_new_file(&draft_path.join("tools.json"), "{\n  \"tools\": []\n}\n")?;
            write_new_file(
                &draft_path.join("pyproject.toml"),
                &render_pyproject(plugin_id, &module_name),
            )?;
            write_new_file(
                &draft_path.join("README.md"),
                &render_readme(display_name, plugin_id),
            )?;
            write_new_file(&draft_path.join(&module_name).join("__init__.py"), "")?;
            write_new_file(
                &draft_path.join(&module_name).join("__init__.md"),
                &format!(
                    "# __init__.py\n\nPackage marker for the `{}` plugin draft.\n",
                    plugin_id
                ),
            )?;
            write_new_file(
                &draft_path.join(&module_name).join("server.py"),
                &render_server(plugin_id, display_name),
            )?;
            write_new_file(
                &draft_path.join(&module_name).join("server.md"),
                &render_server_doc(plugin_id),
            )?;
            Ok(())
        })();
        if let Err(error) = create_result {
            let _ = fs::remove_dir_all(&draft_path);
            return Err(error).context("initialize Python plugin draft");
        }

        Ok(PluginDraftSummary {
            id: plugin_id.to_string(),
            path: draft_path,
            manifest_present: true,
        })
    }

    pub fn write_draft_file(
        &self,
        plugin_id: &str,
        relative_path: &str,
        contents: &str,
    ) -> Result<PathBuf> {
        let _guard = self.lock_mutations()?;
        validate_plugin_id(plugin_id)?;
        if contents.len() > MAX_DRAFT_FILE_BYTES {
            anyhow::bail!(
                "draft file exceeds the {} byte workbench limit",
                MAX_DRAFT_FILE_BYTES
            );
        }
        let relative = validate_relative_path(relative_path)?;
        let draft_root = self.existing_draft_root(plugin_id)?;
        let destination = draft_root.join(&relative);
        ensure_no_symlink_ancestors(&draft_root, &destination)?;
        if destination.is_dir() {
            anyhow::bail!("draft destination {:?} is a directory", relative);
        }
        let usage = tree_usage(&draft_root)?;
        let existing = fs::metadata(&destination)
            .ok()
            .filter(|metadata| metadata.is_file());
        let existing_size = existing
            .as_ref()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let projected_files = usage.files + usize::from(existing.is_none());
        let projected_bytes = usage
            .bytes
            .saturating_sub(existing_size)
            .saturating_add(contents.len() as u64);
        if projected_files > MAX_DRAFT_FILES || projected_bytes > MAX_DRAFT_BYTES {
            anyhow::bail!("plugin draft exceeds workbench file or byte limits");
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, contents)
            .with_context(|| format!("write plugin draft file {:?}", relative))?;
        Ok(destination)
    }

    pub fn read_draft_file(&self, plugin_id: &str, relative_path: &str) -> Result<String> {
        validate_plugin_id(plugin_id)?;
        let relative = validate_relative_path(relative_path)?;
        let draft_root = self.existing_draft_root(plugin_id)?;
        let source = draft_root.join(&relative);
        ensure_no_symlink_ancestors(&draft_root, &source)?;
        let metadata = fs::metadata(&source)
            .with_context(|| format!("read plugin draft metadata {:?}", relative))?;
        if !metadata.is_file() || metadata.len() > MAX_DRAFT_FILE_BYTES as u64 {
            anyhow::bail!("draft file is missing, not regular, or too large");
        }
        fs::read_to_string(&source).with_context(|| format!("read plugin draft {:?}", relative))
    }

    pub fn validate_draft(&self, plugin_id: &str) -> Result<PluginDraftValidation> {
        validate_plugin_id(plugin_id)?;
        let draft_root = self.existing_draft_root(plugin_id)?;
        self.validate_package_root(plugin_id, &draft_root)
    }

    fn validate_package_root(
        &self,
        plugin_id: &str,
        package_root: &Path,
    ) -> Result<PluginDraftValidation> {
        let manifest_path = package_root.join("plugin.toml");
        let mut validation = PluginDraftValidation {
            plugin_id: plugin_id.to_string(),
            manifest: None,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        let raw = match fs::read_to_string(&manifest_path) {
            Ok(raw) => raw,
            Err(error) => {
                validation
                    .errors
                    .push(format!("cannot read plugin.toml: {error}"));
                return Ok(validation);
            }
        };
        let manifest_document: toml::Value = match toml::from_str(&raw) {
            Ok(document) => document,
            Err(error) => {
                validation
                    .errors
                    .push(format!("invalid plugin.toml: {error}"));
                return Ok(validation);
            }
        };
        let package: RuntimeProcessPluginPackageManifest = match toml::from_str(&raw) {
            Ok(package) => package,
            Err(error) => {
                validation
                    .errors
                    .push(format!("invalid plugin.toml: {error}"));
                return Ok(validation);
            }
        };
        if manifest_document.get("manifest_version").is_none() {
            validation
                .errors
                .push("workbench packages must explicitly declare manifest_version".to_string());
        }
        if manifest_document.get("protocol_version").is_none() {
            validation
                .errors
                .push("workbench packages must explicitly declare protocol_version".to_string());
        }
        if let Err(error) = RuntimeProcessPluginBundle::load_from_dir(package_root) {
            validation
                .errors
                .push(format!("invalid package contract: {error:#}"));
        }
        let manifest = package.plugin;
        if manifest.id != plugin_id {
            validation.errors.push(format!(
                "manifest id '{}' does not match draft directory '{}'",
                manifest.id, plugin_id
            ));
        }
        if manifest.kind != PluginKind::RuntimeProcessBundle {
            validation
                .errors
                .push("workbench v1 supports runtime_process packages only".to_string());
        }
        if manifest.manifest_version != CURRENT_PLUGIN_MANIFEST_VERSION {
            validation.errors.push(format!(
                "unsupported manifest version {}",
                manifest.manifest_version
            ));
        }
        if !is_supported_plugin_protocol_version(manifest.protocol_version) {
            validation.errors.push(format!(
                "unsupported protocol version {}",
                manifest.protocol_version
            ));
        }
        if package.command.is_empty() || package.command.iter().any(|part| part.trim().is_empty()) {
            validation
                .errors
                .push("runtime command must contain non-empty tokens".to_string());
        }
        if manifest.requested_capabilities.is_empty() {
            validation
                .warnings
                .push("manifest requests no host capabilities".to_string());
        }
        if manifest.declared_effects.is_empty() {
            validation
                .warnings
                .push("manifest declares no package-level semantic effects".to_string());
        }
        if manifest.contributions.is_none() {
            validation
                .errors
                .push("workbench packages require a static [contributions] contract".to_string());
        }
        collect_companion_doc_warnings(package_root, &mut validation.warnings)?;
        validation.manifest = Some(manifest);
        Ok(validation)
    }

    pub fn stage_draft(&self, plugin_id: &str) -> Result<StagedPluginPackage> {
        let _guard = self.lock_mutations()?;
        let validation = self.validate_draft(plugin_id)?;
        if !validation.is_valid() {
            anyhow::bail!(
                "plugin draft '{}' is invalid: {}",
                plugin_id,
                validation.errors.join("; ")
            );
        }
        let manifest = validation.manifest.context("validated manifest missing")?;
        validate_version_segment(&manifest.version)?;
        let draft_root = self.existing_draft_root(plugin_id)?;
        let package_root = self.store_root.join(plugin_id).join(&manifest.version);
        if package_root.exists() {
            anyhow::bail!(
                "staged package '{}@{}' already exists",
                plugin_id,
                manifest.version
            );
        }
        ensure_directory(&self.store_root)?;
        let store_usage = tree_usage(&self.store_root)?;
        let draft_usage = tree_usage(&draft_root)?;
        if count_staged_packages(&self.store_root)? >= MAX_STAGED_PACKAGES
            || store_usage.bytes.saturating_add(draft_usage.bytes) > MAX_STORE_BYTES
        {
            anyhow::bail!("plugin package store has reached its staging quota");
        }
        fs::create_dir_all(
            package_root
                .parent()
                .context("staged package path has no parent")?,
        )?;
        let temporary_root = package_root
            .parent()
            .context("staged package path has no parent")?
            .join(format!(".staging-{}", uuid::Uuid::new_v4()));
        let mut budget = CopyBudget::default();
        if let Err(error) = copy_package_tree(&draft_root, &temporary_root, &mut budget) {
            let _ = fs::remove_dir_all(&temporary_root);
            return Err(error).context("stage plugin package");
        }
        let snapshot_validation = self.validate_package_root(plugin_id, &temporary_root)?;
        if !snapshot_validation.is_valid() {
            let _ = fs::remove_dir_all(&temporary_root);
            anyhow::bail!(
                "copied plugin snapshot '{}' is invalid: {}",
                plugin_id,
                snapshot_validation.errors.join("; ")
            );
        }
        let staged_at = Utc::now();
        let staged = StagedPluginPackage {
            plugin_id: plugin_id.to_string(),
            version: manifest.version,
            path: package_root.clone(),
            enabled: false,
            staged_at,
        };
        let metadata = serde_json::to_string_pretty(&staged)?;
        if let Err(error) = write_new_file(&temporary_root.join(".ponderer-stage.json"), &metadata)
        {
            let _ = fs::remove_dir_all(&temporary_root);
            return Err(error).context("write staged package metadata");
        }
        if let Err(error) = fs::rename(&temporary_root, &package_root) {
            let _ = fs::remove_dir_all(&temporary_root);
            if package_root.exists() {
                anyhow::bail!(
                    "staged package '{}@{}' already exists",
                    plugin_id,
                    staged.version
                );
            }
            return Err(error).context("atomically publish staged plugin package");
        }
        Ok(staged)
    }

    fn existing_draft_root(&self, plugin_id: &str) -> Result<PathBuf> {
        let path = self.drafts_root.join(plugin_id);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("plugin draft '{}' does not exist", plugin_id))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            anyhow::bail!("plugin draft '{}' is not a regular directory", plugin_id);
        }
        Ok(path)
    }

    fn lock_mutations(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.mutation_gate
            .lock()
            .map_err(|error| anyhow::anyhow!("plugin workbench lock poisoned: {error}"))
    }
}

#[derive(Default)]
struct CopyBudget {
    files: usize,
    bytes: u64,
}

fn copy_package_tree(source: &Path, destination: &Path, budget: &mut CopyBudget) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if should_skip_package_entry(&file_name) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!(
                "plugin packages may not contain symlinks: {:?}",
                entry.path()
            );
        }
        let target = destination.join(&file_name);
        if file_type.is_dir() {
            copy_package_tree(&entry.path(), &target, budget)?;
        } else if file_type.is_file() {
            let size = entry.metadata()?.len();
            budget.files = budget.files.saturating_add(1);
            budget.bytes = budget.bytes.saturating_add(size);
            if budget.files > MAX_PACKAGE_FILES || budget.bytes > MAX_PACKAGE_BYTES {
                anyhow::bail!("plugin package exceeds workbench staging limits");
            }
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn tree_usage(root: &Path) -> Result<CopyBudget> {
    let mut usage = CopyBudget::default();
    if root.exists() {
        collect_tree_usage(root, &mut usage)?;
    }
    Ok(usage)
}

fn collect_tree_usage(root: &Path, usage: &mut CopyBudget) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!(
                "workbench trees may not contain symlinks: {:?}",
                entry.path()
            );
        }
        if file_type.is_dir() {
            collect_tree_usage(&entry.path(), usage)?;
        } else if file_type.is_file() {
            usage.files = usage.files.saturating_add(1);
            usage.bytes = usage.bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(())
}

fn count_staged_packages(store_root: &Path) -> Result<usize> {
    let mut count = 0usize;
    if !store_root.exists() {
        return Ok(count);
    }
    for plugin_entry in fs::read_dir(store_root)? {
        let plugin_entry = plugin_entry?;
        if !plugin_entry.file_type()?.is_dir() {
            continue;
        }
        for version_entry in fs::read_dir(plugin_entry.path())? {
            let version_entry = version_entry?;
            if version_entry.file_type()?.is_dir()
                && !version_entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".staging-")
            {
                count = count.saturating_add(1);
            }
        }
    }
    Ok(count)
}

fn should_skip_package_entry(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == ".venv"
        || name == "__pycache__"
        || name == ".DS_Store"
        || name.ends_with(".egg-info")
        || name == ".ponderer-stage.json"
}

fn collect_companion_doc_warnings(root: &Path, warnings: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            warnings.push(format!(
                "symlink will be rejected during staging: {}",
                entry.path().display()
            ));
            continue;
        }
        if file_type.is_dir() {
            collect_companion_doc_warnings(&entry.path(), warnings)?;
            continue;
        }
        let path = entry.path();
        if matches!(path.extension().and_then(OsStr::to_str), Some("py" | "rs"))
            && !path.with_extension("md").is_file()
        {
            warnings.push(format!(
                "code file has no companion documentation: {}",
                path.strip_prefix(root).unwrap_or(&path).display()
            ));
        }
    }
    Ok(())
}

fn validate_plugin_id(plugin_id: &str) -> Result<()> {
    let valid = !plugin_id.is_empty()
        && plugin_id.len() <= 96
        && plugin_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
        })
        && plugin_id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && !plugin_id.contains("..")
        && !plugin_id.ends_with(['.', '-', '_']);
    if !valid {
        anyhow::bail!(
            "plugin id must be 1-96 lowercase ASCII characters using letters, digits, '.', '-', or '_'"
        );
    }
    Ok(())
}

fn validate_version_segment(version: &str) -> Result<()> {
    if version.is_empty()
        || version.len() > 64
        || version == "."
        || version == ".."
        || version.contains(['/', '\\'])
    {
        anyhow::bail!("plugin version is not safe for package storage");
    }
    Ok(())
}

fn validate_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        anyhow::bail!("draft path must be a non-empty relative path without traversal");
    }
    Ok(path.to_path_buf())
}

fn ensure_no_symlink_ancestors(root: &Path, destination: &Path) -> Result<()> {
    let relative = destination
        .strip_prefix(root)
        .context("draft path escaped workbench root")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("draft path may not traverse symlinks");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn require_single_line<'a>(label: &str, value: &'a str, max_bytes: usize) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\n', '\r']) || value.len() > max_bytes {
        anyhow::bail!("{label} must be a non-empty bounded single line");
    }
    Ok(value)
}

fn python_module_name(plugin_id: &str) -> String {
    plugin_id
        .chars()
        .map(|ch| if ch == '-' || ch == '.' { '_' } else { ch })
        .collect()
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn ensure_directory(path: &Path) -> Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            anyhow::bail!("workbench path {:?} is not a regular directory", path);
        }
    } else {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

fn write_new_file(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {:?}", path))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings always serialize")
}

fn render_manifest(id: &str, name: &str, description: &str, module: &str) -> String {
    format!(
        "manifest_version = 1\nprotocol_version = 1\nid = {}\nname = {}\nversion = \"0.1.0\"\ndescription = {}\nplugin_type = \"runtime_process\"\ncommand = [\"python3\", \"-m\", \"{}.server\"]\ntool_contract_file = \"tools.json\"\nrequested_capabilities = []\ndeclared_effects = []\n\n[contributions]\nevent_hooks = []\nprompt_slots = []\npoll_events = false\n",
        toml_string(id),
        toml_string(name),
        toml_string(description),
        module
    )
}

fn render_pyproject(id: &str, module: &str) -> String {
    format!(
        "[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[project]\nname = {}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = [\"ponderer-plugin-sdk>=0.1.0\"]\n\n[tool.setuptools]\npackages = [{}]\n",
        toml_string(id),
        toml_string(module)
    )
}

fn render_readme(name: &str, id: &str) -> String {
    format!(
        "# {}\n\nPonderer plugin draft `{}`. It remains inert in the workbench until validated, staged, granted capabilities, and explicitly enabled.\n",
        name, id
    )
}

fn render_server(id: &str, name: &str) -> String {
    format!(
        "from ponderer_plugin_sdk import Plugin, PluginMetadata, serve_stdio\n\nplugin = Plugin(PluginMetadata({}, {}, \"0.1.0\"))\n\n\ndef main() -> int:\n    return serve_stdio(plugin)\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n",
        toml_string(id),
        toml_string(name)
    )
}

fn render_server_doc(id: &str) -> String {
    format!(
        "# server.py\n\n## Purpose\n\nProtocol-v1 entrypoint for `{}`. Add SDK-decorated tools, event handlers, and prompt contributions here; keep complete tool schemas/effects in `tools.json` and other authority in `plugin.toml`.\n",
        id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workbench() -> (PluginWorkbench, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("ponderer_workbench_{}", uuid::Uuid::new_v4()));
        (
            PluginWorkbench::new(root.join("drafts"), root.join("store")),
            root,
        )
    }

    #[test]
    fn scaffold_validates_and_stages_disabled() {
        let (workbench, root) = workbench();
        let draft = workbench
            .create_python_draft("dev.clock", "Clock", "Observes local time")
            .expect("draft");
        assert!(draft.path.join("dev_clock/server.py").is_file());
        assert_eq!(
            fs::read_to_string(draft.path.join("tools.json")).unwrap(),
            "{\n  \"tools\": []\n}\n"
        );
        let server = fs::read_to_string(draft.path.join("dev_clock/server.py")).unwrap();
        assert!(server.contains("PluginMetadata"));
        assert!(server.contains("serve_stdio"));
        let validation = workbench.validate_draft("dev.clock").expect("validation");
        assert!(validation.is_valid(), "{:?}", validation.errors);
        let staged = workbench.stage_draft("dev.clock").expect("stage");
        assert!(!staged.enabled);
        assert!(staged.path.join(".ponderer-stage.json").is_file());
        assert!(workbench
            .stage_draft("dev.clock")
            .expect_err("immutable version should not overwrite")
            .to_string()
            .contains("already exists"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn draft_io_rejects_traversal_and_symlinks() {
        let (workbench, root) = workbench();
        workbench
            .create_python_draft("safe-plugin", "Safe", "Safe draft")
            .expect("draft");
        assert!(workbench
            .write_draft_file("safe-plugin", "../escape", "bad")
            .is_err());
        workbench
            .write_draft_file("safe-plugin", "safe/note.txt", "hello")
            .expect("write");
        assert_eq!(
            workbench
                .read_draft_file("safe-plugin", "safe/note.txt")
                .expect("read"),
            "hello"
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", root.join("drafts/safe-plugin/link"))
                .expect("symlink");
            assert!(workbench
                .write_draft_file("safe-plugin", "link/escape.txt", "bad")
                .is_err());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_manifest_returns_structured_validation_errors() {
        let (workbench, root) = workbench();
        workbench
            .create_python_draft("dev.bad", "Bad", "Bad manifest")
            .expect("draft");
        workbench
            .write_draft_file("dev.bad", "plugin.toml", "not = [valid")
            .expect("write malformed manifest");
        let validation = workbench.validate_draft("dev.bad").expect("validation");
        assert!(!validation.is_valid());
        assert!(validation.errors[0].contains("invalid plugin.toml"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workbench_rejects_legacy_authority_downgrade() {
        let (workbench, root) = workbench();
        workbench
            .create_python_draft("dev.downgrade", "Downgrade", "Authority fixture")
            .expect("draft");
        workbench
            .write_draft_file(
                "dev.downgrade",
                "plugin.toml",
                r#"
manifest_version = 1
protocol_version = 1
id = "dev.downgrade"
name = "Downgrade"
version = "0.1.0"
description = "fixture"
plugin_type = "runtime_process"
command = ["python3"]
"#,
            )
            .expect("write downgraded manifest");

        let validation = workbench.validate_draft("dev.downgrade").unwrap();
        assert!(!validation.is_valid());
        assert!(validation
            .errors
            .iter()
            .any(|error| error.contains("[contributions]")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scaffold_metadata_is_bounded() {
        let (workbench, root) = workbench();
        assert!(workbench
            .create_python_draft(
                "dev.too-large",
                &"x".repeat(MAX_DISPLAY_NAME_BYTES + 1),
                "description",
            )
            .is_err());
        assert!(workbench.list_drafts().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_staging_preserves_the_single_winning_package() {
        let (workbench, root) = workbench();
        workbench
            .create_python_draft("dev.race", "Race", "Race fixture")
            .expect("draft");
        let left = workbench.clone();
        let right = workbench.clone();
        let first = std::thread::spawn(move || left.stage_draft("dev.race"));
        let second = std::thread::spawn(move || right.stage_draft("dev.race"));
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
        let package = root.join("store/dev.race/0.1.0");
        assert!(package.join("plugin.toml").is_file());
        assert!(package.join(".ponderer-stage.json").is_file());
        let _ = fs::remove_dir_all(root);
    }
}
