//! Hook installation into provider configs. Surgical by contract:
//! unrelated keys, ordering, and TOML formatting are preserved; targets are
//! backed up (`<file>.gw-backup`) before the first write; install and remove
//! are idempotent. See docs/protocol.md for patch semantics.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::Value as JsonValue;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value as TomlValue};

use crate::protocol::{FileFormat, ManagedFile, Manifest, Patch, PatchMode};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    AlreadyApplied,
}

/// Apply every hook patch of every manifest. Returns touched files.
pub fn install(manifests: &[Manifest]) -> Result<Vec<(PathBuf, Outcome)>> {
    apply(manifests, false)
}

/// Reverse `install`: remove `ensure` elements, keep `set` values.
pub fn remove(manifests: &[Manifest]) -> Result<Vec<(PathBuf, Outcome)>> {
    apply(manifests, true)
}

struct Target<'a> {
    path: PathBuf,
    format: FileFormat,
    patches: Vec<&'a Patch>,
    /// Ownership signatures (`gw hook <id>`) of the manifests touching this
    /// file: any array element mentioning one is a gw-managed entry.
    markers: Vec<String>,
}

fn apply(manifests: &[Manifest], removing: bool) -> Result<Vec<(PathBuf, Outcome)>> {
    let mut targets: Vec<Target<'_>> = Vec::new();
    for manifest in manifests {
        let marker = format!("gw hook {}", manifest.id);
        for hook in &manifest.hooks {
            let path = expand_path(&hook.path)?;
            if let Some(target) = targets.iter_mut().find(|target| target.path == path) {
                if target.format != hook.format {
                    bail!("conflicting formats for {}", path.display());
                }
                target.patches.extend(&hook.patches);
                if !target.markers.contains(&marker) {
                    target.markers.push(marker.clone());
                }
            } else {
                targets.push(Target {
                    path,
                    format: hook.format,
                    patches: hook.patches.iter().collect(),
                    markers: vec![marker.clone()],
                });
            }
        }
    }

    let mut managed = Vec::new();
    for manifest in manifests {
        for file in &manifest.managed_files {
            let path = expand_path(&file.path)?;
            if targets.iter().any(|target| target.path == path)
                || managed
                    .iter()
                    .any(|(existing, _, _): &(PathBuf, &str, &ManagedFile)| *existing == path)
            {
                bail!("duplicate or colliding setup target {}", path.display());
            }
            managed.push((path, manifest.id.as_str(), file));
        }
    }

    let mut outcomes: Vec<_> = targets
        .into_iter()
        .map(|target| {
            let outcome = apply_target(&target, removing)
                .with_context(|| format!("update {}", target.path.display()))?;
            Ok((target.path, outcome))
        })
        .collect::<Result<_>>()?;
    for (path, provider, file) in managed {
        let outcome = apply_managed_file(&path, provider, file, removing)
            .with_context(|| format!("update {}", path.display()))?;
        outcomes.push((path, outcome));
    }
    Ok(outcomes)
}

fn apply_managed_file(
    path: &Path,
    provider: &str,
    file: &ManagedFile,
    removing: bool,
) -> Result<Outcome> {
    if file.comment_prefix.is_empty() || file.comment_prefix.contains(['\n', '\r']) {
        bail!("managed file comment_prefix must be nonempty and single-line");
    }
    let existed = path.exists();
    if !existed && removing {
        return Ok(Outcome::AlreadyApplied);
    }
    let desired = render_managed(provider, file);
    if !existed {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(path, desired.as_bytes(), false)?;
        return Ok(Outcome::Changed);
    }
    let current = fs::read_to_string(path)?;
    if !removing && current == desired {
        return Ok(Outcome::AlreadyApplied);
    }
    validate_managed(&current, provider, &file.comment_prefix)?;
    let backup = backup_path(path);
    if !backup.exists() {
        fs::copy(path, backup)?;
    }
    if removing {
        fs::remove_file(path)?;
    } else {
        atomic_write(path, desired.as_bytes(), true)?;
    }
    Ok(Outcome::Changed)
}

fn render_managed(provider: &str, file: &ManagedFile) -> String {
    let hash = format!("{:x}", Sha256::digest(file.content.as_bytes()));
    format!(
        "{} Managed by gw for provider {}; content-sha256={}\n{}",
        file.comment_prefix, provider, hash, file.content
    )
}

fn validate_managed(input: &str, provider: &str, comment_prefix: &str) -> Result<()> {
    let (header, body) = input.split_once('\n').context("file is not gw-managed")?;
    let marker = format!("{comment_prefix} Managed by gw for provider {provider}; content-sha256=");
    let stored = header
        .strip_prefix(&marker)
        .context("file belongs to another owner or provider")?;
    if stored.len() != 64 || !stored.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("gw-managed file has an invalid content hash");
    }
    let actual = format!("{:x}", Sha256::digest(body.as_bytes()));
    if stored != actual {
        bail!("gw-managed file was modified");
    }
    Ok(())
}

fn apply_target(target: &Target<'_>, removing: bool) -> Result<Outcome> {
    let existed = target.path.exists();
    if removing && !existed {
        return Ok(Outcome::AlreadyApplied);
    }
    let input = if existed {
        fs::read_to_string(&target.path)?
    } else {
        String::new()
    };

    let output = match target.format {
        FileFormat::Json => {
            let mut document = if existed {
                serde_json::from_str(&input)?
            } else {
                JsonValue::Object(serde_json::Map::new())
            };
            let mut changed = false;
            for patch in &target.patches {
                changed |= apply_json_patch(&mut document, patch, removing)?;
            }
            // Prune orphans: gw-managed entries surviving from an older
            // manifest (install keeps only the current patch values,
            // uninstall keeps none). Scoped to the parent containers of the
            // current ensure pointers — orphans sit under sibling keys of
            // subscribed events. TOML targets carry only `set` flags, so
            // pruning is JSON-only.
            let mut parents: Vec<Vec<String>> = Vec::new();
            for patch in &target.patches {
                if patch.mode != PatchMode::Ensure {
                    continue;
                }
                let mut tokens = pointer_tokens(&patch.pointer)?;
                tokens.pop();
                if !parents.contains(&tokens) {
                    parents.push(tokens);
                }
            }
            let mut keep: Vec<(Vec<String>, &JsonValue)> = Vec::new();
            if !removing {
                for patch in &target.patches {
                    if patch.mode == PatchMode::Ensure {
                        keep.push((pointer_tokens(&patch.pointer)?, &patch.value));
                    }
                }
            }
            for parent in &parents {
                changed |= prune_gw_entries(&mut document, parent, &target.markers, &keep);
            }
            if !changed {
                return Ok(Outcome::AlreadyApplied);
            }
            let mut output = serde_json::to_vec_pretty(&document)?;
            output.push(b'\n');
            output
        }
        FileFormat::Toml => {
            let mut document = if existed {
                input.parse::<DocumentMut>()?
            } else {
                DocumentMut::new()
            };
            let mut changed = false;
            for patch in &target.patches {
                let tokens = pointer_tokens(&patch.pointer)?;
                changed |= apply_toml_patch(document.as_table_mut(), &tokens, patch, removing)?;
            }
            if !changed {
                return Ok(Outcome::AlreadyApplied);
            }
            document.to_string().into_bytes()
        }
    };

    if let Some(parent) = target.path.parent() {
        fs::create_dir_all(parent)?;
    }
    if existed {
        let backup = backup_path(&target.path);
        if !backup.exists() {
            fs::copy(&target.path, backup)?;
        }
    }
    atomic_write(&target.path, &output, existed)?;
    Ok(Outcome::Changed)
}

fn apply_json_patch(document: &mut JsonValue, patch: &Patch, removing: bool) -> Result<bool> {
    let tokens = pointer_tokens(&patch.pointer)?;
    match (patch.mode, removing) {
        (PatchMode::Ensure, false) => {
            let (target, _) = json_get_or_create(document, &tokens, &JsonValue::Array(Vec::new()))?;
            let array = target
                .as_array_mut()
                .with_context(|| format!("{} does not address an array", patch.pointer))?;
            if array.iter().any(|value| value == &patch.value) {
                Ok(false)
            } else {
                array.push(patch.value.clone());
                Ok(true)
            }
        }
        (PatchMode::Ensure, true) => json_remove_ensure(document, &tokens, &patch.value),
        (PatchMode::Set, false) => {
            let (target, created) = json_get_or_create(document, &tokens, &patch.value)?;
            if !created && target == &patch.value {
                Ok(false)
            } else {
                *target = patch.value.clone();
                Ok(true)
            }
        }
        (PatchMode::Set, true) => Ok(false),
    }
}

fn json_get_or_create<'a>(
    current: &'a mut JsonValue,
    tokens: &[String],
    leaf_default: &JsonValue,
) -> Result<(&'a mut JsonValue, bool)> {
    let Some((token, rest)) = tokens.split_first() else {
        return Ok((current, false));
    };
    let mut created = false;
    if current.is_null() {
        *current = container_for(token);
        created = true;
    }
    let child = match current {
        JsonValue::Object(object) => match object.entry(token.clone()) {
            serde_json::map::Entry::Occupied(entry) => entry.into_mut(),
            serde_json::map::Entry::Vacant(entry) => {
                created = true;
                entry.insert(if rest.is_empty() {
                    leaf_default.clone()
                } else {
                    container_for(&rest[0])
                })
            }
        },
        JsonValue::Array(array) => {
            let index = array_index(token, array.len())?;
            if index > array.len() {
                array.resize(index, JsonValue::Null);
                created = true;
            }
            if index == array.len() {
                array.push(if rest.is_empty() {
                    leaf_default.clone()
                } else {
                    container_for(&rest[0])
                });
                created = true;
            }
            &mut array[index]
        }
        _ => bail!("cannot descend through a non-container JSON value"),
    };
    let (target, child_created) = json_get_or_create(child, rest, leaf_default)?;
    Ok((target, created || child_created))
}

fn json_remove_ensure(
    current: &mut JsonValue,
    tokens: &[String],
    expected: &JsonValue,
) -> Result<bool> {
    let Some((token, rest)) = tokens.split_first() else {
        let array = current
            .as_array_mut()
            .context("ensure pointer does not address an array")?;
        let previous_len = array.len();
        array.retain(|value| value != expected);
        return Ok(array.len() != previous_len);
    };
    match current {
        JsonValue::Object(object) => {
            if rest.is_empty() {
                let Some(target) = object.get_mut(token) else {
                    return Ok(false);
                };
                let array = target
                    .as_array_mut()
                    .context("ensure pointer does not address an array")?;
                let previous_len = array.len();
                array.retain(|value| value != expected);
                let changed = array.len() != previous_len;
                if changed && array.is_empty() {
                    object.remove(token);
                }
                Ok(changed)
            } else {
                let Some(child) = object.get_mut(token) else {
                    return Ok(false);
                };
                json_remove_ensure(child, rest, expected)
            }
        }
        JsonValue::Array(array) => {
            let Some(index) = existing_array_index(token, array.len())? else {
                return Ok(false);
            };
            if rest.is_empty() {
                let target = array[index]
                    .as_array_mut()
                    .context("ensure pointer does not address an array")?;
                let previous_len = target.len();
                target.retain(|value| value != expected);
                let changed = target.len() != previous_len;
                if changed && target.is_empty() {
                    array.remove(index);
                }
                Ok(changed)
            } else {
                json_remove_ensure(&mut array[index], rest, expected)
            }
        }
        _ => bail!("cannot descend through a non-container JSON value"),
    }
}

fn apply_toml_patch(
    table: &mut dyn TableLike,
    tokens: &[String],
    patch: &Patch,
    removing: bool,
) -> Result<bool> {
    let Some((key, rest)) = tokens.split_first() else {
        bail!("TOML patch pointer must not be empty");
    };
    if !rest.is_empty() {
        if table.get(key).is_none() {
            if removing {
                return Ok(false);
            }
            table.insert(key, Item::Table(Table::new()));
        }
        let child = table
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .with_context(|| format!("{} is not a TOML table", key))?;
        return apply_toml_patch(child, rest, patch, removing);
    }

    match (patch.mode, removing) {
        (PatchMode::Ensure, false) => {
            if table.get(key).is_none() {
                table.insert(key, Item::Value(TomlValue::Array(Array::new())));
            }
            let array = table
                .get_mut(key)
                .and_then(Item::as_value_mut)
                .and_then(TomlValue::as_array_mut)
                .with_context(|| format!("{} does not address a TOML array", patch.pointer))?;
            if array
                .iter()
                .any(|value| toml_value_matches_json(value, &patch.value))
            {
                Ok(false)
            } else {
                array.push(json_to_toml_value(&patch.value)?);
                Ok(true)
            }
        }
        (PatchMode::Ensure, true) => {
            let Some(item) = table.get_mut(key) else {
                return Ok(false);
            };
            let array = item
                .as_value_mut()
                .and_then(TomlValue::as_array_mut)
                .with_context(|| format!("{} does not address a TOML array", patch.pointer))?;
            let previous_len = array.len();
            array.retain(|value| !toml_value_matches_json(value, &patch.value));
            let changed = array.len() != previous_len;
            if changed && array.is_empty() {
                table.remove(key);
            }
            Ok(changed)
        }
        (PatchMode::Set, false) => {
            if table
                .get(key)
                .is_some_and(|item| toml_item_matches_json(item, &patch.value))
            {
                Ok(false)
            } else {
                table.insert(key, Item::Value(json_to_toml_value(&patch.value)?));
                Ok(true)
            }
        }
        (PatchMode::Set, true) => Ok(false),
    }
}

/// In every array directly under the object at `parent`, remove elements
/// that mention a gw ownership marker but are not the value `keep` ensures
/// at that exact pointer — the same value ensured under a sibling key does
/// not shield an orphan. Elements not mentioning a marker are the user's;
/// kept elements are ours verbatim — neither is descended into. Arrays
/// emptied by pruning lose their key.
fn prune_gw_entries(
    document: &mut JsonValue,
    parent: &[String],
    markers: &[String],
    keep: &[(Vec<String>, &JsonValue)],
) -> bool {
    let Some(container) = json_lookup_mut(document, parent) else {
        return false;
    };
    let Some(object) = container.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let mut emptied = Vec::new();
    for (key, child) in object.iter_mut() {
        let Some(array) = child.as_array_mut() else {
            continue;
        };
        let kept: Vec<&JsonValue> = keep
            .iter()
            .filter(|(tokens, _)| {
                tokens.len() == parent.len() + 1
                    && tokens.starts_with(parent)
                    && tokens[parent.len()] == **key
            })
            .map(|(_, value)| *value)
            .collect();
        let previous_len = array.len();
        array.retain(|item| {
            kept.contains(&item) || {
                let text = item.to_string();
                !markers.iter().any(|marker| text.contains(marker.as_str()))
            }
        });
        if array.len() != previous_len {
            changed = true;
            if array.is_empty() {
                emptied.push(key.clone());
            }
        }
    }
    for key in emptied {
        object.remove(&key);
    }
    changed
}

fn json_lookup_mut<'a>(
    mut current: &'a mut JsonValue,
    tokens: &[String],
) -> Option<&'a mut JsonValue> {
    for token in tokens {
        current = match current {
            JsonValue::Object(object) => object.get_mut(token)?,
            JsonValue::Array(array) => array.get_mut(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn pointer_tokens(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let rest = pointer
        .strip_prefix('/')
        .with_context(|| format!("invalid pointer {pointer:?}"))?;
    rest.split('/').map(decode_pointer_token).collect()
}

fn decode_pointer_token(token: &str) -> Result<String> {
    let mut decoded = String::new();
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch != '~' {
            decoded.push(ch);
            continue;
        }
        match chars.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => bail!("invalid JSON pointer escape"),
        }
    }
    Ok(decoded)
}

fn container_for(token: &str) -> JsonValue {
    if token == "-" || token.parse::<usize>().is_ok() {
        JsonValue::Array(Vec::new())
    } else {
        JsonValue::Object(serde_json::Map::new())
    }
}

fn array_index(token: &str, len: usize) -> Result<usize> {
    if token == "-" {
        Ok(len)
    } else {
        token
            .parse()
            .with_context(|| format!("invalid array index {token:?}"))
    }
}

fn existing_array_index(token: &str, len: usize) -> Result<Option<usize>> {
    if token == "-" {
        return Ok(None);
    }
    let index: usize = token
        .parse()
        .with_context(|| format!("invalid array index {token:?}"))?;
    Ok((index < len).then_some(index))
}

fn json_to_toml_value(value: &JsonValue) -> Result<TomlValue> {
    match value {
        JsonValue::Null => bail!("TOML has no null value"),
        JsonValue::Bool(value) => Ok(TomlValue::from(*value)),
        JsonValue::Number(value) if value.is_i64() => Ok(TomlValue::from(value.as_i64().unwrap())),
        JsonValue::Number(value) if value.is_f64() => Ok(TomlValue::from(value.as_f64().unwrap())),
        JsonValue::Number(_) => bail!("integer does not fit in TOML's i64 range"),
        JsonValue::String(value) => Ok(TomlValue::from(value.as_str())),
        JsonValue::Array(values) => {
            let mut array = Array::new();
            for value in values {
                array.push(json_to_toml_value(value)?);
            }
            Ok(TomlValue::Array(array))
        }
        JsonValue::Object(values) => {
            let mut table = InlineTable::new();
            for (key, value) in values {
                table.insert(key, json_to_toml_value(value)?);
            }
            table.fmt();
            Ok(TomlValue::InlineTable(table))
        }
    }
}

fn toml_item_matches_json(item: &Item, expected: &JsonValue) -> bool {
    if let Some(value) = item.as_value() {
        return toml_value_matches_json(value, expected);
    }
    let Some(table) = item.as_table_like() else {
        return false;
    };
    let Some(expected) = expected.as_object() else {
        return false;
    };
    table.len() == expected.len()
        && expected.iter().all(|(key, expected)| {
            table
                .get(key)
                .is_some_and(|item| toml_item_matches_json(item, expected))
        })
}

fn toml_value_matches_json(value: &TomlValue, expected: &JsonValue) -> bool {
    match (value, expected) {
        (TomlValue::String(actual), JsonValue::String(expected)) => actual.value() == expected,
        (TomlValue::Integer(actual), JsonValue::Number(expected)) if expected.is_i64() => {
            Some(*actual.value()) == expected.as_i64()
        }
        (TomlValue::Float(actual), JsonValue::Number(expected)) if expected.is_f64() => {
            Some(*actual.value()) == expected.as_f64()
        }
        (TomlValue::Boolean(actual), JsonValue::Bool(expected)) => actual.value() == expected,
        (TomlValue::Array(actual), JsonValue::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| toml_value_matches_json(actual, expected))
        }
        (TomlValue::InlineTable(actual), JsonValue::Object(expected)) => {
            actual.len() == expected.len()
                && expected.iter().all(|(key, expected)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| toml_value_matches_json(actual, expected))
                })
        }
        _ => false,
    }
}

fn expand_path(path: &str) -> Result<PathBuf> {
    if path == "~" {
        return dirs::home_dir().context("could not determine home directory");
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(dirs::home_dir()
            .context("could not determine home directory")?
            .join(rest));
    }
    Ok(PathBuf::from(path))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut backup = OsString::from(path.as_os_str());
    backup.push(".gw-backup");
    PathBuf::from(backup)
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8], existed: bool) -> Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let permissions = if existed {
        Some(fs::metadata(path)?.permissions())
    } else {
        None
    };
    let mut temp_name = OsString::from(path.as_os_str());
    temp_name.push(format!(
        ".gw-tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = PathBuf::from(temp_name);
    let result = (|| -> Result<()> {
        let mut temp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)?;
        temp.write_all(contents)?;
        temp.sync_all()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temp_path, permissions)?;
        }
        fs::rename(&temp_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        Command, HookFile, ManagedFile, Manifest, Patch, PatchMode, ProcessMatch, PROTOCOL_VERSION,
    };
    use serde_json::json;

    fn manifest(path: &Path, format: FileFormat, patches: Vec<Patch>) -> Manifest {
        Manifest {
            protocol: PROTOCOL_VERSION,
            id: "fixture".to_owned(),
            label: "Fixture".to_owned(),
            color: None,
            process: ProcessMatch {
                argv0: vec!["fixture".to_owned()],
                exclude_args: Vec::new(),
            },
            launch: Command {
                argv: vec!["fixture".to_owned()],
            },
            resume: None,
            hooks: vec![HookFile {
                path: path.to_string_lossy().into_owned(),
                format,
                patches,
            }],
            managed_files: Vec::new(),
        }
    }

    fn ensure(pointer: &str, value: JsonValue) -> Patch {
        Patch {
            pointer: pointer.to_owned(),
            mode: PatchMode::Ensure,
            value,
        }
    }

    fn managed(path: &Path, content: &str) -> Manifest {
        let mut result = manifest(path, FileFormat::Json, Vec::new());
        result.hooks.clear();
        result.managed_files.push(ManagedFile {
            path: path.to_string_lossy().into_owned(),
            content: content.into(),
            comment_prefix: "//".into(),
        });
        result
    }

    #[test]
    fn managed_file_create_upgrade_conflicts_and_remove() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gw.ts");
        let old = managed(&path, "old\n");
        assert_eq!(
            install(std::slice::from_ref(&old)).unwrap()[0].1,
            Outcome::Changed
        );
        assert_eq!(
            install(std::slice::from_ref(&old)).unwrap()[0].1,
            Outcome::AlreadyApplied
        );
        let new = managed(&path, "new\n");
        assert_eq!(
            install(std::slice::from_ref(&new)).unwrap()[0].1,
            Outcome::Changed
        );
        assert!(backup_path(&path).exists());
        fs::write(&path, "unrelated").unwrap();
        assert!(install(std::slice::from_ref(&new)).is_err());
        fs::write(
            &path,
            render_managed("fixture", &new.managed_files[0]).replacen("//", "#", 1),
        )
        .unwrap();
        assert!(install(std::slice::from_ref(&new)).is_err());
        fs::write(
            &path,
            render_managed("fixture", &new.managed_files[0]) + "modified",
        )
        .unwrap();
        assert!(remove(std::slice::from_ref(&new)).is_err());
        fs::write(&path, render_managed("fixture", &new.managed_files[0])).unwrap();
        assert_eq!(
            remove(std::slice::from_ref(&new)).unwrap()[0].1,
            Outcome::Changed
        );
        assert_eq!(remove(&[new]).unwrap()[0].1, Outcome::AlreadyApplied);
    }

    fn set(pointer: &str, value: JsonValue) -> Patch {
        Patch {
            pointer: pointer.to_owned(),
            mode: PatchMode::Set,
            value,
        }
    }

    #[test]
    fn json_preserves_unrelated_keys_and_is_idempotent_both_ways() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let original = r#"{
  "z-odd/key": {"nested": {"keep": [3, 2, 1]}},
  "alpha": true,
  "hooks": {"Other": [{"command": "keep"}]}
}
"#;
        fs::write(&path, original).unwrap();
        let value = json!({"hooks": [{"type": "command", "command": "gw hook fixture"}]});
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", value.clone())],
        );

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let installed = fs::read_to_string(&path).unwrap();
        let document: JsonValue = serde_json::from_str(&installed).unwrap();
        assert_eq!(document["z-odd/key"]["nested"]["keep"], json!([3, 2, 1]));
        assert_eq!(document["alpha"], true);
        assert_eq!(document["hooks"]["Other"], json!([{"command": "keep"}]));
        assert_eq!(document["hooks"]["Stop"], json!([value]));
        assert!(installed.find("z-odd/key").unwrap() < installed.find("alpha").unwrap());
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), original);

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::AlreadyApplied
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), installed);

        assert_eq!(
            remove(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let removed = fs::read_to_string(&path).unwrap();
        let document: JsonValue = serde_json::from_str(&removed).unwrap();
        assert!(document["hooks"].get("Stop").is_none());
        assert_eq!(document["hooks"]["Other"], json!([{"command": "keep"}]));
        assert_eq!(remove(&[manifest]).unwrap()[0].1, Outcome::AlreadyApplied);
        assert_eq!(fs::read_to_string(&path).unwrap(), removed);
    }

    #[test]
    fn ensure_deduplicates_without_writing_or_backing_up() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let value = json!({"command": "gw hook fixture"});
        let original = format!("{{\"hooks\":{{\"Stop\":[{}]}}}}\n", value);
        fs::write(&path, &original).unwrap();
        let manifest = manifest(&path, FileFormat::Json, vec![ensure("/hooks/Stop", value)]);

        assert_eq!(install(&[manifest]).unwrap()[0].1, Outcome::AlreadyApplied);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn ensure_creates_intermediate_objects_and_arrays() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, "{}\n").unwrap();
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/providers/0/hooks", json!("gw hook fixture"))],
        );

        install(&[manifest]).unwrap();

        let document: JsonValue = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            document["providers"][0]["hooks"],
            json!(["gw hook fixture"])
        );
    }

    #[test]
    fn set_creates_missing_value_and_remove_leaves_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        fs::write(&path, "{}\n").unwrap();
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![set("/features/gw", json!(true))],
        );

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let installed = fs::read_to_string(&path).unwrap();
        let document: JsonValue = serde_json::from_str(&installed).unwrap();
        assert_eq!(document["features"]["gw"], true);
        assert_eq!(remove(&[manifest]).unwrap()[0].1, Outcome::AlreadyApplied);
        assert_eq!(fs::read_to_string(path).unwrap(), installed);
    }

    #[test]
    fn remove_only_target_element_and_cleans_empty_array() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let target = json!({"command": "gw hook fixture"});
        let keep = json!({"command": "keep"});
        fs::write(
            &path,
            serde_json::to_vec(&json!({"hooks": {"Stop": [keep.clone(), target.clone()]}}))
                .unwrap(),
        )
        .unwrap();
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", target.clone())],
        );

        remove(std::slice::from_ref(&manifest)).unwrap();
        let document: JsonValue = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["hooks"]["Stop"], json!([keep]));

        fs::write(
            &path,
            serde_json::to_vec(&json!({"hooks": {"Stop": [target]}})).unwrap(),
        )
        .unwrap();
        remove(&[manifest]).unwrap();
        let document: JsonValue = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert!(document["hooks"].get("Stop").is_none());
    }

    #[test]
    fn toml_preserves_comments_and_set_survives_remove() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let original = "# top comment\nodd = \"value\" # inline comment\n\n[nested]\nanswer=42 # answer comment\n";
        fs::write(&path, original).unwrap();
        let hook = json!({"command": "gw hook fixture", "enabled": true});
        let manifest = manifest(
            &path,
            FileFormat::Toml,
            vec![
                ensure("/hooks/Stop", hook),
                set("/features/gw", json!(true)),
            ],
        );

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let installed = fs::read_to_string(&path).unwrap();
        assert!(installed.contains("# top comment"));
        assert!(installed.contains("# inline comment"));
        assert!(installed.contains("answer=42 # answer comment"));
        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::AlreadyApplied
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), installed);

        assert_eq!(
            remove(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let removed = fs::read_to_string(&path).unwrap();
        assert!(removed.contains("# top comment"));
        let document = removed.parse::<DocumentMut>().unwrap();
        assert!(document["hooks"].get("Stop").is_none());
        assert_eq!(document["features"]["gw"].as_bool(), Some(true));
        assert_eq!(remove(&[manifest]).unwrap()[0].1, Outcome::AlreadyApplied);
        assert_eq!(fs::read_to_string(&path).unwrap(), removed);
    }

    #[test]
    fn install_prunes_stale_gw_entries_and_keeps_user_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let old_style = json!({"hooks": [{"type": "command", "command": "gw hook fixture"}]});
        let user = json!({"matcher": "Bash", "hooks": [{"command": "my-own-thing"}]});
        fs::write(
            &path,
            serde_json::to_vec(&json!({"hooks": {
                // Same event, superseded shape (no matcher) + a user entry.
                "Notification": [old_style.clone(), user.clone()],
                // Event gw no longer subscribes: entry goes, key follows.
                "Stop": [old_style.clone()],
                // User-only arrays are never touched.
                "PreToolUse": [user.clone()],
            }}))
            .unwrap(),
        )
        .unwrap();
        let current = json!({"matcher": "elicitation_dialog", "hooks": [{"type": "command", "command": "gw hook fixture"}]});
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![
                ensure("/hooks/Notification", current.clone()),
                // Same shape as the stale Notification entry: ensured under a
                // sibling key, it must not shield the orphan next door.
                ensure("/hooks/SessionStart", old_style.clone()),
            ],
        );

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let document: JsonValue = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["hooks"]["Notification"], json!([user, current]));
        assert_eq!(document["hooks"]["SessionStart"], json!([old_style]));
        assert!(document["hooks"].get("Stop").is_none());
        assert_eq!(document["hooks"]["PreToolUse"], json!([user]));

        assert_eq!(
            install(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::AlreadyApplied
        );
    }

    #[test]
    fn remove_prunes_orphaned_gw_entries_too() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let orphan = json!({"hooks": [{"type": "command", "command": "gw hook fixture"}]});
        let user = json!({"command": "keep"});
        fs::write(
            &path,
            serde_json::to_vec(&json!({"hooks": {"Stop": [orphan, user.clone()]}})).unwrap(),
        )
        .unwrap();
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![ensure(
                "/hooks/PermissionRequest",
                json!({"command": "gw hook fixture"}),
            )],
        );

        assert_eq!(
            remove(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::Changed
        );
        let document: JsonValue = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["hooks"]["Stop"], json!([user]));
        assert!(document["hooks"].get("PermissionRequest").is_none());
    }

    #[test]
    fn handles_missing_files_without_spurious_backups() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/settings.json");
        let manifest = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", json!("gw hook fixture"))],
        );

        assert_eq!(
            remove(std::slice::from_ref(&manifest)).unwrap()[0].1,
            Outcome::AlreadyApplied
        );
        assert!(!path.exists());
        assert_eq!(install(&[manifest]).unwrap()[0].1, Outcome::Changed);
        assert!(path.exists());
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn creates_backup_exactly_once_across_later_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let original = "{\"unrelated\":true}\n";
        fs::write(&path, original).unwrap();
        let first = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", json!("first"))],
        );
        let second = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", json!("second"))],
        );

        install(&[first]).unwrap();
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), original);
        install(&[second]).unwrap();
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), original);
    }

    #[test]
    fn groups_multiple_manifests_for_one_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let first = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", json!("first"))],
        );
        let second = manifest(
            &path,
            FileFormat::Json,
            vec![ensure("/hooks/Stop", json!("second"))],
        );

        let outcomes = install(&[first, second]).unwrap();

        assert_eq!(outcomes, vec![(path.clone(), Outcome::Changed)]);
        let document: JsonValue = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(document["hooks"]["Stop"], json!(["first", "second"]));
    }
}
