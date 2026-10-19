//! Verified online backups use the same bundled SQLite as the application.
use std::{fs::OpenOptions, path::Path, time::Duration};

use anyhow::{ensure, Context};
use rusqlite::{backup::Backup, Connection, OpenFlags, OptionalExtension};

pub fn backup(database: &Path, target: &Path) -> anyhow::Result<()> {
    ensure!(
        rusqlite::version_number() >= 3_051_003,
        "SQLite >=3.51.3 required; rebuild with cargo build --locked"
    );
    let source = database
        .canonicalize()
        .context("Backup source does not exist")?;
    let src = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    src.busy_timeout(Duration::from_secs(30))?;
    // Validate before reserving a target; never create a missing source database.
    src.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(
        options
            .open(target)
            .context("Backup target must be a new file")?,
    );
    let result = (|| -> anyhow::Result<()> {
        let mut dst = Connection::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        dst.busy_timeout(Duration::from_secs(30))?;
        Backup::new(&src, &mut dst)?.run_to_completion(1024, Duration::from_millis(100), None)?;
        let integrity = dst
            .prepare("PRAGMA integrity_check")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ensure!(integrity == ["ok"], "Backup integrity check failed");
        ensure!(
            dst.query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional()?
                .is_none(),
            "Backup foreign key check failed"
        );
        let mode: String = dst.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
        ensure!(mode == "delete", "Backup must be a standalone database");
        dst.close().map_err(|(_, error)| error)?;
        Ok(())
    })();
    if result.is_err() {
        // Only remove the new, incomplete file reserved above, after closing it.
        std::fs::remove_file(target).context("Failed to remove incomplete backup")?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_wal_data_is_backed_up_as_a_standalone_database() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.sqlite3");
        let target = dir.path().join("backup.sqlite3");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('committed in WAL')").unwrap();
        assert!(source.with_extension("sqlite3-wal").exists());

        backup(&source, &target).unwrap();
        let saved = Connection::open(&target).unwrap();
        assert_eq!(saved.query_row("SELECT value FROM records", [], |row| row.get::<_, String>(0)).unwrap(), "committed in WAL");
        assert_eq!(saved.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)).unwrap(), "delete");
        assert_eq!(saved.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn foreign_key_failure_removes_incomplete_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.sqlite3");
        let target = dir.path().join("backup.sqlite3");
        let db = Connection::open(&source).unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=OFF; CREATE TABLE parent(id INTEGER PRIMARY KEY);
            CREATE TABLE child(parent_id REFERENCES parent(id)); INSERT INTO child VALUES (1);",
        )
        .unwrap();
        assert!(backup(&source, &target)
            .unwrap_err()
            .to_string()
            .contains("foreign key"));
        assert!(!target.exists());
        assert_eq!(
            db.query_row("SELECT parent_id FROM child", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn missing_source_and_existing_symlink_target_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source #中文.sqlite3");
        let target = dir.path().join("backup.sqlite3");
        assert!(backup(&source, &target).is_err());
        assert!(!source.exists());
        assert!(!target.exists());
        let db = Connection::open(&source).unwrap();
        db.execute_batch("CREATE TABLE records(id INTEGER); INSERT INTO records VALUES (1)")
            .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source, &target).unwrap();
            assert!(backup(&source, &target).is_err());
            assert!(target.is_symlink());
            assert_eq!(
                db.query_row("SELECT id FROM records", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
}
