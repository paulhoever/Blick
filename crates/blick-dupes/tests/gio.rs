#![cfg(feature = "gio")]
use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use blick_dupes::gio_storage::GioStorage;
use blick_dupes::hashdb::HashDb;
use blick_dupes::{Options, Scanner, SourceSpec};

#[test]
fn gio_scans_file_uris_with_range_reads() {
    let d = tempfile::tempdir().unwrap();
    let big = vec![5u8; 500_000];
    fs::write(d.path().join("a.bin"), &big).unwrap();
    fs::create_dir(d.path().join("s")).unwrap();
    fs::write(d.path().join("s/b b.bin"), &big).unwrap();
    fs::write(d.path().join("c.bin"), vec![6u8; 500_000]).unwrap();
    let uri = format!("file://{}", d.path().display());
    let g = Scanner {
        sources: vec![SourceSpec { storage: Arc::new(GioStorage::new(uri)), reference: false }],
        options: Options::default(),
        db: Arc::new(HashDb::in_memory().unwrap()),
    }
    .run(&AtomicBool::new(false), &|_| {})
    .unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].members.len(), 2);
}
