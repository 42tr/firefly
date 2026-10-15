pub mod activity;

/// Child agents run until completion, timeout or cancellation, without a tool-call count cap.
#[cfg(test)]
fn subagent_limits(timeout: std::time::Duration) -> g::RunLimits {
    g::RunLimits { max_tool_calls: usize::MAX, timeout }
}

#[cfg(test)]
mod tests;

pub const FILE_SEARCH_INSTRUCTION: &str = "\n\n# 本地文件搜索\n\
搜索文件内容优先使用 rg（ripgrep）；按文件名定位优先使用 rg --files 配合 -g。不要默认使用 grep -R、grep -r 或 find 递归扫描整个工作区。\n\
先根据对话中的项目名和路径确定目标项目；项目未知时先浅层列出目录，定位候选项目，再搜索文件内容。\n\
command 显式设置目标项目的 cwd，搜索命令显式指定相关文件或源码目录；没有结果时按相关目录、项目、其他候选项目逐步扩大范围。\n\
默认遵守 .gitignore、.ignore 和 .rgignore，跳过依赖、构建产物和缓存目录；不要默认使用 --no-ignore、-uu 或 -uuu。只有任务需要时才在明确路径上搜索隐藏或被忽略文件。\n\
普通关键词用 rg -n -F，需要正则时再使用正则；仅需要匹配文件路径时使用 rg -l，减少无关输出。\n\
只有 command -v rg 确认 rg 不可用时，才回退到 grep，并保持同样的搜索范围。rg 退出码 1 表示没有匹配，不代表 rg 不可用，不应因此改用 grep 重扫。";
