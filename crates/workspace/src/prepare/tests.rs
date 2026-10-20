use super::*;

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> (tempfile::TempDir, PathBuf, Workspace) {
    let temporary = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temporary.path()).unwrap().join("workspace");
    let source = root.join("project");
    fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "Test"]);
    fs::write(source.join("tracked"), "committed").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "base"]);
    let workspace = Workspace::new(
        json!({"root":root,"source":"project","workspace_id":uuid::Uuid::new_v4(),
        "attempt":uuid::Uuid::new_v4(),"conversation_id":42}),
    )
    .unwrap();
    (temporary, source, workspace)
}

#[test]
fn disappearing_build_files_are_tolerated_and_open_descriptors_preserve_metadata() {
    for phase in ["scan_stat", "copy_stat", "open", "after_open"] {
        let (_temporary, source, mut workspace) = fixture();
        let artifact = source.join("build.o");
        fs::write(&artifact, b"build output").unwrap();
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o755)).unwrap();
        let time = UNIX_EPOCH + Duration::from_secs(1234567890);
        File::open(&artifact)
            .unwrap()
            .set_times(FileTimes::new().set_modified(time))
            .unwrap();
        let target = artifact.clone();
        workspace.hook = Some(Box::new(move |path, step| {
            if path == target && step == phase && target.exists() {
                fs::remove_file(&target).unwrap();
            }
        }));
        workspace.prepare().unwrap();
        let copied = workspace.final_path.join("work/build.o");
        if phase == "after_open" {
            assert_eq!(fs::read(&copied).unwrap(), b"build output");
            let metadata = fs::metadata(&copied).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o755);
            assert_eq!(metadata.modified().unwrap(), time);
        } else {
            assert!(!copied.exists(), "{phase}");
        }
        assert_eq!(
            fs::read_to_string(workspace.final_path.join("work/tracked")).unwrap(),
            "committed"
        );
    }
}

#[test]
fn disappearing_directories_are_tolerated_in_scan_copy_and_metadata() {
    for phase in ["scan", "enumerate", "metadata"] {
        let (_temporary, source, mut workspace) = fixture();
        let directory = source.join("build");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("output"), "output").unwrap();
        let target = directory.clone();
        workspace.hook = Some(Box::new(move |path, step| {
            if path == target && step == phase && target.exists() {
                fs::remove_dir_all(&target).unwrap();
            }
        }));
        workspace.prepare().unwrap();
        let copied = workspace.final_path.join("work/build");
        if phase == "metadata" {
            assert_eq!(fs::read_to_string(copied.join("output")).unwrap(), "output");
        } else {
            assert!(!copied.exists(), "{phase}");
        }
    }
}

#[test]
fn deleted_tracked_files_are_restored_without_restoring_the_source() {
    let (_temporary, source, mut workspace) = fixture();
    let tracked = source.join("tracked");
    let target = tracked.clone();
    workspace.hook = Some(Box::new(move |path, step| {
        if path == target && step == "copy_stat" {
            fs::remove_file(&target).unwrap();
        }
    }));
    workspace.prepare().unwrap();
    assert!(!tracked.exists());
    assert_eq!(
        fs::read_to_string(workspace.final_path.join("work/tracked")).unwrap(),
        "committed"
    );
}

#[test]
fn changed_head_and_copy_errors_never_publish_and_clean_staging() {
    for failure in ["head", "destination", "root", "git"] {
        let (_temporary, source, mut workspace) = fixture();
        let stage = workspace.stage.clone();
        workspace.hook = Some(Box::new(move |path, step| {
            if failure == "head" && path == source && step == "metadata" {
                git(
                    &source,
                    &["commit", "--allow-empty", "-qm", "concurrent commit"],
                );
            }
            if failure == "destination" && path == source.join("tracked") && step == "open" {
                fs::remove_dir_all(stage.join("work")).unwrap();
            }
            if failure == "root" && path == source && step == "enumerate" {
                fs::remove_dir_all(&source).unwrap();
            }
            if failure == "git" && path == source.join(".git/config") && step == "copy_stat" {
                fs::remove_file(path).unwrap();
            }
        }));
        let error = workspace.prepare().unwrap_err();
        if failure == "head" {
            assert!(error.to_string().contains("发生变化"), "{error:#}");
        }
        if failure == "root" || failure == "git" {
            assert!(error.to_string().contains("消失"), "{error:#}");
        }
        assert!(!workspace.final_path.exists(), "{failure}");
        assert!(!workspace.stage.exists(), "{failure}");
    }
}

#[test]
fn external_symlink_replacing_file_before_open_does_not_publish() {
    let (_temporary, source, mut workspace) = fixture();
    let outside = source.parent().unwrap().join("secret");
    fs::write(&outside, b"secret").unwrap();
    workspace.hook = Some(Box::new(move |path, phase| {
        if path == source.join("tracked") && phase == "open" {
            fs::remove_file(path).unwrap();
            std::os::unix::fs::symlink(&outside, path).unwrap();
        }
    }));
    let error = workspace.prepare().unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .raw_os_error(),
        Some(libc::ELOOP)
    );
    assert!(!workspace.final_path.exists());
    assert!(!workspace.stage.exists());
}

#[test]
fn cancellation_stops_git_and_cleans_staging() {
    let (_temporary, source, mut workspace) = fixture();
    let cancel = workspace.cancel.clone();
    workspace.hook = Some(Box::new(move |path, phase| {
        if path == source.join("tracked") && phase == "after_open" {
            fs::write(&cancel, "").unwrap();
        }
    }));
    assert!(workspace.run().unwrap_err().to_string().contains("已取消"));
    assert!(!workspace.stage.exists());
    assert!(!workspace.final_path.exists());
    assert!(!workspace.cancel.exists());
}

#[test]
fn incomplete_metadata_permissions_and_other_io_errors_are_not_ignored() {
    let (_temporary, source, workspace) = fixture();
    for path in [
        source.clone(),
        source.join(".git/config"),
        source.join(".git/missing"),
    ] {
        let error = workspace
            .source_read::<()>(&path, &source, || Err(std::io::ErrorKind::NotFound.into()))
            .unwrap_err();
        assert!(error.to_string().contains("消失"));
    }
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::StorageFull,
    ] {
        let error = workspace
            .source_read::<()>(&source.join("ordinary"), &source, || Err(kind.into()))
            .unwrap_err();
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), kind);
    }
}

#[test]
fn relative_links_survive_publication_including_dangling_targets() {
    let (_temporary, source, workspace) = fixture();
    std::os::unix::fs::symlink(source.join("missing/target"), source.join("link")).unwrap();
    workspace.prepare().unwrap();
    assert_eq!(
        fs::read_link(workspace.final_path.join("work/link")).unwrap(),
        Path::new("missing/target")
    );
}
