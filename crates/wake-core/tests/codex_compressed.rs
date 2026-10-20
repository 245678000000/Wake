//! Compressed Codex rollouts use the same identity, metadata and index as JSONL.
//! All transcripts and databases here are synthetic.
mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::json;
use wake_core::adapters::codex::CodexAdapter;
use wake_core::adapters::AgentAdapter;
use wake_core::db::Store;
use wake_core::models::SessionFilter;
use wake_core::scanner::{run_scan, scan_files, NullEvents};

const ID: &str = "22222222-aaaa-bbbb-cccc-000000000002";
const NAME: &str = "rollout-2026-08-02T09-15-00-22222222-aaaa-bbbb-cccc-000000000002.jsonl";

fn rollout() -> String {
    fs::read_to_string(common::fixture(&format!(
        "codex/sessions/2026/08/02/{NAME}"
    )))
    .unwrap()
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn modified_after(path: &Path, seconds: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn compressed_rollouts_preserve_transcripts_and_state_metadata() {
    let home = tempfile::tempdir().unwrap();
    let plain = home.path().join("sessions/2026/08/02").join(NAME);
    write(&plain, rollout());
    let adapter = CodexAdapter::new().with_custom_root(home.path().to_path_buf());
    let original = adapter
        .parse_transcript(&adapter.file_ref(&plain).unwrap())
        .unwrap();
    let compressed = plain.with_extension("jsonl.zst");
    // Both single-frame and concatenated-frame files must decode to the last line.
    for bytes in [
        zstd::encode_all(rollout().as_bytes(), 3).unwrap(),
        common::dsh_zstd(&rollout()),
    ] {
        write(&compressed, &bytes);
        let r = adapter.file_ref(&compressed).unwrap();
        assert_eq!(r.native_id, ID);
        let parsed = adapter.parse_session(&r).unwrap();
        let transcript = adapter.parse_transcript(&r).unwrap();
        assert_eq!(parsed.meta.key, original.meta.key);
        assert_eq!(parsed.meta.title, original.meta.title);
        assert_eq!(parsed.meta.tokens_used, original.meta.tokens_used);
        assert_eq!(parsed.meta.message_count, original.meta.message_count);
        assert_eq!(
            serde_json::to_value(&transcript.mainline).unwrap(),
            serde_json::to_value(&original.mainline).unwrap()
        );
        assert_eq!(
            fs::read(&compressed).unwrap(),
            bytes,
            "reading must not rewrite the source"
        );
    }
    let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE threads (id TEXT, rollout_path TEXT, cwd TEXT, title TEXT, name TEXT,
        tokens_used INTEGER, archived INTEGER, git_branch TEXT, model TEXT, source TEXT,
        created_at_ms INTEGER, updated_at_ms INTEGER);",
    )
    .unwrap();
    // As on real migrated stores, the database still names the uncompressed file;
    // the foreign prefix also exercises remote-cache matching by filename.
    db.execute(
        "INSERT INTO threads VALUES (?1, ?2, '/work/project', 'raw title', 'My renamed session',
        4321, 0, NULL, NULL, 'cli', 1, 2)",
        rusqlite::params![ID, format!("/home/remote/.codex/sessions/{NAME}")],
    )
    .unwrap();
    drop(db);
    let r = adapter.file_ref(&compressed).unwrap();
    let quick = adapter.quick_meta(std::slice::from_ref(&r)).unwrap();
    let parsed = adapter.parse_session(&r).unwrap();
    let merged = adapter.merge_quick_meta(parsed.meta, &quick[&r.file_path]);
    assert_eq!(merged.title, "My renamed session");
    assert_eq!(merged.key, format!("codex:{ID}"));
    assert_eq!(merged.file_path, compressed.to_string_lossy());
    assert_eq!(
        adapter.session_paths(&merged).len(),
        2,
        "cleanup owns both encodings of one session"
    );
}

#[test]
fn compressed_archives_and_thread_filtering_use_the_same_boundary() {
    let home = tempfile::tempdir().unwrap();
    let archived = home.path().join("archived_sessions");
    let compressed = archived.join(format!("{NAME}.zst"));
    write(&compressed, common::dsh_zstd(&rollout()));
    let adapter = CodexAdapter::new().with_custom_root(archived.clone());
    let refs = adapter.list_session_files().unwrap();
    assert_eq!(refs.len(), 1);
    assert!(adapter.parse_session(&refs[0]).unwrap().meta.archived);
    // A standalone, arbitrarily named flat rollout store also needs recognition.
    let flat = home.path().join("exported-copy");
    write(
        &flat.join(format!("{NAME}.zst")),
        common::dsh_zstd(&rollout()),
    );
    assert_eq!(
        CodexAdapter::new()
            .with_custom_root(flat)
            .list_session_files()
            .unwrap()
            .len(),
        1
    );

    let adapter = CodexAdapter::new().with_custom_root(home.path().to_path_buf());
    let day = home.path().join("sessions/2026/10/19");
    for (id, source, visible) in [
        ("user", json!("cli"), true),
        ("guardian", json!({"subagent":{"other":"guardian"}}), false),
        ("review", json!({"subagent":"review"}), false),
        (
            "child",
            json!({"subagent":{"thread_spawn":{"agent_path":"/root/check"}}}),
            true,
        ),
    ] {
        let path = day.join(format!("{id}.jsonl.zst"));
        let header = format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":id,"source":source}})
        );
        write(&path, common::dsh_zstd(&header));
        assert_eq!(adapter.file_ref(&path).is_some(), visible, "{id}");
    }
    let refs = adapter.list_session_files().unwrap();
    assert_eq!(
        refs.len(),
        3,
        "only user, spawned child and archived user are visible"
    );
    assert!(refs
        .iter()
        .all(|r| adapter.file_ref(Path::new(&r.file_path)).is_some()));
}

#[test]
fn compression_transitions_keep_one_searchable_session() {
    let home = tempfile::tempdir().unwrap();
    let plain = home.path().join("sessions/2026/08/02").join(NAME);
    write(&plain, rollout());
    let adapters = vec![CodexAdapter::new().with_custom_root(home.path().to_path_buf())];
    let store = Arc::new(Store::open(&home.path().join("index.db")).unwrap());
    run_scan(&adapters, &store, &NullEvents, false).unwrap();
    let key = format!("codex:{ID}");
    let compressed = plain.with_extension("jsonl.zst");
    write(
        &compressed,
        common::dsh_zstd(&rollout().replace("useEffect", "compressedmarker7429")),
    );
    modified_after(&compressed, 5);
    let r = adapters[0].file_ref(&compressed).unwrap();
    scan_files(&adapters, &store, &NullEvents, vec![r]);
    assert_eq!(
        store.get_session(&key).unwrap().unwrap().file_path,
        compressed.to_string_lossy()
    );
    assert!(!store
        .search("compressedmarker7429", &[], None, 10)
        .unwrap()
        .0
        .is_empty());
    // A late event for the older plain copy must not replace the compressed one.
    scan_files(
        &adapters,
        &store,
        &NullEvents,
        vec![adapters[0].file_ref(&plain).unwrap()],
    );
    assert_eq!(
        store.get_session(&key).unwrap().unwrap().file_path,
        compressed.to_string_lossy()
    );
    fs::remove_file(&plain).unwrap();
    run_scan(&adapters, &store, &NullEvents, false).unwrap();
    let (_, total) = store
        .list_sessions(&SessionFilter {
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(total, 1);
    // Resuming a session can recreate its writable JSONL; the newer copy wins.
    write(&plain, rollout().replace("useEffect", "resumedmarker7429"));
    modified_after(&plain, 10);
    run_scan(&adapters, &store, &NullEvents, false).unwrap();
    assert_eq!(
        store.get_session(&key).unwrap().unwrap().file_path,
        plain.to_string_lossy()
    );
    assert!(!store
        .search("resumedmarker7429", &[], None, 10)
        .unwrap()
        .0
        .is_empty());
    assert!(store
        .search("compressedmarker7429", &[], None, 10)
        .unwrap()
        .0
        .is_empty());
    let (_, total) = store
        .list_sessions(&SessionFilter {
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(total, 1);
}

#[test]
fn damaged_compressed_rollouts_report_errors_without_rewriting_files() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("sessions").join(format!("{NAME}.zst"));
    let adapter = CodexAdapter::new().with_custom_root(home.path().to_path_buf());
    let mut truncated = zstd::encode_all(rollout().as_bytes(), 3).unwrap();
    truncated.truncate(truncated.len() - 8);
    for bytes in [b"not a zstd frame".to_vec(), truncated] {
        write(&path, &bytes);
        let r = adapter.file_ref(&path).unwrap();
        assert!(adapter.parse_session(&r).is_err());
        assert!(adapter.parse_transcript(&r).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}
