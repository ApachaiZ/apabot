//! Construction des embeds Discord (cartes + dashboard modulaire).
//!
//! Portage de `lib/embeds.js`. Le dashboard est MODULAIRE : chaque ligne
//! (CPU, RAM, disque, joueurs, uptime, adresse, nœud) n'apparaît que si le
//! provider la renseigne — aucune ligne « Non rapporté » inutile. Joueurs
//! et uptime (puis adresse et nœud) se placent côte à côte quand ils vont
//! par paire, sinon pleine largeur.

use poise::serenity_prelude as serenity;
use serenity::{CreateEmbed, CreateEmbedFooter, Timestamp};

use crate::i18n::{fill, Catalog};
use crate::providers::Status;
use crate::services::display_name;

/// Ton de couleur d'une carte (parité avec `lib/embeds.js`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Info,
    Wait,
}

impl Tone {
    pub fn color(self) -> u32 {
        match self {
            Tone::Good => 0x2ecc71,
            Tone::Bad => 0xe74c3c,
            Tone::Info => 0x5865f2,
            Tone::Wait => 0xf1c40f,
        }
    }
}

/// Carte standard : titre + corps + couleur + pied de page + horodatage.
pub fn card(title: &str, text: &str, tone: Tone, footer: Option<&str>) -> CreateEmbed {
    CreateEmbed::new()
        .color(tone.color())
        .title(title)
        .description(text)
        .footer(CreateEmbedFooter::new(footer.unwrap_or("APABOT • GAME CONTROL")))
        .timestamp(Timestamp::now())
}

/// État d'un service, en minuscules (le contrat garantit le vocabulaire
/// commun, mais on se défend aussi contre un « online » brut).
pub fn state(status: &Status) -> String {
    status.state.trim().to_lowercase()
}

pub fn online(status: &Status) -> bool {
    matches!(state(status).as_str(), "running" | "online")
}

pub fn offline(status: &Status) -> bool {
    matches!(state(status).as_str(), "offline" | "stopped")
}

/// Formatage d'un nombre façon `toLocaleString("en-US")` : `2048` → `2,048`.
pub fn fmt_num(n: f64) -> String {
    let rounded = (n * 100.0).round() / 100.0;
    let (int_part, frac_part) = if rounded.fract().abs() < 1e-9 {
        (format!("{rounded:.0}"), String::new())
    } else {
        let s = format!("{rounded:.2}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        match s.split_once('.') {
            Some((int, frac)) => (int.to_string(), format!(".{frac}")),
            None => (s.to_string(), String::new()),
        }
    };
    // Groupement par milliers (en partant de la droite).
    let mut grouped = String::new();
    for (i, c) in int_part.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{}{}", grouped.chars().rev().collect::<String>(), frac_part)
}

/// Durée lisible : `3h 24m` / `1d 2h`. Unités volontairement neutres (d/h/m).
pub fn fmt_duration(total_seconds: f64) -> String {
    let s = total_seconds.max(0.0).floor() as u64;
    let d = s / 86400;
    let h = s % 86400 / 3600;
    let m = s % 3600 / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Barre visuelle sur 10 segments : `▰▰▰▰▱▱▱▱▱▱ 42.0%`.
/// `None` si l'usage ou le maximum n'est pas renseigné.
pub fn bar(used: Option<f64>, max: Option<f64>) -> Option<String> {
    let (used, max) = (used?, max?);
    if max <= 0.0 {
        return None;
    }
    let pct = (used / max * 100.0).clamp(0.0, 100.0);
    let filled = (pct / 10.0).round() as usize;
    Some(format!(
        "{}{} {:.1}%",
        "▰".repeat(filled),
        "▱".repeat(10 - filled),
        pct
    ))
}

/// Dashboard `/status` modulaire (voir doc en tête de module).
pub fn dashboard(status: &Status, alias: &str, catalog: &Catalog) -> CreateEmbed {
    let tone = if online(status) {
        Tone::Good
    } else if offline(status) {
        Tone::Bad
    } else {
        Tone::Wait
    };
    let state_label = if online(status) {
        catalog.status.state_running
    } else if offline(status) {
        catalog.status.state_offline
    } else {
        catalog.status.state_unknown
    };
    let embed = card(
        &fill(catalog.status.title, &[("name", &display_name(status, alias))]),
        state_label,
        tone,
        None,
    );

    // Lignes métriques pleine largeur (si renseignées).
    let mut fields: Vec<(String, String, bool)> = Vec::new();
    if let Some(bar) = bar(status.cpu_pct, Some(100.0)) {
        fields.push((catalog.status.cpu.to_string(), bar, false));
    }
    if let (Some(used), Some(max)) = (status.ram_mb, status.ram_max_mb) {
        if let Some(bar) = bar(Some(used), Some(max)) {
            fields.push((
                catalog.status.memory.to_string(),
                format!("{} / {} MB\n{}", fmt_num(used), fmt_num(max), bar),
                false,
            ));
        }
    }
    if let (Some(used), Some(max)) = (status.disk_mb, status.disk_max_mb) {
        if let Some(bar) = bar(Some(used), Some(max)) {
            fields.push((
                catalog.status.storage.to_string(),
                format!("{} / {} MB\n{}", fmt_num(used), fmt_num(max), bar),
                false,
            ));
        }
    }

    // « Puces » courtes : joueurs et uptime côte à côte s'ils vont par paire.
    let mut chips: Vec<(String, String, bool)> = Vec::new();
    if let Some(players) = status.players {
        chips.push((catalog.status.players.to_string(), format!("{players:.0}"), false));
    }
    if let Some(uptime) = status.uptime_seconds {
        chips.push((catalog.status.uptime.to_string(), fmt_duration(uptime), false));
    }
    let chips_inline = chips.len() > 1;
    for mut chip in chips {
        chip.2 = chips_inline;
        fields.push(chip);
    }

    // Infos : adresse et nœud côte à côte s'ils vont par paire.
    let mut info: Vec<(String, String, bool)> = Vec::new();
    if let Some(addr) = &status.address {
        let value = match addr.port {
            // Amélioration par rapport au JS (qui affichait `ip:null`) :
            // sans port, on n'affiche que l'IP.
            Some(port) => format!("{}:{port}", addr.ip),
            None => addr.ip.clone(),
        };
        info.push((catalog.status.address.to_string(), value, false));
    }
    if let Some(node) = &status.node {
        info.push((catalog.status.node.to_string(), node.clone(), false));
    }
    let info_inline = info.len() > 1;
    // Séparateur invisible : sans lui, deux paires inline consécutives
    // seraient regroupées par Discord en une grille 3+1.
    if chips_inline && info_inline {
        fields.push(("\u{200b}".to_string(), "\u{200b}".to_string(), false));
    }
    for mut i in info {
        i.2 = info_inline;
        fields.push(i);
    }

    if fields.is_empty() {
        fields.push((
            catalog.status.metrics.to_string(),
            catalog.status.not_reported.to_string(),
            false,
        ));
    }

    let fields: Vec<(String, String, bool)> = fields;
    let mut embed = embed;
    embed = embed.fields(fields);
    embed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_groups_thousands() {
        assert_eq!(fmt_num(2048.0), "2,048");
        assert_eq!(fmt_num(42.5), "42.5");
        assert_eq!(fmt_num(42.05), "42.05");
        assert_eq!(fmt_num(0.0), "0");
    }

    #[test]
    fn fmt_duration_uses_neutral_units() {
        assert_eq!(fmt_duration(3.0 * 3600.0 + 24.0 * 60.0), "3h 24m");
        assert_eq!(fmt_duration(2.0 * 86400.0 + 3600.0), "2d 1h");
        assert_eq!(fmt_duration(59.0), "0m");
    }

    #[test]
    fn bar_clamps_and_rounds() {
        assert_eq!(bar(Some(42.0), Some(100.0)).unwrap(), "▰▰▰▰▱▱▱▱▱▱ 42.0%");
        assert_eq!(bar(Some(150.0), Some(100.0)).unwrap(), "▰▰▰▰▰▰▰▰▰▰ 100.0%");
        assert_eq!(bar(None, Some(100.0)), None);
        assert_eq!(bar(Some(10.0), Some(0.0)), None);
    }

    #[test]
    fn online_offline_detection() {
        let s: Status = serde_json::from_value(serde_json::json!({"state": "running"})).unwrap();
        assert!(online(&s));
        let s: Status = serde_json::from_value(serde_json::json!({"state": "stopped"})).unwrap();
        assert!(offline(&s));
        let s: Status = serde_json::from_value(serde_json::json!({"state": "starting"})).unwrap();
        assert!(!online(&s) && !offline(&s));
    }
}
