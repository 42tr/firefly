//! Native workspace file worker. The descriptor-anchored safe primitives
//! (openat + O_NOFOLLOW traversal, listing, image checks, ZIP archiving)
//! live in glow-agent's fs feature; this module keeps Firefly's worker
//! CLI protocol on top of them.
use std::{
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{ensure, Context};
use zip::ZipWriter;

pub use g::fs::{
    archive_directory, archive_prefix, check_image, listing, names, open_child, open_path,
    stat_child,
};

/// Workers keep downloads streaming and ZIP preparation isolated from the server.
pub fn run(root: &Path, relative: &str, operation: &str) -> anyhow::Result<()> {
    if operation == "list" {
        println!("{}", listing(root, relative)?);
        return Ok(());
    }
    let mut file = open_path(root, relative)?;
    if operation == "check" {
        println!("{{}}");
        return Ok(());
    }
    let operation = if operation == "image" {
        check_image(&file, relative)?;
        "download"
    } else {
        operation
    };
    ensure!(matches!(operation, "download" | "archive"), "未知文件操作");
    ensure!(
        file.metadata()?.is_dir() == (operation == "archive"),
        "文件类型已变化，请刷新目录后重试"
    );
    if operation == "archive" {
        let temporary = tempfile::tempfile().context("创建下载临时文件失败")?;
        let mut archive = ZipWriter::new(temporary);
        archive_directory(&file, &mut archive, &archive_prefix(root, relative))?;
        file = archive.finish()?;
        file.seek(SeekFrom::Start(0))?;
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(b"OK\n")?;
    std::io::copy(&mut file, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}
