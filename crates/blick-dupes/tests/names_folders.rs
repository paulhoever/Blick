use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use blick_dupes::hashdb::HashDb;
use blick_dupes::{LocalStorage, Mode, Options, Scanner, SourceSpec};

fn run(dir: &std::path::Path, tweak: impl FnOnce(&mut Options)) -> Vec<blick_dupes::Group> {
    let mut options = Options { threshold: 80, ..Options::default() };
    tweak(&mut options);
    Scanner {
        sources: vec![SourceSpec { storage: Arc::new(LocalStorage::new(dir)), reference: false }],
        options,
        db: Arc::new(HashDb::in_memory().unwrap()),
    }
    .run(&AtomicBool::new(false), &|_| {})
    .unwrap()
}

#[test]
fn similar_names_group_by_words() {
    let d = tempfile::tempdir().unwrap();
    for n in ["Urlaub Italien 2024.jpg", "urlaub_italien_2024 (1).jpg", "Geburtstag.jpg", "urlaub_italien.mp4"] {
        fs::write(d.path().join(n), n).unwrap();
    }
    let g = run(d.path(), |o| o.mode = Mode::Names);
    assert_eq!(g.len(), 1);
    assert!(g[0].members.len() >= 2 && g[0].members.iter().all(|m| !m.entry.uri.contains("Geburtstag")));
}

#[test]
fn fuzzy_words_catch_typos() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("Hochzeit Anna.jpg"), "1").unwrap();
    fs::write(d.path().join("Hochzeitt Anna.jpg"), "2").unwrap();
    assert!(run(d.path(), |o| { o.mode = Mode::Names; o.threshold = 90 }).is_empty());
    assert_eq!(run(d.path(), |o| { o.mode = Mode::Names; o.threshold = 90; o.fuzzy_words = true }).len(), 1);
}

#[test]
fn identical_folders_are_found() {
    let d = tempfile::tempdir().unwrap();
    for dir in ["a", "b", "c"] {
        fs::create_dir(d.path().join(dir)).unwrap();
    }
    for dir in ["a", "b"] {
        fs::write(d.path().join(dir).join("1.txt"), "eins").unwrap();
        fs::write(d.path().join(dir).join("2.txt"), "zwei").unwrap();
    }
    fs::write(d.path().join("c/1.txt"), "eins").unwrap();
    fs::write(d.path().join("c/3.txt"), "drei").unwrap();
    let g = run(d.path(), |o| o.mode = Mode::Folders);
    assert_eq!(g.len(), 1);
    let mut dirs: Vec<_> = g[0].members.iter().map(|m| m.entry.uri.rsplit('/').next().unwrap().to_string()).collect();
    dirs.sort();
    assert_eq!(dirs, ["a", "b"]);
}
