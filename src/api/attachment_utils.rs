//! 附件上传公共工具函数。

/// 清理上传文件名，去除路径分隔符、控制字符并截断长度。
pub fn sanitize_file_name(raw: Option<&str>) -> String {
    let fallback = "未命名文件";
    let raw_name = raw.unwrap_or(fallback);
    let last_segment = raw_name.split(['/', '\\']).next_back().unwrap_or(fallback).trim();
    let filtered: String = last_segment.chars().filter(|c| !c.is_control()).collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}
