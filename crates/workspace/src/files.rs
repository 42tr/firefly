//! Descriptor-anchored safe file access for model-facing tools and workers.
//!
//! Every path is resolved by walking openat from a root directory with
//! O_NOFOLLOW, so symlinks cannot escape the root and renamed ancestors
//! cannot redirect the lookup. Only regular files and directories are
//! exposed; everything else is skipped or rejected.

use std::{
    ffi::{CStr, CString, OsString},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileExt, OpenOptionsExt},
        },
    },
    path::Path,
};

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub fn open_child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> std::io::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // The descriptor anchors lookup even if an ancestor is renamed concurrently.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Resolve `relative` under `root` without following symlinks. Absolute paths,
/// empty segments, `.` and `..` are rejected before any filesystem access.
pub fn open_path(root: &Path, relative: &str) -> anyhow::Result<File> {
    ensure!(
        !relative.starts_with('/')
            && (relative.is_empty()
                || relative
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | ".."))),
        "非法文件路径"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(root)?;
    let parts: Vec<_> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for (index, part) in parts.iter().enumerate() {
        file = open_child(&file, part.as_ref(), index + 1 < parts.len())?;
    }
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir() || metadata.is_file(),
        "仅支持普通文件和目录"
    );
    Ok(file)
}

/// Entry names of an open directory, sorted, excluding `.` and `..`.
pub fn names(directory: &File) -> std::io::Result<Vec<OsString>> {
    // Open a new directory description so enumeration does not share an offset.
    let copy = open_child(directory, std::ffi::OsStr::new("."), true)?;
    use std::os::fd::IntoRawFd;
    let fd = copy.into_raw_fd();
    let handle = unsafe { libc::fdopendir(fd) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let handle = Directory(handle);
    let mut names = Vec::new();
    loop {
        // readdir distinguishes EOF from failure through errno.
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        let entry = unsafe { libc::readdir(handle.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error);
            }
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    names.sort();
    Ok(names)
}

/// Stat a child without following symlinks (fstatat with AT_SYMLINK_NOFOLLOW).
pub fn stat_child(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<libc::stat> {
    let name = CString::new(name.as_bytes())?;
    let mut stat = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

/// A JSON directory listing with directories first, then case-insensitive names.
pub fn listing(root: &Path, relative: &str) -> anyhow::Result<Value> {
    let directory = open_path(root, relative)?;
    ensure!(directory.metadata()?.is_dir(), "该路径不是目录");
    let mut entries = Vec::new();
    for name in names(&directory)? {
        let stat = match stat_child(&directory, &name) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let kind = stat.st_mode & libc::S_IFMT;
        if kind != libc::S_IFDIR && kind != libc::S_IFREG {
            continue;
        }
        let name = name.to_string_lossy();
        entries.push(json!({"name":name,"path":if relative.is_empty() {name.to_string()} else {format!("{relative}/{name}")},
            "is_dir":kind == libc::S_IFDIR,"size":if kind == libc::S_IFDIR {None} else {Some(stat.st_size)},
            "modified_at":stat.st_mtime as f64 + stat.st_mtime_nsec as f64 / 1_000_000_000.0}));
    }
    entries.sort_by_key(|entry| {
        (
            !entry["is_dir"].as_bool().unwrap(),
            entry["name"].as_str().unwrap().to_lowercase(),
        )
    });
    Ok(json!({"path":relative,"entries":entries}))
}

/// Verify an open file is a regular file with a known image magic prefix.
pub fn check_image(file: &File, path: &str) -> anyhow::Result<()> {
    ensure!(file.metadata()?.is_file(), "该路径不是图片文件");
    let mut header = [0; 32];
    let count = file.read_at(&mut header, 0)?;
    let header = &header[..count];
    let extension = Path::new(path)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let valid = match extension.as_str() {
        "png" => header.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => header.starts_with(b"\xff\xd8\xff"),
        "gif" => header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a"),
        "webp" => header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP"),
        "avif" => {
            header.get(4..8) == Some(b"ftyp")
                && matches!(header.get(8..12), Some(b"avif" | b"avis"))
        }
        _ => false,
    };
    ensure!(valid, "临时目录仅支持图片文件");
    Ok(())
}

/// 应用内 HTML 预览的硬上限：8 MiB。
pub const PREVIEW_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Worker 侧的纵深防御：扩展名只由 handler 校验时，单独调用 worker 仍会输出正文。
///
/// 与 handler 的 `validate_preview_query` 保持同一条中文错误文案。
fn ensure_preview_extension(relative: &str) -> anyhow::Result<()> {
    let extension = Path::new(relative)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    ensure!(
        matches!(extension.as_deref(), Some("html" | "htm")),
        "只能预览 .html/.htm 文件"
    );
    Ok(())
}

/// 打开预览文件，并在同一个 fd 上取快照大小。
///
/// 复用 `open_path` 的 O_NOFOLLOW / 路径段校验；只接受普通文件，因此
/// 名字以 `.html` 结尾的目录也不会被当成可预览文件。
pub fn open_preview(root: &Path, relative: &str) -> anyhow::Result<(File, u64)> {
    ensure_preview_extension(relative)?;
    let file = open_path(root, relative)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "该路径不是文件");
    Ok((file, metadata.len()))
}

/// Append a directory tree to an in-progress ZIP archive. Only regular files
/// and directories are included; the walk revalidates each child after the
/// directory listing so entries swapped mid-archive are still type-checked.
pub fn archive_directory(
    directory: &File,
    archive: &mut ZipWriter<File>,
    prefix: &str,
) -> anyhow::Result<()> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    archive.add_directory(format!("{prefix}/"), options)?;
    for name in names(directory)? {
        let kind = stat_child(directory, &name)?.st_mode & libc::S_IFMT;
        if kind != libc::S_IFDIR && kind != libc::S_IFREG {
            continue;
        }
        let mut child = open_child(directory, &name, kind == libc::S_IFDIR)?;
        let metadata = child.metadata()?;
        let path = format!("{prefix}/{}", name.to_string_lossy());
        if metadata.is_dir() {
            archive_directory(&child, archive, &path)?;
        } else if metadata.is_file() {
            archive.start_file(path, options)?;
            std::io::copy(&mut child, archive)?;
        }
    }
    Ok(())
}

/// Default archive prefix for a workspace-relative path.
pub fn archive_prefix(root: &Path, relative: &str) -> String {
    Path::new(if relative.is_empty() {
        root.as_os_str()
    } else {
        relative.as_ref()
    })
    .file_name()
    .unwrap_or_else(|| "workspace".as_ref())
    .to_string_lossy()
    .into_owned()
}


/// 预览协议：`OK <字节数>\n` 一行元数据，随后是原始字节。
///
/// 超限时只写头部、不读正文，让 handler 能在提交响应前返回 413；
/// `file.take(size)` 以打开时的快照为准，文件增长多出的部分不会被发送。
pub fn write_preview(writer: &mut impl Write, file: File, size: u64) -> anyhow::Result<()> {
    writeln!(writer, "OK {size}")?;
    writer.flush()?;
    if size <= PREVIEW_MAX_BYTES {
        std::io::copy(&mut file.take(size), writer)?;
        writer.flush()?;
    }
    Ok(())
}

/// Workers keep downloads streaming and ZIP preparation isolated from the server.
pub fn run(root: &Path, relative: &str, operation: &str) -> anyhow::Result<()> {
    if operation == "list" {
        println!("{}", listing(root, relative)?);
        return Ok(());
    }
    if operation == "preview" {
        let (file, size) = open_preview(root, relative)?;
        let mut stdout = std::io::stdout().lock();
        write_preview(&mut stdout, file, size)?;
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
    stdout.write_all(b"OK
")?;
    std::io::copy(&mut file, &mut stdout)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_path_rejects_traversal_and_follows_no_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("file.txt"), b"data").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("file.txt"), root.join("link.txt")).unwrap();

        assert!(open_path(root, "file.txt").is_ok());
        // The root itself is addressable as the empty relative path.
        assert!(open_path(root, "").unwrap().metadata().unwrap().is_dir());
        for bad in ["/", "..", "a/../b", "a//b", "./a"] {
            assert!(open_path(root, bad).is_err(), "accepted {bad}");
        }
        // The symlink itself is refused by O_NOFOLLOW during the walk.
        assert!(open_path(root, "link.txt").is_err());
    }

    #[test]
    fn listing_reports_directories_first_with_sizes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("z.txt"), b"12345").unwrap();
        let listed = listing(root, "").unwrap();
        let entries = listed["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "sub");
        assert!(entries[0]["is_dir"].as_bool().unwrap());
        assert_eq!(entries[1]["size"], 5);
    }

    #[test]
    fn preview_only_opens_regular_files_inside_the_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("index.html"), b"<p>hi</p>").unwrap();
        std::os::unix::fs::symlink(root.join("index.html"), root.join("link.html")).unwrap();

        let (_file, size) = open_preview(root, "index.html").unwrap();
        assert_eq!(size, b"<p>hi</p>".len() as u64);
        // 目录、缺失文件与符号链接都不进入预览。
        assert!(open_preview(root, "").is_err());
        assert!(open_preview(root, "missing.html").is_err());
        assert!(open_preview(root, "link.html").is_err());
    }

    #[test]
    fn preview_rejects_non_html_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        // worker 直调也必须拒绝：内容合法、名字不是 .html/.htm。
        for name in ["noext.txt", "a.html.txt", "noext", "page.htmlx"] {
            std::fs::write(root.join(name), b"<p>hi</p>").unwrap();
            let error = open_preview(root, name).unwrap_err().to_string();
            assert_eq!(error, "只能预览 .html/.htm 文件", "accepted {name}");
        }
        for name in ["INDEX.HTML", "x.htm"] {
            std::fs::write(root.join(name), b"<!doctype html>").unwrap();
            let handle = std::fs::OpenOptions::new()
                .write(true)
                .open(root.join(name))
                .unwrap();
            // 用 set_len 构造大小，不实际写入内容。
            handle.set_len(16).unwrap();
            drop(handle);
            let (_file, size) = open_preview(root, name).unwrap();
            assert_eq!(size, 16);
        }
    }

    #[test]
    fn preview_protocol_streams_sized_header_then_body() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("index.html"), b"<p>hi</p>").unwrap();
        let (file, size) = open_preview(root, "index.html").unwrap();
        let mut output = Vec::new();
        write_preview(&mut output, file, size).unwrap();
        assert_eq!(output, b"OK 9\n<p>hi</p>");
    }

    #[test]
    fn preview_limit_accepts_exactly_eight_mib_and_rejects_one_byte_more() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("exact.html"), b"<!doctype html>").unwrap();
        let exact = std::fs::OpenOptions::new().write(true).open(root.join("exact.html")).unwrap();
        // 用 set_len 构造边界大小，不实际分配 8 MiB 内容。
        exact.set_len(PREVIEW_MAX_BYTES).unwrap();
        drop(exact);
        let (file, size) = open_preview(root, "exact.html").unwrap();
        assert_eq!(size, PREVIEW_MAX_BYTES);
        let mut output = Vec::new();
        write_preview(&mut output, file, size).unwrap();
        assert_eq!(output.len() as u64, format!("OK {PREVIEW_MAX_BYTES}\n").len() as u64 + PREVIEW_MAX_BYTES);

        std::fs::write(root.join("over.html"), b"<!doctype html>").unwrap();
        let over = std::fs::OpenOptions::new().write(true).open(root.join("over.html")).unwrap();
        over.set_len(PREVIEW_MAX_BYTES + 1).unwrap();
        drop(over);
        let (file, size) = open_preview(root, "over.html").unwrap();
        assert_eq!(size, PREVIEW_MAX_BYTES + 1);
        let mut output = Vec::new();
        write_preview(&mut output, file, size).unwrap();
        // 超限时只有头部，正文一个字节都不发送。
        assert_eq!(output, format!("OK {}\n", PREVIEW_MAX_BYTES + 1).into_bytes());
    }

    #[test]
    fn image_magic_prefixes_are_enforced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nimage data").unwrap();
        let file = File::open(&path).unwrap();
        assert!(check_image(&file, "image.png").is_ok());
        assert!(check_image(&file, "image.jpg").is_err());
    }
}
