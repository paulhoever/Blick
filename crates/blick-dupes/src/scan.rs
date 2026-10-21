//! Die Pipeline: Auflisten, vorfiltern, hashen, gruppieren.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::error::{DupeError, Result};
use crate::hashdb::{Col, HashDb};
use crate::sig::{self, Sig};
use crate::source::{Entry, Storage};

/// Bytes an Anfang, Mitte und Ende, die für den Vorab-Hash gelesen werden.
const PROBE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Gleiche Bytes.
    Contents,
    /// Ähnliche Bilder (15×15-Blocksignatur).
    Pictures,
    /// Ähnliche Dateinamen (Wortvergleich).
    Names,
    /// Ordner mit gleichem Inhalt (jede Datei hat ein Gegenstück).
    Folders,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub min_size: u64,
    pub max_size: u64,
    /// Nur diese Endungen (klein, ohne Punkt). Leer: alle Dateien.
    pub extensions: Vec<String>,
    /// Bildmodus: Mindestähnlichkeit in Prozent (dupeGuru-Standard 95).
    pub threshold: u8,
    /// Bildmodus: auch um 90° gedrehte Treffer.
    pub ignore_rotation: bool,
    /// Bildmodus: erlaubte Abweichung des Seitenverhältnisses; `None` = egal.
    pub aspect_tolerance: Option<f32>,
    /// Namensmodus: Wörter mit einem Tippfehler Abstand gelten als gleich.
    pub fuzzy_words: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Contents,
            min_size: 1,
            max_size: u64::MAX,
            extensions: Vec::new(),
            threshold: 95,
            ignore_rotation: false,
            aspect_tolerance: Some(0.02),
            fuzzy_words: false,
        }
    }
}

pub struct SourceSpec {
    pub storage: Arc<dyn Storage>,
    /// Referenz: wird nie als löschbar vorgeschlagen.
    pub reference: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Member {
    pub source: usize,
    pub entry: Entry,
    pub reference: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Group {
    pub members: Vec<Member>,
    /// Ähnlichkeit in Prozent; Inhalt gleich = 100.
    pub score: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Listing,
    Probing,
    Hashing,
    Grouping,
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
}

pub struct Scanner {
    pub sources: Vec<SourceSpec>,
    pub options: Options,
    pub db: Arc<HashDb>,
}

type Report<'a> = &'a (dyn Fn(Progress) + Sync);

impl Scanner {
    pub fn run(&self, cancel: &AtomicBool, report: Report) -> Result<Vec<Group>> {
        let files = self.list(cancel, report)?;
        let groups = match self.options.mode {
            Mode::Contents => self.by_contents(files, cancel, report)?,
            Mode::Pictures => self.by_pictures(files, cancel, report)?,
            Mode::Names => self.by_names(files, cancel)?,
            Mode::Folders => self.by_folders(files, cancel, report)?,
        };
        Ok(self.finish(groups))
    }

    fn list(&self, cancel: &AtomicBool, report: Report) -> Result<Vec<(usize, Entry)>> {
        let mut out = Vec::new();
        for (i, s) in self.sources.iter().enumerate() {
            s.storage.list(cancel, &mut |e| {
                if self.accepts(&e) {
                    out.push((i, e));
                    if out.len() % 500 == 0 {
                        report(Progress { stage: Stage::Listing, done: out.len(), total: 0 });
                    }
                }
            })?;
        }
        report(Progress { stage: Stage::Listing, done: out.len(), total: out.len() });
        Ok(out)
    }

    fn accepts(&self, e: &Entry) -> bool {
        let o = &self.options;
        if e.size < o.min_size || e.size > o.max_size {
            return false;
        }
        if o.mode == Mode::Pictures && !crate::sig::is_picture(&e.uri) {
            return false;
        }
        if o.extensions.is_empty() {
            return true;
        }
        let ext = e.uri.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase());
        ext.is_some_and(|x| o.extensions.contains(&x))
    }

    fn by_contents(
        &self,
        files: Vec<(usize, Entry)>,
        cancel: &AtomicBool,
        report: Report,
    ) -> Result<Vec<Group>> {
        let files = self.attach_checksums(files, cancel)?;
        // Stufe 1: Größe (und Server-Prüfsumme, wo vorhanden).
        let mut by_size: HashMap<(u64, Option<String>), Vec<(usize, Entry)>> = HashMap::new();
        for f in files {
            by_size.entry((f.1.size, f.1.checksum.clone())).or_default().push(f);
        }
        let mut cands: Vec<Vec<(usize, Entry)>> =
            by_size.into_values().filter(|v| v.len() > 1).collect();

        // Stufe 2: Vorab-Hash aus Anfang, Mitte und Ende. Entfällt, wenn der
        // Server schon eine Prüfsumme geliefert hat.
        cands = self.refine(cands, Stage::Probing, cancel, report, |s, e| self.probe_hash(s, e))?;
        // Stufe 3: voller Hash, nur wo der Vorab-Hash nicht schon alles gesehen hat.
        cands = self.refine(cands, Stage::Hashing, cancel, report, |s, e| self.full_hash(s, e))?;

        report(Progress { stage: Stage::Grouping, done: 1, total: 1 });
        Ok(cands
            .into_iter()
            .map(|v| Group {
                score: 100,
                members: v
                    .into_iter()
                    .map(|(source, entry)| Member {
                        source,
                        entry,
                        reference: self.sources[source].reference,
                    })
                    .collect(),
            })
            .collect())
    }

    /// Teilt jede Kandidatengruppe nach dem Hash weiter auf, parallel.
    fn refine(
        &self,
        groups: Vec<Vec<(usize, Entry)>>,
        stage: Stage,
        cancel: &AtomicBool,
        report: Report,
        hash: impl Fn(&dyn Storage, &Entry) -> Result<Vec<u8>> + Sync,
    ) -> Result<Vec<Vec<(usize, Entry)>>> {
        let total: usize = groups.iter().map(Vec::len).sum();
        let done = AtomicUsize::new(0);
        let items: Vec<(usize, usize, (usize, Entry))> = groups
            .into_iter()
            .enumerate()
            .flat_map(|(g, v)| v.into_iter().enumerate().map(move |(i, f)| (g, i, f)))
            .collect();
        let hashed: Vec<(usize, Vec<u8>, (usize, Entry))> = items
            .into_par_iter()
            .filter_map(|(g, _, f)| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let h = if f.1.checksum.is_some() {
                    Ok(Vec::new())
                } else {
                    hash(self.sources[f.0].storage.as_ref(), &f.1)
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 64 == 0 || n == total {
                    report(Progress { stage, done: n, total });
                }
                match h {
                    Ok(h) => Some((g, h, f)),
                    Err(e) => {
                        tracing::warn!("{}: {e}", f.1.uri);
                        None
                    }
                }
            })
            .collect();
        if cancel.load(Ordering::Relaxed) {
            return Err(DupeError::Cancelled);
        }
        let mut split: HashMap<(usize, Vec<u8>), Vec<(usize, Entry)>> = HashMap::new();
        for (g, h, f) in hashed {
            split.entry((g, h)).or_default().push(f);
        }
        let mut out: Vec<_> = split.into_values().filter(|v| v.len() > 1).collect();
        out.sort_by(|a, b| a[0].1.uri.cmp(&b[0].1.uri));
        Ok(out)
    }

    fn by_pictures(
        &self,
        files: Vec<(usize, Entry)>,
        cancel: &AtomicBool,
        report: Report,
    ) -> Result<Vec<Group>> {
        let total = files.len();
        let done = AtomicUsize::new(0);
        let sigs: Vec<Option<(usize, Entry, Sig)>> = files
            .into_par_iter()
            .map(|(si, mut e)| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let sig = match self.db.get(&e, Col::Sig).and_then(|b| Sig::from_blob(&b)) {
                    Some(s) => Some(s),
                    None => match sig::compute(self.sources[si].storage.as_ref(), &e) {
                        Ok(s) => {
                            self.db.put(&e, Col::Sig, &s.to_blob());
                            Some(s)
                        }
                        Err(err) => {
                            tracing::warn!("{}: {err}", e.uri);
                            None
                        }
                    },
                };
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 32 == 0 || n == total {
                    report(Progress { stage: Stage::Hashing, done: n, total });
                }
                sig.map(|s| {
                    e.dims.get_or_insert((s.width, s.height));
                    (si, e, s)
                })
            })
            .collect();
        if cancel.load(Ordering::Relaxed) {
            return Err(DupeError::Cancelled);
        }
        let items: Vec<(usize, Entry, Sig)> = sigs.into_iter().flatten().collect();
        report(Progress { stage: Stage::Grouping, done: 0, total: items.len() });

        let edges = similar_pairs(&items.iter().map(|i| &i.2).collect::<Vec<_>>(), &self.options, cancel)?;
        // Zusammenhangskomponenten, Gruppenwert = schwächste Kante.
        let mut parent: Vec<usize> = (0..items.len()).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        for &(a, b, _) in &edges {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra] = rb;
            }
        }
        let mut min_score: HashMap<usize, f32> = HashMap::new();
        for &(a, _, s) in &edges {
            let r = find(&mut parent, a);
            let m = min_score.entry(r).or_insert(100.0);
            *m = m.min(s);
        }
        let mut members: HashMap<usize, Vec<Member>> = HashMap::new();
        for (i, (source, entry, _)) in items.into_iter().enumerate() {
            let r = find(&mut parent, i);
            if min_score.contains_key(&r) {
                members.entry(r).or_default().push(Member {
                    source,
                    entry,
                    reference: self.sources[source].reference,
                });
            }
        }
        Ok(members
            .into_iter()
            .map(|(r, members)| Group { members, score: min_score[&r].floor() as u8 })
            .collect())
    }

    /// Hat eine Quelle Server-Prüfsummen (Immich), bekommen lokale Dateien gleicher
    /// Größe ebenfalls eine SHA-1, damit „liegt schon in Immich“ ohne Download geht.
    fn attach_checksums(
        &self,
        mut files: Vec<(usize, Entry)>,
        cancel: &AtomicBool,
    ) -> Result<Vec<(usize, Entry)>> {
        if !self.sources.iter().any(|s| s.storage.has_cheap_checksums()) {
            return Ok(files);
        }
        let sizes: std::collections::HashSet<u64> = files
            .iter()
            .filter(|(_, e)| e.checksum.is_some())
            .map(|(_, e)| e.size)
            .collect();
        files.par_iter_mut().for_each(|(si, e)| {
            if e.checksum.is_some() || !sizes.contains(&e.size) || cancel.load(Ordering::Relaxed) {
                return;
            }
            let hex = match self.db.get(e, Col::Sha1) {
                Some(h) => String::from_utf8(h).ok(),
                None => self.sources[*si].storage.read_all(e).ok().map(|b| {
                    use sha1::{Digest, Sha1};
                    let hex: String = Sha1::digest(&b).iter().map(|x| format!("{x:02x}")).collect();
                    self.db.put(e, Col::Sha1, hex.as_bytes());
                    hex
                }),
            };
            e.checksum = hex;
        });
        if cancel.load(Ordering::Relaxed) {
            return Err(DupeError::Cancelled);
        }
        Ok(files)
    }

    fn by_names(&self, files: Vec<(usize, Entry)>, cancel: &AtomicBool) -> Result<Vec<Group>> {
        let words: Vec<Vec<String>> = files.iter().map(|(_, e)| name_words(&e.uri)).collect();
        let fuzzy = self.options.fuzzy_words;
        let key = |w: &str| -> String { if fuzzy { w.chars().take(3).collect() } else { w.to_owned() } };
        let mut index: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, ws) in words.iter().enumerate() {
            for w in ws {
                index.entry(key(w)).or_default().push(i as u32);
            }
        }
        let thr = self.options.threshold as f32;
        let edges: Vec<Vec<(usize, usize, f32)>> = (0..files.len())
            .into_par_iter()
            .map(|i| {
                if cancel.load(Ordering::Relaxed) || words[i].is_empty() {
                    return Vec::new();
                }
                let mut seen = std::collections::HashSet::new();
                let mut out = Vec::new();
                for w in &words[i] {
                    let Some(b) = index.get(&key(w)) else { continue };
                    if b.len() > 3000 {
                        continue;
                    }
                    for &j in b {
                        let j = j as usize;
                        if j > i && seen.insert(j) {
                            let sc = word_score(&words[i], &words[j], fuzzy);
                            if sc >= thr {
                                out.push((i, j, sc));
                            }
                        }
                    }
                }
                out
            })
            .collect();
        if cancel.load(Ordering::Relaxed) {
            return Err(DupeError::Cancelled);
        }
        let edges: Vec<_> = edges.into_iter().flatten().collect();
        let members: Vec<Member> = files
            .into_iter()
            .map(|(source, entry)| Member { source, entry, reference: self.sources[source].reference })
            .collect();
        Ok(components(members, &edges))
    }

    fn by_folders(
        &self,
        files: Vec<(usize, Entry)>,
        cancel: &AtomicBool,
        report: Report,
    ) -> Result<Vec<Group>> {
        let mut dirs: HashMap<(usize, String), Vec<Entry>> = HashMap::new();
        for (s, e) in &files {
            let dir = e.uri.rsplit_once('/').map(|d| d.0).unwrap_or("").to_owned();
            dirs.entry((*s, dir)).or_default().push(e.clone());
        }
        let file_groups = self.by_contents(files, cancel, report)?;
        let mut gid: HashMap<(usize, String), usize> = HashMap::new();
        for (g, grp) in file_groups.iter().enumerate() {
            for m in &grp.members {
                gid.insert((m.source, m.entry.uri.clone()), g);
            }
        }
        let mut by_key: HashMap<Vec<usize>, Vec<Member>> = HashMap::new();
        'dir: for ((source, dir), es) in dirs {
            let mut key = Vec::with_capacity(es.len());
            for e in &es {
                match gid.get(&(source, e.uri.clone())) {
                    Some(g) => key.push(*g),
                    None => continue 'dir,
                }
            }
            key.sort_unstable();
            let entry = Entry {
                uri: dir,
                size: es.iter().map(|e| e.size).sum(),
                mtime: 0,
                id: None,
                checksum: None,
                dims: None,
            };
            by_key.entry(key).or_default().push(Member {
                source,
                entry,
                reference: self.sources[source].reference,
            });
        }
        Ok(by_key
            .into_values()
            .filter(|m| m.len() > 1)
            .map(|members| Group { members, score: 100 })
            .collect())
    }

    fn probe_hash(&self, s: &dyn Storage, e: &Entry) -> Result<Vec<u8>> {
        if let Some(h) = self.db.get(e, Col::Partial) {
            return Ok(h);
        }
        let mut hasher = blake3::Hasher::new();
        if e.size <= (PROBE * 3) as u64 {
            hasher.update(&s.read_all(e)?);
        } else {
            let mid = (e.size / 2).saturating_sub((PROBE / 2) as u64);
            for off in [0, mid, e.size - PROBE as u64] {
                hasher.update(&s.read_range(e, off, PROBE)?);
            }
        }
        let h = hasher.finalize().as_bytes().to_vec();
        self.db.put(e, Col::Partial, &h);
        Ok(h)
    }

    fn full_hash(&self, s: &dyn Storage, e: &Entry) -> Result<Vec<u8>> {
        if let Some(h) = self.db.get(e, Col::Full) {
            return Ok(h);
        }
        let h = blake3::hash(&s.read_all(e)?).as_bytes().to_vec();
        self.db.put(e, Col::Full, &h);
        Ok(h)
    }

    /// Hardlinks derselben Datei zählen einmal, reine Referenzgruppen fallen weg,
    /// Referenzen stehen vorn.
    fn finish(&self, groups: Vec<Group>) -> Vec<Group> {
        let mut out: Vec<Group> = groups
            .into_iter()
            .filter_map(|mut g| {
                let mut seen = std::collections::HashSet::new();
                g.members.retain(|m| match &m.entry.id {
                    Some(id) => seen.insert(id.clone()),
                    None => true,
                });
                let has_work = g.members.iter().any(|m| !m.reference);
                (g.members.len() > 1 && has_work).then(|| {
                    g.members.sort_by_key(|m| (!m.reference, m.entry.uri.clone()));
                    g
                })
            })
            .collect();
        out.sort_by(|a, b| {
            let size = |g: &Group| g.members[0].entry.size;
            size(b).cmp(&size(a)).then_with(|| a.members[0].entry.uri.cmp(&b.members[0].entry.uri))
        });
        out
    }
}

/// Alle Paare mit Ähnlichkeit >= Schwelle. Statt jeden mit jedem zu vergleichen:
/// vier 16-Bit-Streifen des Gradientenhashs als Index. Zwei Bilder, deren Hashes
/// sich in höchstens drei Bits unterscheiden, teilen immer einen Streifen.
pub(crate) fn similar_pairs(
    sigs: &[&Sig],
    o: &Options,
    cancel: &AtomicBool,
) -> Result<Vec<(usize, usize, f32)>> {
    const BUCKET_CAP: usize = 1500;
    let mut index: HashMap<(u8, u16), Vec<u32>> = HashMap::new();
    for (i, s) in sigs.iter().enumerate() {
        let h = s.hash();
        for band in 0..4u8 {
            index.entry((band, (h >> (band * 16)) as u16)).or_default().push(i as u32);
        }
    }
    let thr = o.threshold as f32;
    let rotations = if o.ignore_rotation { 4 } else { 1 };
    let edges: Vec<Vec<(usize, usize, f32)>> = (0..sigs.len())
        .into_par_iter()
        .map(|i| {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let mut best: HashMap<usize, f32> = HashMap::new();
            let mut q = (*sigs[i]).clone();
            for _ in 0..rotations {
                let h = q.hash();
                for band in 0..4u8 {
                    let Some(b) = index.get(&(band, (h >> (band * 16)) as u16)) else { continue };
                    if b.len() > BUCKET_CAP {
                        continue;
                    }
                    for &j in b {
                        let j = j as usize;
                        if j <= i {
                            continue;
                        }
                        let t = sigs[j];
                        if let Some(tol) = o.aspect_tolerance {
                            if !q.same_aspect(t, tol) {
                                continue;
                            }
                        }
                        let s = q.similarity(t);
                        if s >= thr {
                            let e = best.entry(j).or_insert(0.0);
                            *e = e.max(s);
                        }
                    }
                }
                q = q.rotated();
            }
            best.into_iter().map(|(j, s)| (i, j, s)).collect()
        })
        .collect();
    if cancel.load(Ordering::Relaxed) {
        return Err(DupeError::Cancelled);
    }
    Ok(edges.into_iter().flatten().collect())
}

fn components(members: Vec<Member>, edges: &[(usize, usize, f32)]) -> Vec<Group> {
    let mut parent: Vec<usize> = (0..members.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for &(a, b, _) in edges {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut score: HashMap<usize, f32> = HashMap::new();
    for &(a, _, s) in edges {
        let r = find(&mut parent, a);
        let m = score.entry(r).or_insert(100.0);
        *m = m.min(s);
    }
    let mut out: HashMap<usize, Vec<Member>> = HashMap::new();
    for (i, m) in members.into_iter().enumerate() {
        let r = find(&mut parent, i);
        if score.contains_key(&r) {
            out.entry(r).or_default().push(m);
        }
    }
    out.into_iter()
        .map(|(r, members)| Group { members, score: score[&r].floor() as u8 })
        .collect()
}

/// Kleinbuchstaben-Wörter des Dateinamens ohne Endung; Ziffern gehören dazu.
fn name_words(uri: &str) -> Vec<String> {
    let name = unescape(uri.rsplit('/').next().unwrap_or(uri));
    let name = name.as_str();
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn lev_le1(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let i = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (&a[i..], &b[i..]);
    match ra.len().cmp(&rb.len()) {
        std::cmp::Ordering::Equal => ra.get(1..) == rb.get(1..),
        std::cmp::Ordering::Greater => ra[1..] == rb[..],
        std::cmp::Ordering::Less => ra[..] == rb[1..],
    }
}

/// Dice-Koeffizient über die Wörter, in Prozent.
fn word_score(a: &[String], b: &[String], fuzzy: bool) -> f32 {
    let mut used = vec![false; b.len()];
    let mut common = 0;
    for w in a {
        let hit = b.iter().enumerate().position(|(k, v)| {
            !used[k] && (v == w || (fuzzy && w.chars().count() >= 5 && lev_le1(w, v)))
        });
        if let Some(k) = hit {
            used[k] = true;
            common += 1;
        }
    }
    200.0 * common as f32 / (a.len() + b.len()) as f32
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
