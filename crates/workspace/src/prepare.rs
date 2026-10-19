//! Persistent parallel workspaces. Runs in a native child process with flock
//! ownership, atomic publication and cancellation flags that survive restarts.
use std::{
    fs::{self, File, FileTimes, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::files;
use anyhow::{bail, ensure, Context};
use serde_json::{json, Value};

const MANAGED: &str = ".assistant-parallel";

struct GitProcess(std::process::Child);

impl Drop for GitProcess {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            unsafe {
                libc::killpg(self.0.id() as i32, libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
}

fn emit(event: Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{event}")?;
    stdout.flush()?;
    Ok(())
}

fn real_directory(path: &Path) -> anyhow::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.is_symlink(),
        "目录不存在或为符号链接：{}",
        path.display()
    );
    Ok(fs::canonicalize(path)?)
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    real_directory(path)?;
    Ok(())
}

/// Canonicalize existing ancestors, also supporting internal dangling symlinks.
fn resolved(path: &Path) -> anyhow::Result<PathBuf> {
    fn resolve(path: &Path, depth: usize) -> anyhow::Result<PathBuf> {
        ensure!(depth < 40, "符号链接过多：{}", path.display());
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir => result.push("/"),
                Component::CurDir => {}
                Component::ParentDir => {
                    result.pop();
                }
                Component::Normal(name) => {
                    result.push(name);
                    match fs::read_link(&result) {
                        Ok(target) => {
                            let link = if target.is_absolute() {
                                target
                            } else {
                                result.parent().unwrap().join(target)
                            };
                            result = resolve(&link, depth + 1)?;
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                            ) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                _ => bail!("非法来源路径"),
            }
        }
        Ok(result)
    }
    resolve(path, 0)
}

fn relative_link(target: &Path, parent: &Path) -> PathBuf {
    let target: Vec<_> = target.components().collect();
    let parent: Vec<_> = parent.components().collect();
    let common = target
        .iter()
        .zip(&parent)
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in common..parent.len() {
        result.push("..");
    }
    for component in &target[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
type PreparationHook = Box<dyn Fn(&Path, &str)>;

struct Workspace {
    request: Value,
    root: PathBuf,
    id: String,
    base: PathBuf,
    final_path: PathBuf,
    stage: PathBuf,
    cancel: PathBuf,
    #[cfg(test)]
    hook: Option<PreparationHook>,
}

impl Workspace {
    fn new(request: Value) -> anyhow::Result<Self> {
        let root = real_directory(Path::new(
            request["root"].as_str().context("缺少 workspace 根目录")?,
        ))?;
        let id = uuid::Uuid::parse_str(request["workspace_id"].as_str().context("缺少副本标识")?)?
            .to_string();
        let attempt = uuid::Uuid::parse_str(request["attempt"].as_str().context("缺少准备标识")?)?
            .to_string();
        ensure!(request["conversation_id"].is_i64(), "缺少会话标识");
        let base = root.join(MANAGED);
        private_directory(&base)?;
        for name in [".staging", ".locks", ".cancel"] {
            private_directory(&base.join(name))?;
        }
        Ok(Self {
            final_path: base.join(&id),
            stage: base.join(".staging").join(&id),
            cancel: base.join(".cancel").join(format!("{id}-{attempt}")),
            request,
            root,
            id,
            base,
            #[cfg(test)]
            hook: None,
        })
    }

    fn check(&self) -> anyhow::Result<()> {
        ensure!(!self.cancel.try_exists()?, "工作目录准备已取消");
        Ok(())
    }

    fn lock_file(&self, name: &str) -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.base.join(".locks").join(name))?;
        ensure!(file.metadata()?.is_file(), "副本锁不是普通文件");
        Ok(file)
    }

    fn try_lock(file: &File) -> std::io::Result<bool> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }

    fn lock(&self, wait: bool) -> anyhow::Result<File> {
        let file = self.lock_file(&self.id)?;
        loop {
            self.check()?;
            if Self::try_lock(&file)? {
                return Ok(file);
            }
            ensure!(wait, "副本仍有准备任务，请稍后重试");
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn slot(&self) -> anyhow::Result<File> {
        let files = [self.lock_file("slot-0")?, self.lock_file("slot-1")?];
        loop {
            self.check()?;
            for file in &files {
                if Self::try_lock(file)? {
                    return Ok(file.try_clone()?);
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn git(
        &self,
        path: &Path,
        args: &[&str],
        allow_failure: bool,
        locks: &[&File],
    ) -> anyhow::Result<Option<String>> {
        self.check()?;
        let mut command = Command::new("git");
        command
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "submodule.recurse=false",
                "-C",
            ])
            .arg(path)
            .args(args)
            .stdin(Stdio::null());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        command
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        let descriptors: Vec<_> = locks.iter().map(|file| file.as_raw_fd()).collect();
        // Git inherits the locks if this worker is killed during checkout. A
        // recovering worker must wait for Git before deleting the staging tree.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for fd in &descriptors {
                    let flags = libc::fcntl(*fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = GitProcess(
            command
                .spawn()
                .context("无法启动 Git，请确认主机已安装 git")?,
        );
        let status = loop {
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            self.check()?;
            thread::sleep(Duration::from_millis(25));
        };
        if !status.success() {
            if allow_failure {
                return Ok(None);
            }
            stderr.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            stderr.take(2048).read_to_end(&mut bytes)?;
            bail!(
                "Git 操作失败（{}）：{}",
                path.display(),
                String::from_utf8_lossy(&bytes).trim()
            );
        }
        stdout.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes)?;
        // Preserve NUL-delimited ls-tree records; only trim conventional newlines.
        Ok(Some(
            String::from_utf8_lossy(&bytes)
                .trim_end_matches(['\n', '\r'])
                .to_owned(),
        ))
    }

    fn required_git(&self, path: &Path, args: &[&str], locks: &[&File]) -> anyhow::Result<String> {
        self.git(path, args, false, locks)?
            .context("Git 没有返回结果")
    }

    fn validate_repo(&self, path: &Path, locks: &[&File]) -> anyhow::Result<Value> {
        let gitdir = path.join(".git");
        real_directory(&gitdir).context("暂不支持 worktree、子模块或分离 Git 目录")?;
        for name in [
            "commondir",
            "objects/info/alternates",
            "objects/info/http-alternates",
            "worktrees",
            "modules",
            "config.worktree",
        ] {
            ensure!(
                !gitdir.join(name).try_exists()?,
                "暂不支持共享 Git 元数据或子模块：{}",
                path.display()
            );
        }
        for name in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "BISECT_START",
        ] {
            ensure!(
                !gitdir.join(name).try_exists()?,
                "请先结束仓库中的 merge/rebase 等操作：{}",
                path.display()
            );
        }
        let config = self.required_git(path, &["config", "--local", "--list"], locks)?;
        for line in config.lines() {
            let key = line.split('=').next().unwrap_or_default().to_lowercase();
            ensure!(
                !(matches!(key.as_str(), "core.worktree" | "core.bare")
                    && line.to_lowercase() != "core.bare=false"),
                "暂不支持自定义 Git 工作树"
            );
            ensure!(
                !["include.", "includeif.", "filter."]
                    .iter()
                    .any(|prefix| key.starts_with(prefix)),
                "暂不支持 Git include/filter 配置"
            );
        }
        self.check_git_metadata(&gitdir)?;
        let top = self.required_git(path, &["rev-parse", "--show-toplevel"], locks)?;
        let actual = self.required_git(path, &["rev-parse", "--absolute-git-dir"], locks)?;
        ensure!(
            fs::canonicalize(top)? == fs::canonicalize(path)?
                && fs::canonicalize(actual)? == fs::canonicalize(&gitdir)?,
            "Git 工作树或元数据越出副本"
        );
        let head = self
            .git(
                path,
                &["rev-parse", "--verify", "HEAD^{commit}"],
                true,
                locks,
            )?
            .context("仓库没有有效提交，请先创建初始提交")?;
        for entry in self
            .required_git(path, &["ls-tree", "-rz", &head], locks)?
            .split('\0')
        {
            ensure!(!entry.starts_with("160000 "), "暂不支持包含子模块的仓库");
            if entry.starts_with("120000 ") {
                let object = entry
                    .split('\t')
                    .next()
                    .unwrap_or_default()
                    .split(' ')
                    .nth(2)
                    .context("Git 符号链接记录无效")?;
                ensure!(
                    !Path::new(&self.required_git(path, &["cat-file", "blob", object], locks)?)
                        .is_absolute(),
                    "暂不支持 Git 已跟踪的绝对符号链接"
                );
            }
        }
        Ok(json!({"commit":head,"branch":self.git(path,&["symbolic-ref","-q","HEAD"],true,locks)?}))
    }

    fn check_git_metadata(&self, path: &Path) -> anyhow::Result<()> {
        self.check()?;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let info = fs::symlink_metadata(entry.path())?;
            ensure!(
                !info.is_symlink(),
                "Git 元数据含符号链接：{}",
                entry.path().display()
            );
            ensure!(
                !entry.file_name().to_string_lossy().ends_with(".lock"),
                "Git 仓库正在写入，请稍后重试"
            );
            if info.is_dir() {
                self.check_git_metadata(&entry.path())?;
            }
        }
        Ok(())
    }

    fn source_read<T>(
        &self,
        path: &Path,
        root: &Path,
        read: impl FnOnce() -> std::io::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        match read() {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    path != root
                        && !path
                            .strip_prefix(root)?
                            .components()
                            .any(|part| part.as_os_str() == ".git"),
                    "来源目录或 Git 元数据在复制期间消失，请重试：{}",
                    path.display()
                );
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn scan(&self, source: &Path, locks: &[&File]) -> anyhow::Result<Vec<Value>> {
        let device = fs::metadata(source)?.dev();
        let mut repos = Vec::new();
        self.scan_directory(source, source, device, locks, &mut repos)?;
        Ok(repos)
    }

    fn scan_directory(
        &self,
        path: &Path,
        root: &Path,
        device: u64,
        locks: &[&File],
        repos: &mut Vec<Value>,
    ) -> anyhow::Result<()> {
        self.check()?;
        self.test_hook(path, "scan");
        let Some(directory) = self.source_read(path, root, || {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(path)
        })?
        else {
            return Ok(());
        };
        let names = files::names(&directory)?;
        let repo = names.iter().any(|name| name == ".git");
        if repo {
            let mut meta = self.validate_repo(path, locks)?;
            let relative = path.strip_prefix(root)?;
            meta["path"] = json!(if relative.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                relative.to_string_lossy().into_owned()
            });
            repos.push(meta);
        } else {
            ensure!(
                !(names.iter().any(|n| n == "HEAD")
                    && names.iter().any(|n| n == "config")
                    && names.iter().any(|n| n == "objects")
                    && names.iter().any(|n| n == "refs")),
                "暂不支持裸仓库"
            );
        }
        for name in names {
            if (path == self.root && name == MANAGED) || (repo && name == ".git") {
                continue;
            }
            let child = path.join(&name);
            self.test_hook(&child, "scan_stat");
            let Some(info) = self.source_read(&child, root, || fs::symlink_metadata(&child))?
            else {
                continue;
            };
            if info.is_symlink() {
                ensure!(
                    resolved(&child)?.starts_with(root),
                    "符号链接指向复制范围外：{}",
                    child.display()
                );
            } else {
                ensure!(
                    info.is_dir() || info.is_file(),
                    "暂不支持复制特殊文件：{}",
                    child.display()
                );
                ensure!(
                    info.dev() == device
                        && !(info.is_dir() && info.ino() == fs::metadata(path)?.ino()),
                    "暂不支持跨挂载点复制"
                );
                if info.is_dir() {
                    self.scan_directory(&child, root, device, locks, repos)?;
                }
            }
        }
        Ok(())
    }

    fn copy_metadata(destination: &Path, info: &Metadata) -> anyhow::Result<()> {
        // Use the opened source's metadata even if a build removes its pathname.
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(info.mode() & 0o7777),
        )?;
        let file = File::open(destination)?;
        file.set_times(
            FileTimes::new()
                .set_accessed(info.accessed()?)
                .set_modified(info.modified()?),
        )?;
        Ok(())
    }

    fn copy(&self, source: &Path, destination: &Path, root: &Path) -> anyhow::Result<()> {
        self.check()?;
        ensure!(
            source == root
                || resolved(source.parent().context("来源没有父目录")?)?.starts_with(root),
            "来源路径在复制期间越出范围"
        );
        self.test_hook(source, "copy_stat");
        let Some(mut info) = self.source_read(source, root, || fs::symlink_metadata(source))?
        else {
            return Ok(());
        };
        if info.is_symlink() {
            let target = resolved(source)?;
            ensure!(
                target.starts_with(root),
                "符号链接指向复制范围外：{}",
                source.display()
            );
            let mapped = self.stage.join("work").join(target.strip_prefix(root)?);
            std::os::unix::fs::symlink(
                relative_link(&mapped, destination.parent().context("副本没有父目录")?),
                destination,
            )?;
        } else if info.is_dir() {
            ensure!(
                source == root || info.dev() == fs::metadata(root)?.dev(),
                "暂不支持跨挂载点复制"
            );
            self.test_hook(source, "enumerate");
            let Some(directory) = self.source_read(source, root, || {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(source)
            })?
            else {
                return Ok(());
            };
            let children = files::names(&directory)?;
            private_directory(destination)?;
            for name in children {
                if source == self.root && name == MANAGED {
                    continue;
                }
                self.copy(&source.join(&name), &destination.join(&name), root)?;
            }
            self.test_hook(source, "metadata");
            Self::copy_metadata(destination, &info)?;
        } else if info.is_file() {
            self.test_hook(source, "open");
            let Some(mut input) = self.source_read(source, root, || {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(source)
            })?
            else {
                return Ok(());
            };
            info = input.metadata()?;
            ensure!(
                info.is_file(),
                "文件类型在复制期间变化：{}",
                source.display()
            );
            self.test_hook(source, "after_open");
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(destination)?;
            let mut buffer = vec![0; 1024 * 1024];
            loop {
                self.check()?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
            }
            Self::copy_metadata(destination, &info)?;
        } else {
            bail!("暂不支持复制特殊文件：{}", source.display());
        }
        Ok(())
    }

    fn test_hook(&self, _path: &Path, _phase: &str) {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(_path, _phase);
        }
    }

    fn manifest(&self) -> anyhow::Result<Value> {
        real_directory(&self.final_path)?;
        let path = self.final_path.join("manifest.json");
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        ensure!(file.metadata()?.is_file(), "副本完成标记无效");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let data: Value = serde_json::from_slice(&bytes)?;
        for key in ["workspace_id", "conversation_id"] {
            ensure!(data[key] == self.request[key], "副本归属与会话不一致");
        }
        ensure!(data["state"] == "ready", "副本尚未完成初始化");
        real_directory(&self.final_path.join("work"))?;
        Ok(data)
    }

    fn prepare(&self) -> anyhow::Result<Value> {
        let lock = self.lock(true)?;
        let slot = self.slot()?;
        let locks = [&lock, &slot];
        self.check()?;
        if fs::symlink_metadata(&self.final_path).is_ok() {
            return self.manifest();
        }
        ensure!(
            self.request["ready"] != true,
            "副本目录已丢失，不能自动重建；请新建并行会话"
        );
        let relative = Path::new(self.request["source"].as_str().context("缺少来源目录")?);
        ensure!(
            !relative.is_absolute() && !relative.components().any(|c| c == Component::ParentDir),
            "来源必须是 workspace 内的相对路径"
        );
        let source = fs::canonicalize(self.root.join(relative))?;
        ensure!(
            source.starts_with(&self.root) && source.is_dir(),
            "来源目录越出 workspace 或不存在"
        );
        if source.starts_with(&self.base) {
            let parts: Vec<_> = source.strip_prefix(&self.base)?.components().collect();
            ensure!(
                parts.len() == 2 && parts[1].as_os_str() == "work",
                "不能复制副本管理目录"
            );
            uuid::Uuid::parse_str(&parts[0].as_os_str().to_string_lossy())?;
            ensure!(
                source != self.final_path.join("work"),
                "来源不能是当前会话自身"
            );
            let manifest: Value =
                serde_json::from_slice(&fs::read(source.parent().unwrap().join("manifest.json"))?)?;
            ensure!(manifest["state"] == "ready", "来源副本尚未就绪");
        }
        if let Some(top) = self.git(&source, &["rev-parse", "--show-toplevel"], true, &locks)? {
            ensure!(
                fs::canonicalize(top)? == source,
                "所选目录位于 Git 仓库内部，请选择仓库根目录"
            );
        }
        emit(json!({"stage":"checking","message":"检查目录与 Git 仓库"}))?;
        let repos = self.scan(&source, &locks)?;
        if let Ok(info) = fs::symlink_metadata(&self.stage) {
            ensure!(!info.is_symlink(), "临时目录不能为符号链接");
            fs::remove_dir_all(&self.stage)?;
        }
        private_directory(&self.stage)?;
        let result = (|| {
            emit(json!({"stage":"copying","message":"复制工作目录"}))?;
            self.copy(&source, &self.stage.join("work"), &source)?;
            ensure!(
                self.scan(&source, &locks)? == repos,
                "来源仓库在复制期间发生变化，请重试"
            );
            for (index, repo) in repos.iter().enumerate() {
                let target = self
                    .stage
                    .join("work")
                    .join(repo["path"].as_str().context("仓库路径无效")?);
                self.validate_repo(&target, &locks)?;
                self.required_git(
                    &target,
                    &["fsck", "--connectivity-only", "--no-dangling"],
                    &locks,
                )?;
                emit(
                    json!({"stage":"resetting","message":format!("重置 Git 仓库 {}/{}",index + 1,repos.len())}),
                )?;
                self.required_git(
                    &target,
                    &[
                        "reset",
                        "--hard",
                        repo["commit"].as_str().context("仓库提交无效")?,
                    ],
                    &locks,
                )?;
                ensure!(
                    self.required_git(&target, &["diff", "--name-only", "HEAD"], &locks)?
                        .is_empty(),
                    "副本未正确还原"
                );
            }
            self.scan(&self.stage.join("work"), &locks)?;
            self.check()?;
            let data = json!({"workspace_id":self.request["workspace_id"],"conversation_id":self.request["conversation_id"],
                "state":"ready","source":self.request["source"],"repositories":repos,
                "prepared_at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64()});
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(self.stage.join("manifest.json"))?;
            serde_json::to_writer(&mut file, &data)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&self.stage, &self.final_path)?;
            Ok(data)
        })();
        if let Ok(info) = fs::symlink_metadata(&self.stage) {
            if info.is_dir() && !info.is_symlink() {
                fs::remove_dir_all(&self.stage).context("清理未完成副本失败")?;
            }
        }
        result
    }

    fn run(&self) -> anyhow::Result<Value> {
        match self.request["op"].as_str().unwrap_or("prepare") {
            "cancel" => {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&self.cancel)?;
                Ok(json!({}))
            }
            "check" => self.manifest(),
            "remove" => {
                let _lock = self.lock(false)?;
                self.manifest()?;
                fs::remove_dir_all(&self.final_path)?;
                Ok(json!({}))
            }
            "prepare" => {
                let result = self.prepare();
                let _ = fs::remove_file(&self.cancel);
                result
            }
            _ => bail!("未知工作目录操作"),
        }
    }
}

pub fn run(payload: &str) -> anyhow::Result<()> {
    let result = (|| Workspace::new(serde_json::from_str(payload)?)?.run())();
    match result {
        Ok(metadata) => emit(json!({"stage":"ready","metadata":metadata})),
        Err(error) => {
            emit(
                json!({"stage":"error","message":format!("{error:#}").chars().take(2000).collect::<String>()}),
            )?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
