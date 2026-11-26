//! Lowers the global note size limit, so it runs in its own test binary (its own process).
use fastobsidian::ops::{read_note, save_note, Error};
use fastobsidian::vault::{VaultIndex, MAX_NOTE_BYTES};
use std::fs;
use std::sync::atomic::Ordering;

fn ids(idx: &VaultIndex) -> Vec<String> {
    idx.build(true).note_ids().map(String::from).collect()
}

#[test]
fn oversized_notes_skipped_and_refused() {
    let v = std::env::temp_dir().join(format!("fo-size-{}", std::process::id()));
    let _ = fs::remove_dir_all(&v);
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("big.md"), "x".repeat(100)).unwrap();
    fs::write(v.join("small.md"), "x").unwrap();
    let old = MAX_NOTE_BYTES.swap(50, Ordering::SeqCst);
    let r = std::panic::catch_unwind(|| {
        assert_eq!(ids(&VaultIndex::new(&v)), ["small"]);
        assert!(matches!(read_note(&v, "big"), Err(Error::Invalid(_))));
        assert!(matches!(
            save_note(&v, &VaultIndex::new(&v), "small", &"y".repeat(60), None, false),
            Err(Error::Invalid(_))
        ));
    });
    MAX_NOTE_BYTES.store(old, Ordering::SeqCst);
    r.unwrap();
}
