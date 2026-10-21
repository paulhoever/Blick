use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use blick_dupes::hashdb::HashDb;
use blick_dupes::results::{self, Action, Keep, Marks};
use blick_dupes::{LocalStorage, Options, Scanner, SourceSpec, Storage};

fn groups(dir: &std::path::Path, reference: bool) -> (Vec<blick_dupes::Group>, Vec<Arc<dyn Storage>>) {
    let st: Arc<dyn Storage> = Arc::new(LocalStorage::new(dir));
    let g = Scanner {
        sources: vec![SourceSpec { storage: st.clone(), reference }],
        options: Options::default(),
        db: Arc::new(HashDb::in_memory().unwrap()),
    }
    .run(&AtomicBool::new(false), &|_| {})
    .unwrap();
    (g, vec![st])
}

#[test]
fn all_but_best_keeps_one_and_refuses_full_marking() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), "dup").unwrap();
    fs::write(d.path().join("bb"), "dup").unwrap();
    let (g, st) = groups(d.path(), false);
    let m = Marks::all_but_best(&g, Keep::ShortestPath);
    assert_eq!(m.count(), 1);
    assert!(!m.0[0][g[0].members.iter().position(|x| x.entry.uri.ends_with("/a")).unwrap()]);
    let all = Marks(vec![vec![true, true]]);
    assert!(results::apply(&g, &all, &Action::MoveTo(d.path().join("x")), &st).is_err());
}

#[test]
fn hardlink_replaces_copy_and_move_works() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), "dup").unwrap();
    fs::write(d.path().join("bb"), "dup").unwrap();
    let (g, st) = groups(d.path(), false);
    let m = Marks::all_but_best(&g, Keep::ShortestPath);
    let o = results::apply(&g, &m, &Action::Hardlink(Keep::ShortestPath), &st).unwrap();
    assert_eq!((o.done, o.failed.len()), (1, 0));
    assert_eq!(fs::metadata(d.path().join("a")).unwrap().ino(), fs::metadata(d.path().join("bb")).unwrap().ino());

    fs::write(d.path().join("c"), "other").unwrap();
    fs::write(d.path().join("dd"), "other").unwrap();
    let (g, st) = groups(d.path(), false);
    let m = Marks::all_but_best(&g, Keep::ShortestPath);
    let dest = d.path().join("weg");
    let o = results::apply(&g, &m, &Action::MoveTo(dest.clone()), &st).unwrap();
    assert_eq!(o.done, 1);
    assert!(dest.join("dd").exists() && !d.path().join("dd").exists());
}

#[test]
fn csv_and_json_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), "dup").unwrap();
    fs::write(d.path().join("b"), "dup").unwrap();
    let (g, _) = groups(d.path(), false);
    let m = Marks::none(&g);
    assert_eq!(results::export_csv(&g, &m).lines().count(), 3);
    assert_eq!(results::load_json(&results::save_json(&g)).unwrap(), g);
}
