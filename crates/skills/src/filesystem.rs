//! Import skills stored as markdown files under the execlaw data directory.

use crate::model::{NewSkill, NewSkillVersion, RegistrationKind, SkillError};
use crate::store::SkillStore;
use std::path::{Path, PathBuf};

/// Import every `SKILL.md` below `root`.
///
/// The stored name is the sanitized relative directory, for example
/// `skills/Research/Gather/SKILL.md` becomes `research/gather`. Existing
/// filesystem skills are left untouched when their body has not changed.
pub fn import_filesystem_skills(
    store: &SkillStore,
    root: &Path,
    now_ms: i64,
) -> Result<usize, SkillError> {
    if !root.exists() {
        return Ok(0);
    }

    let mut imported = 0;
    for entry in walk_skill_files(root)? {
        let body = std::fs::read_to_string(&entry).map_err(|e| io_error(&entry, e))?;
        let relative_dir = entry
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .unwrap_or_else(|| Path::new(""));
        let name = relative_dir
            .components()
            .map(|component| crate::sanitize_local_name(&component.as_os_str().to_string_lossy()))
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        if name.is_empty() {
            continue;
        }

        let changed = store
            .get(&name)?
            .map(|skill| skill.current_version.body_md != body)
            .unwrap_or(true);
        if !changed {
            continue;
        }

        let new = NewSkill {
            name,
            source: format!("filesystem:{}", root.display()),
            registration_kind: RegistrationKind::Shipped,
            owning_plugin_id: Some("filesystem".into()),
            initial_version: NewSkillVersion {
                description: "Filesystem skill".into(),
                body_md: body,
                frontmatter_json: "{}".into(),
                authored_by: "filesystem".into(),
                promotion_notes: None,
            },
            resources: vec![],
        };
        store.import_shipped(new, now_ms)?;
        imported += 1;
    }
    Ok(imported)
}

fn walk_skill_files(root: &Path) -> Result<Vec<PathBuf>, SkillError> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| io_error(&dir, e))? {
            let entry = entry.map_err(|e| io_error(&dir, e))?;
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|name| name == "SKILL.md") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn io_error(path: &Path, error: std::io::Error) -> SkillError {
    SkillError::Db(execlaw_core::db::DbError::Io(std::io::Error::new(
        error.kind(),
        format!("reading filesystem skill {}: {error}", path.display()),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::db::{Database, DbConfig};
    use execlaw_core::migrations::MigrationRunner;
    use tempfile::tempdir;

    #[test]
    fn imports_nested_skill_files_once_and_updates_changed_content() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = SkillStore::new(db);
        let root = tempdir().unwrap();
        let skill_dir = root.path().join("Research").join("Gather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_path = skill_dir.join("SKILL.md");
        std::fs::write(&skill_path, "v1").unwrap();

        assert_eq!(import_filesystem_skills(&store, root.path(), 1).unwrap(), 1);
        assert_eq!(
            store
                .get("research/gather")
                .unwrap()
                .unwrap()
                .current_version
                .version,
            1
        );
        assert_eq!(import_filesystem_skills(&store, root.path(), 2).unwrap(), 0);

        std::fs::write(skill_path, "v2").unwrap();
        assert_eq!(import_filesystem_skills(&store, root.path(), 3).unwrap(), 1);
        let skill = store.get("research/gather").unwrap().unwrap();
        assert_eq!(skill.current_version.version, 2);
        assert_eq!(skill.current_version.body_md, "v2");
    }
}
