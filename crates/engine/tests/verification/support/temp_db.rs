//! Shared ownership of temporary SQLite files for properties and fuzz histories.
pub(crate) struct TempDb(pub(crate) String);
impl TempDb {
    pub(crate) fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("engine-verification-{}.db", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .into_owned(),
        )
    }
}
impl std::ops::Deref for TempDb {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", ".ready"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::TempDb;
    #[test]
    fn owns_unique_paths_and_removes_database_sidecars_and_crash_markers() {
        let db = TempDb::new();
        let other = TempDb::new();
        assert_ne!(
            db.0, other.0,
            "temporary database owners must be independent"
        );
        let names = ["", "-wal", "-shm", ".ready"].map(|suffix| format!("{}{suffix}", db.0));
        for name in &names {
            std::fs::write(name, b"fixture").unwrap();
        }
        drop(db);
        for name in names {
            assert!(
                !std::path::Path::new(&name).exists(),
                "temporary file survived owner drop: {name}"
            );
        }
    }
}
