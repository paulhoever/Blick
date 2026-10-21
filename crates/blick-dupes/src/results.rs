//! Ergebnisse: bestes Mitglied, Markierungen, Aktionen, Export.

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{DupeError, Result};
use crate::scan::{Group, Member};
use crate::source::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Meiste Pixel, bei Gleichstand größere Datei.
    Resolution,
    BiggestFile,
    Newest,
    Oldest,
    ShortestPath,
}

/// Index des Mitglieds, das bleiben soll: eine Referenz, sonst nach Kriterium.
pub fn best(group: &Group, keep: Keep) -> usize {
    if let Some(i) = group.members.iter().position(|m| m.reference) {
        return i;
    }
    let key = |m: &Member| -> (i128, i128) {
        let e = &m.entry;
        let px = e.dims.map_or(0, |(w, h)| w as i128 * h as i128);
        match keep {
            Keep::Resolution => (px, e.size as i128),
            Keep::BiggestFile => (e.size as i128, px),
            Keep::Newest => (e.mtime as i128, e.size as i128),
            Keep::Oldest => (-(e.mtime as i128), e.size as i128),
            Keep::ShortestPath => (-(e.uri.len() as i128), e.size as i128),
        }
    };
    (0..group.members.len()).max_by_key(|&i| key(&group.members[i])).unwrap_or(0)
}

/// Markierungen je Gruppe und Mitglied. Referenzen sind nie markiert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Marks(pub Vec<Vec<bool>>);

impl Marks {
    pub fn none(groups: &[Group]) -> Self {
        Self(groups.iter().map(|g| vec![false; g.members.len()]).collect())
    }

    pub fn all_but_best(groups: &[Group], keep: Keep) -> Self {
        Self(
            groups
                .iter()
                .map(|g| {
                    let b = best(g, keep);
                    g.members.iter().enumerate().map(|(i, m)| i != b && !m.reference).collect()
                })
                .collect(),
        )
    }

    /// Markiert alles, dessen Adresse `needle` enthält (Ordner oder Namensmuster);
    /// in jeder Gruppe bleibt mindestens ein Mitglied unmarkiert.
    pub fn by_substring(groups: &[Group], needle: &str) -> Self {
        let mut m = Self::none(groups);
        for (g, grp) in groups.iter().enumerate() {
            for (i, mem) in grp.members.iter().enumerate() {
                m.0[g][i] = !mem.reference && mem.entry.uri.contains(needle);
            }
            if m.0[g].iter().all(|&x| x) {
                m.0[g][0] = false;
            }
        }
        m
    }

    pub fn toggle(&mut self, g: usize, i: usize, groups: &[Group]) {
        if groups[g].members[i].reference {
            return;
        }
        self.0[g][i] = !self.0[g][i];
    }

    pub fn count(&self) -> usize {
        self.0.iter().flatten().filter(|&&b| b).count()
    }

    pub fn bytes(&self, groups: &[Group]) -> u64 {
        groups
            .iter()
            .zip(&self.0)
            .flat_map(|(g, m)| g.members.iter().zip(m))
            .filter(|(_, b)| **b)
            .map(|(m, _)| m.entry.size)
            .sum()
    }

    /// Gruppen, in denen alles markiert ist: das würde jede Kopie löschen.
    pub fn fully_marked_groups(&self) -> Vec<usize> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.is_empty() && m.iter().all(|&b| b))
            .map(|(i, _)| i)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Trash,
    /// Endgültig löschen (nur wo es keinen Papierkorb gibt).
    Delete,
    MoveTo(PathBuf),
    /// Durch Hardlink auf das beste Mitglied ersetzen (nur lokal, gleiches Dateisystem).
    Hardlink(Keep),
    Symlink(Keep),
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub done: usize,
    pub bytes: u64,
    pub failed: Vec<(String, String)>,
}

pub fn apply(
    groups: &[Group],
    marks: &Marks,
    action: &Action,
    storages: &[Arc<dyn Storage>],
) -> Result<Outcome> {
    if let Some(&g) = marks.fully_marked_groups().first() {
        return Err(DupeError::Other(format!(
            "Gruppe {} ist komplett markiert, nichts bliebe übrig",
            g + 1
        )));
    }
    let mut out = Outcome::default();
    for (g, grp) in groups.iter().enumerate() {
        for (i, m) in grp.members.iter().enumerate() {
            if !marks.0[g][i] || m.reference {
                continue;
            }
            let st = &storages[m.source];
            let r = match action {
                Action::Trash => st.trash(&m.entry),
                Action::Delete => st.delete(&m.entry),
                Action::MoveTo(dest) => move_to(st.as_ref(), m, dest),
                Action::Hardlink(k) | Action::Symlink(k) => {
                    let b = &grp.members[best(grp, *k)];
                    replace_with_link(st.as_ref(), m, storages[b.source].as_ref(), b, matches!(action, Action::Symlink(_)))
                }
            };
            match r {
                Ok(()) => {
                    out.done += 1;
                    out.bytes += m.entry.size;
                }
                Err(e) => out.failed.push((m.entry.uri.clone(), e.to_string())),
            }
        }
    }
    Ok(out)
}

fn local(st: &dyn Storage, m: &Member) -> Result<PathBuf> {
    st.local_path(&m.entry)
        .ok_or_else(|| DupeError::Other(format!("nur lokal möglich: {}", m.entry.uri)))
}

fn move_to(st: &dyn Storage, m: &Member, dest: &std::path::Path) -> Result<()> {
    let src = local(st, m)?;
    std::fs::create_dir_all(dest).map_err(|e| DupeError::io(dest.display(), e))?;
    let name = src.file_name().ok_or_else(|| DupeError::Other("kein Dateiname".into()))?;
    let mut target = dest.join(name);
    let mut n = 1;
    while target.exists() {
        let stem = src.file_stem().unwrap_or_default().to_string_lossy();
        let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        target = dest.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    if std::fs::rename(&src, &target).is_err() {
        // anderes Dateisystem: kopieren, dann erst löschen
        std::fs::copy(&src, &target).map_err(|e| DupeError::io(src.display(), e))?;
        std::fs::remove_file(&src).map_err(|e| DupeError::io(src.display(), e))?;
    }
    Ok(())
}

fn replace_with_link(
    st: &dyn Storage,
    m: &Member,
    best_st: &dyn Storage,
    best: &Member,
    symbolic: bool,
) -> Result<()> {
    let victim = local(st, m)?;
    let keeper = local(best_st, best)?;
    // Erst den Link unter Nebennamen anlegen, dann atomar drüberlegen:
    // scheitert das Anlegen, bleibt die Kopie unangetastet.
    let tmp = victim.with_extension("blick-link-tmp");
    let _ = std::fs::remove_file(&tmp);
    let made = if symbolic {
        std::os::unix::fs::symlink(&keeper, &tmp)
    } else {
        std::fs::hard_link(&keeper, &tmp)
    };
    made.map_err(|e| DupeError::io(keeper.display(), e))?;
    std::fs::rename(&tmp, &victim).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DupeError::io(victim.display(), e)
    })
}

pub fn export_csv(groups: &[Group], marks: &Marks) -> String {
    fn q(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    let mut out = String::from("Gruppe,Ähnlichkeit,Markiert,Referenz,Pfad,Größe,Breite,Höhe,Geändert\n");
    for (g, grp) in groups.iter().enumerate() {
        for (i, m) in grp.members.iter().enumerate() {
            let (w, h) = m.entry.dims.unwrap_or((0, 0));
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{}\n",
                g + 1,
                grp.score,
                if marks.0[g][i] { "ja" } else { "nein" },
                if m.reference { "ja" } else { "nein" },
                q(&m.entry.uri),
                m.entry.size,
                w,
                h,
                m.entry.mtime
            ));
        }
    }
    out
}

pub fn save_json(groups: &[Group]) -> String {
    serde_json::to_string(groups).unwrap_or_default()
}

pub fn load_json(s: &str) -> Result<Vec<Group>> {
    serde_json::from_str(s).map_err(|e| DupeError::Other(e.to_string()))
}
