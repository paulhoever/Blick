use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use blick_dupes::hashdb::HashDb;
use blick_dupes::{LocalStorage, Options, Scanner, SourceSpec};

fn scanner(roots: &[(&std::path::Path, bool)]) -> Scanner {
    Scanner {
        sources: roots
            .iter()
            .map(|(p, r)| SourceSpec { storage: Arc::new(LocalStorage::new(*p)), reference: *r })
            .collect(),
        options: Options::default(),
        db: Arc::new(HashDb::in_memory().unwrap()),
    }
}

fn run(s: &Scanner) -> Vec<blick_dupes::Group> {
    s.run(&AtomicBool::new(false), &|_| {}).unwrap()
}

#[test]
fn finds_identical_content_and_ignores_same_size_difference() {
    let d = tempfile::tempdir().unwrap();
    let big = vec![7u8; 400_000];
    let mut other = big.clone();
    other[200_000] = 9; // gleiche Größe, Unterschied mitten in der Datei (außerhalb der Probe? Mitte ist Probe)
    other[100_000] = 9; // zwischen den Proben: nur der Volllhash sieht es
    fs::write(d.path().join("a.bin"), &big).unwrap();
    fs::create_dir(d.path().join("sub")).unwrap();
    fs::write(d.path().join("sub/b.bin"), &big).unwrap();
    fs::write(d.path().join("c.bin"), &other).unwrap();
    fs::write(d.path().join("tiny.txt"), b"x").unwrap();
    let groups = run(&scanner(&[(d.path(), false)]));
    assert_eq!(groups.len(), 1);
    let names: Vec<_> = groups[0].members.iter().map(|m| m.entry.uri.rsplit('/').next().unwrap().to_string()).collect();
    assert_eq!(names, ["a.bin", "b.bin"]);
    assert_eq!(groups[0].score, 100);
}

#[test]
fn hardlinks_count_once() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), b"hello").unwrap();
    fs::hard_link(d.path().join("a"), d.path().join("b")).unwrap();
    assert!(run(&scanner(&[(d.path(), false)])).is_empty());
}

#[test]
fn reference_only_groups_are_dropped_and_references_come_first() {
    let r = tempfile::tempdir().unwrap();
    let n = tempfile::tempdir().unwrap();
    fs::write(r.path().join("x1"), b"same").unwrap();
    fs::write(r.path().join("x2"), b"same").unwrap();
    fs::write(r.path().join("y"), b"only-ref").unwrap();
    fs::write(n.path().join("y2"), b"other").unwrap();
    fs::write(n.path().join("x3"), b"same").unwrap();
    let g = run(&scanner(&[(n.path(), false), (r.path(), true)]));
    assert_eq!(g.len(), 1);
    assert!(g[0].members[0].reference && g[0].members[1].reference);
    assert!(!g[0].members[2].reference);
}

#[test]
fn second_run_hits_cache() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), vec![1u8; 300_000]).unwrap();
    fs::write(d.path().join("b"), vec![1u8; 300_000]).unwrap();
    let s = scanner(&[(d.path(), false)]);
    assert_eq!(run(&s).len(), 1);
    assert_eq!(run(&s).len(), 1);
}
