//! Workspace path validation and file access for Codex conversations.
use std::path::{Component, Path, PathBuf};
use anyhow::{anyhow, Context};
use serde::Serialize;
use tokio::fs;

use crate::config;

#[derive(Debug, Serialize)]
pub struct WorkspaceEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceListing {
    pub root: String,
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
}

pub async fn workspace_root() -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(config::get().workspace_root.clone());
    fs::create_dir_all(&path).await.context("创建 Agent workspace 失败")?;
    Ok(fs::canonicalize(path).await?)
}

/// 校验 workspace 相对路径：不允许绝对路径和 `..` 组件。
pub(crate) fn validate_relative_path(path: &str) -> anyhow::Result<()> {
    if Path::new(path).is_absolute() {
        return Err(anyhow!("Agent 工作目录必须是 workspace 下的相对路径"));
    }
    for component in Path::new(path).components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            return Err(anyhow!("Agent 工作目录包含非法路径组件"));
        }
    }
    Ok(())
}

pub async fn resolve_workspace_path(path: Option<&str>) -> anyhow::Result<(PathBuf, String)> {
    let root = workspace_root().await?;
    let relative = path.unwrap_or("").trim();
    validate_relative_path(relative)?;
    let candidate = if relative.is_empty() { root.clone() } else { root.join(relative) };
    let candidate = fs::canonicalize(&candidate).await.context("Agent 工作目录不存在")?;
    if !candidate.starts_with(&root) {
        return Err(anyhow!("Agent 工作目录必须位于 workspace 内"));
    }
    if !fs::metadata(&candidate).await?.is_dir() {
        return Err(anyhow!("Agent 工作目录不是目录"));
    }
    let display = candidate.strip_prefix(&root).unwrap_or(Path::new("")).to_string_lossy().replace('\\', "/");
    Ok((candidate, display))
}

pub async fn list_workspace(path: Option<&str>) -> anyhow::Result<WorkspaceListing> {
    let (directory, relative) = resolve_workspace_path(path).await?;
    let root = workspace_root().await?;
    let mut entries = Vec::new();
    let mut reader = fs::read_dir(directory).await?;
    while let Some(entry) = reader.next_entry().await? {
        let entry_path = entry.path();
        let resolved = match fs::canonicalize(&entry_path).await {
            Ok(path) if path.starts_with(&root) => path,
            _ => continue,
        };
        if !fs::metadata(&resolved).await.map(|meta| meta.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let child_path = if relative.is_empty() { name.clone() } else { format!("{}/{}", relative, name) };
        entries.push(WorkspaceEntry { name, path: child_path });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(WorkspaceListing { root: root.to_string_lossy().into_owned(), path: relative, entries })
}

pub async fn create_workspace_directory(parent: Option<&str>, name: &str) -> anyhow::Result<WorkspaceEntry> {
    let name = name.trim();
    let component = Path::new(name).components().next();
    if name.is_empty() || Path::new(name).components().count() != 1 || !matches!(component, Some(Component::Normal(_)))
    {
        return Err(anyhow!("目录名称只能包含一个有效的目录名"));
    }
    let (directory, relative) = resolve_workspace_path(parent).await?;
    let child = directory.join(name);
    fs::create_dir(&child).await.context("创建 Agent 工作目录失败")?;
    let child_path = if relative.is_empty() { name.to_string() } else { format!("{}/{}", relative, name) };
    Ok(WorkspaceEntry { name: name.to_string(), path: child_path })
}

fn strip_workspace_prefix(path: &str) -> Option<&str> {
    let marker = "workspace/";
    let index = path.find(marker)?;
    if index > 0 && !path.as_bytes().get(index - 1).is_some_and(|byte| *byte == b'/') {
        return None;
    }
    let relative = &path[index + marker.len()..];
    (!relative.is_empty()).then_some(relative)
}

pub async fn resolve_workspace_file(path: &str) -> anyhow::Result<(PathBuf, String)> {
    let root = workspace_root().await?;
    let relative = path.trim();
    if relative.is_empty() {
        return Err(anyhow!("文件路径不能为空"));
    }
    let candidate_path = if Path::new(relative).is_absolute() {
        let absolute = PathBuf::from(relative);
        if !absolute.starts_with(&root) {
            return Err(anyhow!("workspace 文件路径非法"));
        }
        absolute
    } else {
        validate_relative_path(relative)?;
        let candidate = root.join(relative);
        // Models sometimes copy the repository-relative workspace path into a
        // link (for example `firefly/workspace/report.html`). The public file
        // endpoint is workspace-relative, so retry that path after removing
        // the leading repository/workspace directories when the literal path
        // does not exist. Keep the literal candidate first so a real child
        // directory named `workspace` remains addressable.
        match fs::canonicalize(&candidate).await {
            Ok(_) => candidate,
            Err(_) => root.join(strip_workspace_prefix(relative).unwrap_or(relative)),
        }
    };
    let candidate = fs::canonicalize(candidate_path).await.context("workspace 文件不存在")?;
    if !candidate.starts_with(&root) {
        return Err(anyhow!("workspace 文件路径非法"));
    }
    if !fs::metadata(&candidate).await?.is_file() {
        return Err(anyhow!("workspace 路径不是文件"));
    }
    let display = candidate.strip_prefix(&root).unwrap_or(Path::new("/")).to_string_lossy().replace('\\', "/");
    Ok((candidate, display))
}

#[cfg(test)]
mod tests {
    use super::strip_workspace_prefix;

    #[test]
    fn strips_repository_workspace_prefix_from_compatibility_paths() {
        assert_eq!(strip_workspace_prefix("firefly/workspace/esp43/esp32-science.html"), Some("esp43/esp32-science.html"));
        assert_eq!(strip_workspace_prefix("workspace/esp43/esp32-science.html"), Some("esp43/esp32-science.html"));
        assert_eq!(strip_workspace_prefix("someworkspace/esp43.html"), None);
    }
}
