use dictionary::{
    settings::{Appearance, Preferences},
    store::Database,
};
use rusqlite::Connection;

#[test]
fn a_failed_later_migration_rolls_back_the_whole_upgrade_and_keeps_a_wal_aware_backup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("history.sqlite3");
    let original = Connection::open(&path).unwrap();
    original
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    original
        .execute_batch(include_str!("../migrations/001_initial.sql"))
        .unwrap();
    original.execute_batch("INSERT INTO lookups VALUES ('old', '保留的输入', '保留的输入', 'zh_en', '2026-01-01T00:00:00Z'); CREATE INDEX attempts_history_status ON attempts(effort);").unwrap();
    // Keep the source open so its committed data still lives in the WAL.
    let error = Database::open(&path)
        .err()
        .expect("conflicting index must fail the upgrade");
    let backup = std::fs::read_dir(root.path())
        .unwrap()
        .flatten()
        .map(|f| f.path())
        .find(|p| p.extension().is_some_and(|e| e == "sqlite3") && p != &path)
        .unwrap();
    assert!(format!("{error:#}").contains(backup.to_str().unwrap()));
    for db in [&original, &Connection::open(&backup).unwrap()] {
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT input FROM lookups WHERE id='old'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "保留的输入"
        );
        assert!(
            db.prepare("SELECT cache_key FROM lookups").is_err(),
            "earlier migration must also roll back"
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
    original
        .execute_batch("DROP INDEX attempts_history_status")
        .unwrap();
    drop(original);
    drop(Database::open(&path).unwrap());
    assert_eq!(
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn old_preferences_upgrade_with_defaults_without_erasing_saved_choices() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("history.sqlite3");
    let db = Connection::open(&path).unwrap();
    for sql in [
        include_str!("../migrations/001_initial.sql"),
        include_str!("../migrations/002_lookup_cache.sql"),
        include_str!("../migrations/003_settings.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    db.execute("INSERT INTO app_state VALUES ('preferences', ?1)", [r#"{"version":1,"model":"saved-model","effort":"max","fast_mode":true,"skill_path":"/saved/SKILL.md"}"#]).unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    let preferences = db
        .read_state::<Preferences>("preferences")
        .unwrap()
        .unwrap();
    assert_eq!(preferences.model, "saved-model");
    assert_eq!(preferences.effort, "max");
    assert!(preferences.fast_mode);
    assert_eq!(preferences.skill_path.to_str(), Some("/saved/SKILL.md"));
    assert_eq!(preferences.cli_path, None);
    assert_eq!(preferences.appearance, Appearance::System);
}

#[test]
fn newer_history_is_left_untouched() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("future.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=99; CREATE TABLE future(payload TEXT); INSERT INTO future VALUES ('retain');").unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(Database::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
