//! Lecture + suivi du fichier de log, cross-platform.
//!
//! Pas de `tail -f` ni d'inotify (Unix seulement) : un SUIVEUR À SONDAGE —
//! toutes les 300 ms, on rouvre le fichier et on lit les octets nouveaux.
//! C'est le même algorithme partout, il survit à la rotation du logger
//! (fichier renommé : la taille recule, on repart du début du « nouveau »
//! fichier) et il ne consomme rien quand rien n'arrive.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

pub const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Lit les `n` DERNIÈRES lignes d'un fichier (sans le charger entièrement :
/// lecture par blocs depuis la fin). Retourne aussi le décalage d'octets de
/// fin, pour enchaîner avec un suivi.
pub fn tail_lines(path: &Path, n: usize) -> (Vec<String>, u64) {
    let Ok(file) = std::fs::File::open(path) else {
        return (Vec::new(), 0);
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return (Vec::new(), 0);
    };
    if size == 0 || n == 0 {
        return (Vec::new(), size);
    }

    // Blocs de 8 Ko depuis la fin jusqu'à avoir assez de retours à la ligne.
    let mut chunk = 8 * 1024u64;
    let mut lines: Vec<String> = Vec::new();
    let mut pos = size;
    let mut carry = String::new();
    while pos > 0 && lines.len() <= n {
        let start = pos.saturating_sub(chunk);
        let mut buf = vec![0u8; (pos - start) as usize];
        let mut f = file.try_clone().expect("cloning a File cannot fail");
        f.seek(SeekFrom::Start(start)).ok();
        f.read_exact(&mut buf).ok();
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        text.push_str(&carry);
        let mut parts: Vec<&str> = text.split('\n').collect();
        // Le dernier élément est le début de ligne (tronqué par le bloc).
        carry = parts.pop().unwrap_or("").to_string();
        lines.splice(0..0, parts.into_iter().map(String::from));
        if start == 0 {
            if !carry.is_empty() {
                lines.insert(0, carry);
            }
            break;
        }
        pos = start;
        chunk = chunk.saturating_mul(2).min(64 * 1024);
    }
    // On a collecté au plus un bloc de trop : on ne garde que les n
    // dernières (les plus récentes — les insertions se font en tête, donc
    // on évacue l'excédent PAR L'AVANT).
    while lines.len() > n {
        lines.remove(0);
    }
    (lines, size)
}

/// Suiveur de fichier : retourne les nouvelles lignes depuis `offset`.
/// Survit à la rotation (taille qui recule ⇒ nouveau fichier, offset 0).
pub struct Follower {
    offset: u64,
}

impl Follower {
    pub fn new(offset: u64) -> Self {
        Self { offset }
    }

    /// Lit les nouvelles lignes si le fichier a grandi. Ne retourne rien si
    /// le fichier a disparu (le logger ne l'a pas encore créé).
    pub fn poll(&mut self, path: &Path) -> Vec<String> {
        let Ok(file) = std::fs::File::open(path) else {
            return Vec::new();
        };
        let Ok(size) = file.metadata().map(|m| m.len()) else {
            return Vec::new();
        };
        if size < self.offset {
            // Rotation (ou fichier réécrit) : on repart du début.
            self.offset = 0;
        }
        if size <= self.offset {
            return Vec::new();
        }
        let mut f = file;
        let _ = f.seek(SeekFrom::Start(self.offset));
        let mut reader = BufReader::new(f.take(size - self.offset));
        let mut lines = Vec::new();
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    self.offset += buf.len() as u64;
                    let mut line = buf.clone();
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    lines.push(line);
                }
                Err(_) => break,
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file() -> (std::path::PathBuf, std::fs::File) {
        let dir = std::env::temp_dir().join(format!("apabot-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("log-{:?}.txt", std::time::SystemTime::now()));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let (path, mut file) = temp_file();
        for i in 0..10 {
            writeln!(file, "line {i}").unwrap();
        }
        let (lines, _) = tail_lines(&path, 3);
        assert_eq!(lines, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn follower_reads_only_new_lines() {
        let (path, mut file) = temp_file();
        writeln!(file, "one").unwrap();
        writeln!(file, "two").unwrap();
        let (_, offset) = tail_lines(&path, 10);
        assert_eq!(offset, 8); // "one\ntwo\n" = 8 octets

        let mut follower = Follower::new(offset);
        assert!(follower.poll(&path).is_empty());
        writeln!(file, "three").unwrap();
        assert_eq!(follower.poll(&path), vec!["three".to_string()]);
    }

    #[test]
    fn follower_restarts_after_rotation() {
        let (path, mut file) = temp_file();
        writeln!(file, "aaaa").unwrap();
        let mut follower = Follower::new(10);
        // Fichier réécrit plus petit : rotation simulée.
        let mut f2 = std::fs::File::create(&path).unwrap();
        writeln!(f2, "bb").unwrap();
        assert_eq!(follower.poll(&path), vec!["bb".to_string()]);
    }
}
