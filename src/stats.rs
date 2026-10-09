//! Statistiques persistées : durée moyenne des actions power.
//!
//! Utilisées pour la ligne d'estimation de la carte « Action acceptée »
//! (« ⏱ En moyenne : 51 s »). Format fichier : `{ action: {count, sumMs} }`
//! — non chiffré, ce sont de simples durées (aucune donnée privée).

use serde_json::Value;
use std::collections::HashMap;

use crate::fsutil;
use crate::logger;
use crate::paths;

fn read_all() -> HashMap<String, (f64, f64)> {
    let Ok(raw) = std::fs::read_to_string(paths::stats_file()) else {
        return HashMap::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };
    let Some(obj) = data.as_object() else {
        return HashMap::new();
    };
    obj.iter()
        .filter_map(|(action, entry)| {
            let count = entry.get("count")?.as_f64()?;
            let sum = entry.get("sumMs")?.as_f64()?;
            Some((action.clone(), (count, sum)))
        })
        .collect()
}

fn write_all(map: &HashMap<String, (f64, f64)>) {
    let mut obj = serde_json::Map::new();
    for (action, (count, sum)) in map {
        obj.insert(
            action.clone(),
            serde_json::json!({ "count": count, "sumMs": sum }),
        );
    }
    if let Err(e) = fsutil::atomic_write(paths::stats_file().as_path(), serde_json::json!(obj).to_string().as_bytes())
    {
        logger::warn(format!("stats: impossible d'écrire {} : {e}", paths::stats_file().display()));
    }
}

/// Durée moyenne en secondes (arrondie, minimum 1), ou `None` si aucune
/// transaction connue pour cette action.
pub fn avg_seconds(action: &str) -> Option<u64> {
    let (count, sum_ms) = read_all().get(action).copied()?;
    if count <= 0.0 || sum_ms <= 0.0 {
        return None;
    }
    Some(((sum_ms / count / 1000.0).round() as u64).max(1))
}

/// Enregistre la durée d'une transaction réussie (ms).
pub fn record(action: &str, elapsed_ms: u64) {
    let mut all = read_all();
    let entry = all.entry(action.to_string()).or_insert((0.0, 0.0));
    entry.0 += 1.0;
    entry.1 += elapsed_ms as f64;
    write_all(&all);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_formula_rounds_and_floors_at_one() {
        // `avg_seconds` lit le fichier réel (chemin fixe, parité JS) : on
        // teste donc la FORMULE de moyenne isolément.
        let (count, sum_ms): (f64, f64) = (2.0, 102_000.0);
        let avg = ((sum_ms / count / 1000.0).round() as u64).max(1);
        assert_eq!(avg, 51);
        let (count, sum_ms): (f64, f64) = (1.0, 100.0);
        let avg = ((sum_ms / count / 1000.0).round() as u64).max(1);
        assert_eq!(avg, 1);
    }
}
