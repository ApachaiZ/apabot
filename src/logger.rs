//! Logger à double destination (console + fichier), avec rotation automatique.
//!
//! Parité avec `lib/logger.js` :
//! - fichier : `ISO [TAG] message` (grep-friendly), rotation 5 Mo × 5 fichiers ;
//! - console : `HH:MM:SS icône message`, colorée uniquement sur un vrai TTY
//!   (jamais quand la sortie n'est pas un terminal : systemd, superviseur…)
//!   et si `NO_COLOR` n'est pas défini.
//!
//! Conception Rust : un état global unique derrière un `Mutex`, initialisé
//! paresseusement via [`OnceLock`] (l'équivalent d'un "singleton lazy").
//! Les fonctions [`info`], [`warn`] et [`error`] sont donc appelables depuis
//! n'importe quel module sans se passer le logger de main en main.
//!
//! ⚠️ Limite assumée (documentée pour le cours) : l'écriture fichier est
//! bloquante et a lieu dans la main courante de l'exécuteur tokio. À cette
//! échelle (quelques lignes par seconde), c'est négligeable. Si le débit
//! augmentait un jour, la bonne évolution serait un thread dédié alimenté
//! par un canal `mpsc` — la structure du module (état global + mutex) le
//! permettrait sans toucher aux appels.

use chrono::Utc;
use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use crate::paths;

const MAX_BYTES: u64 = 5 * 1024 * 1024; // 5 Mo par fichier
const MAX_FILES: u32 = 5;               // log, log.1 … log.5
/// Vérification de la taille au plus une fois toutes les 30 s : un `stat`
/// par écriture serait du gaspillage (même optimisation que le JS).
const ROTATE_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    /// Étiquette fixe du fichier de log (l'`ERROR` n'a pas d'espace final,
    /// comme dans la version JS : `[ERROR]`).
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Level::Info => "•",
            Level::Warn => "▲",
            Level::Error => "✖",
        }
    }
}

/// État interne du logger : handle de fichier + horodatage de la dernière
/// vérification de rotation + mode couleur.
struct LoggerState {
    file: Option<File>,
    last_rotate_check: Instant,
    color: bool,
    /// `true` quand le bot tourne comme ENFANT du superviseur : sa sortie
    /// standard est capturée par le superviseur (→ `passthrough` → fichier).
    /// Écrire aussi sur la console dupliquerait chaque ligne dans le log.
    console: bool,
}

/// Singleton du logger. `OnceLock` garantit une initialisation unique et
/// sûre même si plusieurs tâches écrivent simultanément au démarrage.
static STATE: OnceLock<Mutex<LoggerState>> = OnceLock::new();

fn open_log_file() -> Option<File> {
    fs::create_dir_all(paths::logs_dir()).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::log_file())
        .ok()
}

/// Initialisation paresseuse (appelée au premier log si `init` n'a pas été
/// appelée explicitement).
fn state() -> &'static Mutex<LoggerState> {
    STATE.get_or_init(|| {
        Mutex::new(LoggerState {
            file: open_log_file(),
            last_rotate_check: Instant::now(),
            // Couleur uniquement sur un vrai terminal, comme le JS :
            // `process.stdout.isTTY && !process.env.NO_COLOR`.
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
            // Le superviseur passe `APABOT_DAEMON_CHILD=1` à l'enfant : sa
            // console est un pipe, pas un terminal — et l'écrire y ferait
            // doubler chaque ligne dans le fichier (via `passthrough`).
            console: std::env::var_os("APABOT_DAEMON_CHILD").is_none(),
        })
    })
}

pub fn init() {
    let _ = state();
}

/// Rotation : `log` → `log.1` → … → `log.5` (le plus ancien est écrasé).
/// Appelée uniquement quand la taille dépasse `MAX_BYTES`.
fn rotate(st: &mut LoggerState) {
    let Ok(meta) = fs::metadata(paths::log_file()) else {
        return;
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    // Décalage des anciens fichiers.
    for i in (1..MAX_FILES).rev() {
        let a = paths::log_file().with_extension(format!("{i}"));
        let b = paths::log_file().with_extension(format!("{}", i + 1));
        if a.exists() {
            let _ = fs::rename(a, b);
        }
    }
    let rotated = paths::log_file().with_extension("1");
    let _ = fs::rename(paths::log_file(), rotated);
    // On rouvre un fichier vierge : le handle `st.file` pointe sur l'ancien
    // inode (renommé), il faut le remplacer.
    st.file = open_log_file();
}

/// Écrit une ligne au niveau demandé, dans le fichier ET sur la console.
fn write(level: Level, message: &str) {
    let st = state();
    let mut guard = match st.lock() {
        Ok(g) => g,
        // Un pointeur empoisonné (panic précédent pendant l'écriture) ne
        // doit pas faire tomber le bot : on récupère quand même l'état.
        Err(poisoned) => poisoned.into_inner(),
    };

    let now = Utc::now();

    // ── Fichier : format stable `ISO [TAG] message` ──
    // Vérification de rotation throttlée (30 s max d'intervalle) AVANT de
    // prendre le handle en emprunt mut : le vérificateur d'emprunt interdit
    // de toucher `guard` pendant que `file` est emprunté.
    let rotate_now = guard.file.is_some() && guard.last_rotate_check.elapsed() >= ROTATE_CHECK;
    if rotate_now {
        guard.last_rotate_check = Instant::now();
        rotate(&mut guard);
    }

    let file_line = format!("{} [{}] {}\n", now.format("%Y-%m-%dT%H:%M:%S%.3fZ"), level.tag(), message);
    if let Some(file) = guard.file.as_mut() {
        let _ = file.write_all(file_line.as_bytes());
        let _ = file.flush(); // flush à chaque ligne : les logs survivent à un crash.
    }

    // ── Console : `HH:MM:SS icône message`, colorée si TTY ──
    // (Sautée sous superviseur : `passthrough` capture déjà stdout/stderr.)
    if !guard.console {
        return;
    }
    let console_line = format!("{} {} {}\n", now.format("%H:%M:%S"), level.icon(), message);
    let painted = if guard.color {
        // Codes ANSI : temps grisé, icône colorée selon le niveau.
        let color = match level {
            Level::Info => "\x1b[90m",
            Level::Warn => "\x1b[33m",
            Level::Error => "\x1b[31m",
        };
        format!(
            "\x1b[2m{}\x1b[0m {}{}\x1b[0m {}\n",
            now.format("%H:%M:%S"),
            color,
            level.icon(),
            message
        )
    } else {
        console_line
    };
    if level == Level::Error {
        let _ = std::io::stderr().write_all(painted.as_bytes());
    } else {
        let _ = std::io::stdout().write_all(painted.as_bytes());
    }
}

pub fn info(message: impl AsRef<str>) {
    write(Level::Info, message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write(Level::Warn, message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write(Level::Error, message.as_ref());
}

/// Écrit une ligne BRUTE dans le fichier de log uniquement (ni console, ni
/// horodatage, ni niveau) — utilisée par le superviseur pour capturer la
/// sortie standard/erreur de l'enfant (panics, backtraces…) dans le MÊME
/// fichier, en respectant la rotation. La ligne est déjà terminée ou pas :
/// on ajoute le `\n` manquant pour garder une ligne par entrée.
pub fn passthrough(line: impl AsRef<str>) {
    let st = state();
    let mut guard = match st.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let rotate_now = guard.file.is_some() && guard.last_rotate_check.elapsed() >= ROTATE_CHECK;
    if rotate_now {
        guard.last_rotate_check = Instant::now();
        rotate(&mut guard);
    }
    if let Some(file) = guard.file.as_mut() {
        let line = line.as_ref();
        let _ = file.write_all(line.as_bytes());
        if !line.ends_with('\n') {
            let _ = file.write_all(b"\n");
        }
        let _ = file.flush();
    }
}

/// Encadre des lignes du cadre « pixel art » de la bannière (`╭─╮ │ │ ╰─╯`).
/// La largeur est calculée sur la largeur VISUELLE (emoji = 2 colonnes).
/// Retourne (haut, lignes intérieures, bas) — la bannière préfixe chaque
/// partie pour le fichier, les cartes les joignent pour la console.
pub fn boxed(lines: &[String]) -> (String, Vec<String>, String) {
    let inner = lines.iter().map(|l| UnicodeWidthStr::width(l.as_str())).max().unwrap_or(0) + 2;
    let top = format!("╭{}╮", "─".repeat(inner));
    let bottom = format!("╰{}╯", "─".repeat(inner));
    let mid: Vec<String> = lines
        .iter()
        .map(|l| {
            let pad = inner - 1 - UnicodeWidthStr::width(l.as_str());
            format!("│ {}{}│", l, " ".repeat(pad))
        })
        .collect();
    (top, mid, bottom)
}

/// Le cadre joint en un seul texte (`boxed` suivi de `\n`).
pub fn boxed_text(lines: &[String]) -> String {
    let (top, mid, bottom) = boxed(lines);
    format!("{top}\n{}\n{bottom}", mid.join("\n"))
}

/// Colore `text` en cyan si la sortie est un vrai terminal (même règle que
/// la bannière : jamais de couleur sous superviseur/systemd, ni si
/// `NO_COLOR` est défini). Utilisé par les cartes console du mission control.
pub fn paint(text: &str) -> String {
    let guard = match state().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.color {
        format!("\x1b[36m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// Bannière de démarrage encadrée (console en cyan + fichier, chaque ligne
/// préfixée du format standard pour rester grep-friendly).
pub fn banner(lines: &[String]) {
    let st = state();
    let mut guard = match st.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    let (top, mid, bottom) = boxed(lines);

    // Version console (sous superviseur, la console de l'enfant est un
    // pipe : on saute l'écriture console, le fichier reçoit déjà la
    // bannière).
    if guard.console {
        let text = format!("{top}\n{}\n{bottom}", mid.join("\n"));
        let out = if guard.color {
            format!("\x1b[36m{text}\x1b[0m\n")
        } else {
            format!("{text}\n")
        };
        let _ = std::io::stdout().write_all(out.as_bytes());
    }

    // Version fichier : chaque ligne préfixée `ISO [INFO ] …`.
    if let Some(file) = guard.file.as_mut() {
        let iso = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let mut out = String::new();
        out.push_str(&format!("{iso} [INFO ] {top}\n"));
        for l in &mid {
            out.push_str(&format!("{iso} [INFO ] {l}\n"));
        }
        out.push_str(&format!("{iso} [INFO ] {bottom}\n"));
        let _ = file.write_all(out.as_bytes());
        let _ = file.flush();
    }
}

/// Anonymisation des identifiants dans les logs :
/// `1477929253072011266` → `1477…1266`. Les logs restent exploitables
/// (corrélation) sans exposer l'identifiant complet sur le disque.
pub fn short_id(id: impl AsRef<str>) -> String {
    let s = id.as_ref();
    if s.len() > 8 {
        format!("{}…{}", &s[..4], &s[s.len() - 4..])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_truncates_long_ids() {
        assert_eq!(short_id("1477929253072011266"), "1477…1266");
        assert_eq!(short_id("12345678"), "12345678");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn boxed_aligns_emoji_visual_width() {
        let text = boxed_text(&["🎮  apabot".to_string(), "👤  bot".to_string()]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4); // haut, 2 lignes, bas
        assert!(lines[0].starts_with("╭") && lines[0].ends_with("╮"));
        assert!(lines[3].starts_with("╰") && lines[3].ends_with("╯"));
        // Les deux lignes intérieures ont la même largeur visuelle : les
        // bordures droites s'alignent.
        assert_eq!(
            UnicodeWidthStr::width(lines[1]),
            UnicodeWidthStr::width(lines[2])
        );
        assert_eq!(lines[1].len(), lines[2].len());
    }
}
