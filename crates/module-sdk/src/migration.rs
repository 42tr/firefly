//! Explicit, copy-only migration. Never open the host database for writing.
use anyhow::{ensure, Context};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub fn import_database(source: &Path, tables: &[&str]) -> anyhow::Result<()> {
    let target = crate::data_dir()?.join("data.sqlite3");
    import_into(source, &target, tables)
}
fn import_into(source: &Path, target: &Path, tables: &[&str]) -> anyhow::Result<()> {
    let source = source.canonicalize()?;
    ensure!(
        source != target.canonicalize()?,
        "source and destination must differ"
    );
    let mut db = Connection::open_with_flags(
        target,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(30))?;
    let mut url = reqwest::Url::from_file_path(&source)
        .map_err(|_| anyhow::anyhow!("invalid source path"))?;
    url.set_query(Some("mode=ro"));
    db.execute("ATTACH DATABASE ?1 AS legacy", [url.as_str()])?;
    db.execute_batch("PRAGMA foreign_keys=ON")?;
    let tx = db.transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys=ON")?;
    for table in tables {
        ensure!(
            table
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid table"
        );
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM legacy.sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        ensure!(exists, "legacy table {table} is missing");
        let count: i64 = tx.query_row(&format!("SELECT count(*) FROM main.{table}"), [], |r| {
            r.get(0)
        })?;
        ensure!(
            count == 0,
            "destination table {table} is not empty; import aborted without changing data"
        );
        let columns = tx
            .prepare(&format!("PRAGMA main.table_info('{table}')"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        let columns = columns
            .iter()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        tx.execute(
            &format!("INSERT INTO main.{table} ({columns}) SELECT {columns} FROM legacy.{table}"),
            [],
        )
        .with_context(|| format!("import {table}"))?;
    }
    tx.commit()?;
    eprintln!(
        "Imported {} tables; source database unchanged",
        tables.len()
    );
    Ok(())
}
/// File import refuses collisions and symlinks. Source remains untouched.
pub fn copy_files(source: &Path, target: &Path) -> anyhow::Result<()> {
    ensure!(source.is_dir(), "source directory does not exist");
    ensure!(
        !std::fs::symlink_metadata(source)?.file_type().is_symlink(),
        "source must not be a symlink"
    );
    std::fs::create_dir_all(target)?;
    ensure!(
        !std::fs::symlink_metadata(target)?.file_type().is_symlink(),
        "destination must not be a symlink"
    );
    ensure!(
        std::fs::read_dir(target)?.next().is_none(),
        "destination directory is not empty"
    );
    let source = source.canonicalize()?;
    let target = target.canonicalize()?;
    ensure!(
        !target.starts_with(&source),
        "destination must not be inside source"
    );
    let staging = target.with_file_name(format!(".module-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)?;
    fn copy(source: &Path, target: &Path) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let to = target.join(entry.file_name());
            ensure!(!ty.is_symlink(), "symlink in source is not allowed");
            if ty.is_dir() {
                std::fs::create_dir(&to)?;
                copy(&entry.path(), &to)?;
            } else if ty.is_file() {
                std::fs::copy(entry.path(), to)?;
            } else {
                anyhow::bail!("unsupported file type in source");
            }
        }
        Ok(())
    }
    let result = (|| {
        copy(&source, &staging)?;
        std::fs::remove_dir(&target)?; // Refuses if another writer filled it meanwhile.
        std::fs::rename(&staging, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::create_dir(&target);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn file_import_failure_leaves_destination_empty_and_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old");
        let target = dir.path().join("new");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("script.sh"), "echo hello").unwrap();
        std::os::unix::fs::symlink(source.join("script.sh"), source.join("link")).unwrap();
        assert!(copy_files(&source, &target).is_err());
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        std::fs::remove_file(source.join("link")).unwrap();
        copy_files(&source, &target).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("script.sh")).unwrap(),
            "echo hello"
        );
        assert!(source.join("script.sh").exists());
        assert!(copy_files(&source, &target).is_err());
    }
    #[test]
    fn import_is_atomic_preserves_blob_ids_and_never_overwrites_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old.db");
        let target = dir.path().join("new.db");
        let old = Connection::open(&source).unwrap();
        let new = Connection::open(&target).unwrap();
        let schema="CREATE TABLE a(id BLOB PRIMARY KEY, stamp INTEGER, b BLOB REFERENCES b(id)); CREATE TABLE b(id BLOB PRIMARY KEY, a BLOB REFERENCES a(id));";
        old.execute_batch(schema).unwrap();
        new.execute_batch(schema).unwrap();
        old.execute_batch("BEGIN; PRAGMA defer_foreign_keys=ON; INSERT INTO a VALUES(x'1234',1790000000123456,x'5678'); INSERT INTO b VALUES(x'5678',x'1234'); COMMIT;").unwrap();
        // Missing table after an earlier insert rolls back the entire import.
        assert!(import_into(&source, &target, &["a", "missing"]).is_err());
        assert_eq!(
            new.query_row::<i64, _, _>("SELECT count(*) FROM a", [], |r| r.get(0))
                .unwrap(),
            0
        );
        import_into(&source, &target, &["a", "b"]).unwrap();
        assert_eq!(
            new.query_row::<i64, _, _>("SELECT stamp FROM a", [], |r| r.get(0))
                .unwrap(),
            1790000000123456
        );
        assert_eq!(
            new.query_row::<Vec<u8>, _, _>("SELECT id FROM a", [], |r| r.get(0))
                .unwrap(),
            vec![0x12, 0x34]
        );
        assert!(import_into(&source, &target, &["a", "b"]).is_err());
        assert_eq!(
            old.query_row::<i64, _, _>("SELECT count(*) FROM a", [], |r| r.get(0))
                .unwrap(),
            1
        );
    }
}
