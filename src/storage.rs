use std::path::PathBuf;

use tokio::fs;
use uuid::Uuid;

use crate::config;

fn ensure_absolute(dir: &str) -> PathBuf {
    let path = PathBuf::from(dir);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
    }
}

/// 保存工具生成的附件到上传目录。
///
/// 返回文件的完整存储路径。
pub async fn save_tool_attachment(bytes: &[u8]) -> anyhow::Result<String> {
    let path = ensure_absolute(&config::get().assistant_upload_dir);
    fs::create_dir_all(&path).await?;
    let file_id = Uuid::new_v4().to_string();
    let full_path = path.join(file_id);
    fs::write(&full_path, bytes).await?;
    Ok(full_path.to_string_lossy().to_string())
}
