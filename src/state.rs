//! État partagé du bot : opérateurs, verrous anti-collision, sessions.
//!
//! Parité avec `lib/state.js` :
//! - `users.json` : liste des opérateurs, chiffrée AES-256-GCM, en mémoire
//!   après chargement (source de vérité : le fichier, réécrit à chaque
//!   mutation) ;
//! - `active.lock` : un verrou PAR SERVICE (`{alias: {action, startedAt}}`),
//!   TTL 11 minutes, écritures atomiques ;
//! - `sessions.json` : service par défaut par utilisateur, chiffré.
//!
//! Conception Rust : [`State`] est un objet partagé via `Arc` entre toutes
//! les tâches (commandes + watchdog). Ses champs mutables sont derrière des
//! `std::sync::Mutex` — le choix du mutex STANDARD (et non tokio) est
//! délibéré : les sections critiques sont courtes et ne contiennent aucun
//! `.await`, donc pas de risque de bloquer l'exécuteur.
//!
//! Amélioration : les sessions sont lues UNE fois au démarrage puis tenues
//! en mémoire (le JS relisait le fichier à chaque appel) — même comportement
//! observable, moins d'IO.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::crypto;
use crate::fsutil;
use crate::i18n::{fill, Catalog};
use crate::logger;
use crate::paths;

/// Durée de vie d'un verrou : au-delà, il est considéré comme orphelin
/// (crash) et purgé à la prochaine lecture.
const LOCK_TTL: Duration = Duration::from_secs(11 * 60);

/// Horodatage UNIX en millisecondes (même unité que `Date.now()` du JS :
/// le fichier `active.lock` reste interopérable entre les deux versions).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Migration silencieuse : l'ancien emplacement (racine) est déplacé vers
/// `.config.d/users.json` s'il existe encore — aucune donnée perdue.
fn migrate_legacy_users() {
    if !paths::users_file().exists() && paths::legacy_users_file().exists() {
        let _ = std::fs::create_dir_all(paths::config_dir());
        let _ = std::fs::rename(paths::legacy_users_file(), paths::users_file());
    }
}

/// Validation PURE du contenu de `users.json` : tableau d'IDs numériques,
/// dédupliqués. Séparée de l'IO pour être testable sans disque.
fn clean_user_list(data: &Value, catalog: &Catalog) -> Result<Vec<String>, String> {
    let Some(arr) = data.as_array() else {
        logger::error(catalog.audit.users_not_array);
        return Err(catalog.errors.invalid_users_json_shape.to_string());
    };
    let mut clean: Vec<String> = Vec::new();
    for entry in arr {
        match entry.as_str() {
            Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) => {
                if !clean.contains(&id.to_string()) {
                    clean.push(id.to_string());
                }
            }
            _ => {
                logger::warn(fill(
                    catalog.audit.users_invalid_entry,
                    &[("entry", &logger::short_id(entry.to_string()))],
                ));
            }
        }
    }
    Ok(clean)
}

/// Charge et valide `users.json` : tableau d'IDs numériques, dédupliqués.
/// Un fichier illisible est un blocage DÉLIBÉRÉ (comme en JS : exit 1) —
/// valider à la volée masquerait une corruption du fichier d'opérateurs.
fn load_users(token: &str, catalog: &Catalog) -> Result<Vec<String>> {
    if !paths::users_file().exists() {
        return Ok(Vec::new());
    }
    let Some(data) = crypto::read_json(token, &paths::users_file()) else {
        return Ok(Vec::new());
    };
    clean_user_list(&data, catalog).map_err(|msg| anyhow!(msg))
}

fn load_sessions(token: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(data) = crypto::read_json(token, &paths::sessions_file()) {
        if let Some(obj) = data.as_object() {
            for (uid, alias) in obj {
                if let Some(alias) = alias.as_str() {
                    map.insert(uid.clone(), alias.to_string());
                }
            }
        }
    }
    map
}

pub struct State {
    token: String,
    owner_id: String,
    catalog: &'static Catalog,
    users: Mutex<Vec<String>>,
    sessions: Mutex<HashMap<String, String>>,
    /// Horodatage de la dernière fin d'action power volontaire, par service.
    /// Mémoire uniquement : un restart du bot n'a pas besoin de persister
    /// cette information (délai de grâce anti-fausse-alerte du watchdog).
    lock_releases: Mutex<HashMap<String, Instant>>,
}

impl State {
    pub fn new(token: &str, owner_id: &str, catalog: &'static Catalog) -> Result<Self> {
        migrate_legacy_users();
        let users = load_users(token, catalog)?;
        let sessions = load_sessions(token);
        Ok(Self {
            token: token.to_string(),
            owner_id: owner_id.to_string(),
            catalog,
            users: Mutex::new(users),
            sessions: Mutex::new(sessions),
            lock_releases: Mutex::new(HashMap::new()),
        })
    }

    // ── Autorisations ──────────────────────────────────────────────────────

    pub fn is_owner(&self, id: &str) -> bool {
        id == self.owner_id
    }

    pub fn is_allowed(&self, id: &str) -> bool {
        self.is_owner(id) || self.get_users().iter().any(|u| u == id)
    }

    pub fn get_users(&self) -> Vec<String> {
        // `lock().unwrap()` : un empoisonnement (panic pendant la tenue du
        // verrou) n'a pas de sens ici — on préfère récupérer l'état.
        self.users.lock().unwrap().clone()
    }

    /// Sauvegarde chiffrée de la liste des opérateurs, puis bascule mémoire.
    pub fn save_users(&self, next: Vec<String>) {
        let obj = Value::Array(next.iter().map(|u| Value::String(u.clone())).collect());
        if let Err(e) = crypto::write_json(&self.token, &paths::users_file(), &obj) {
            logger::error(fill(
                self.catalog.audit.sessions_write_failed,
                &[("error", &e.to_string())],
            ));
        }
        *self.users.lock().unwrap() = next;
    }

    // ── Sessions (service par défaut par utilisateur) ──────────────────────

    pub fn get_session(&self, user_id: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(user_id).cloned()
    }

    fn write_sessions(&self) {
        let map = self.sessions.lock().unwrap();
        let obj = Value::Object(
            map.iter()
                .map(|(uid, alias)| (uid.clone(), Value::String(alias.clone())))
                .collect(),
        );
        if let Err(e) = crypto::write_json(&self.token, &paths::sessions_file(), &obj) {
            logger::error(fill(
                self.catalog.audit.sessions_write_failed,
                &[("error", &e.to_string())],
            ));
        }
    }

    pub fn set_session(&self, user_id: &str, alias: &str) {
        self.sessions
            .lock()
            .unwrap()
            .insert(user_id.to_string(), alias.to_string());
        self.write_sessions();
    }

    pub fn clear_session(&self, user_id: &str) {
        let removed = self.sessions.lock().unwrap().remove(user_id).is_some();
        if removed {
            self.write_sessions();
        }
    }

    // ── Verrous anti-collision (fichier, source de vérité sur disque) ──────

    /// Lecture brute du fichier de verrou (tolérante : fichier absent ou
    /// corrompu → verrous vides).
    fn read_lock_file(&self) -> HashMap<String, (String, u64)> {
        let Ok(raw) = std::fs::read_to_string(paths::lock_file()) else {
            return HashMap::new();
        };
        let Ok(data) = serde_json::from_str::<Value>(&raw) else {
            return HashMap::new();
        };
        let Some(obj) = data.as_object() else {
            return HashMap::new();
        };
        obj.iter()
            .filter_map(|(alias, entry)| {
                let action = entry.get("action")?.as_str()?;
                let started_at = entry.get("startedAt")?.as_u64()?;
                Some((alias.clone(), (action.to_string(), started_at)))
            })
            .collect()
    }

    /// Écriture atomique du fichier de verrou (tmp + rename, mode 0600).
    fn write_lock_file(&self, map: &HashMap<String, (String, u64)>) {
        let obj: serde_json::Map<String, Value> = map
            .iter()
            .map(|(alias, (action, started_at))| {
                (
                    alias.clone(),
                    json!({ "action": action, "startedAt": started_at }),
                )
            })
            .collect();
        if let Err(e) = fsutil::secure_write(
            &paths::lock_file(),
            serde_json::to_string(&Value::Object(obj))
                .unwrap_or_default()
                .as_bytes(),
        ) {
            logger::error(fill(
                self.catalog.audit.lock_write_failed,
                &[("error", &e.to_string())],
            ));
        }
    }

    /// Pose un verrou pour un service.
    pub fn write_lock(&self, alias: &str, action: &str) {
        let mut map = self.read_lock_file();
        map.insert(alias.to_string(), (action.to_string(), now_ms()));
        self.write_lock_file(&map);
    }

    /// Lève le verrou d'un service (et supprime le fichier s'il ne reste
    /// plus aucun verrou — un fichier vide est inutile sur disque).
    pub fn clear_lock(&self, alias: &str) {
        let mut map = self.read_lock_file();
        if map.remove(alias).is_none() {
            return;
        }
        if map.is_empty() {
            let _ = std::fs::remove_file(paths::lock_file());
            return;
        }
        self.write_lock_file(&map);
    }

    /// Verrous actifs (expirés purgés), en RÉÉCRIVANT le fichier si des
    /// entrées ont expiré. Utilisé par le flux power : l'état disque est la
    /// source de vérité (un autre processus ou une édition manuelle peut
    /// l'avoir modifié).
    pub fn load_persisted_lock(&self) -> HashMap<String, String> {
        let map = self.read_lock_file();
        let now = now_ms();
        let mut changed = false;
        let mut active: HashMap<String, String> = HashMap::new();
        for (alias, (action, started_at)) in &map {
            if now.saturating_sub(*started_at) > LOCK_TTL.as_millis() as u64 {
                changed = true;
                continue;
            }
            active.insert(alias.clone(), action.clone());
        }
        if changed {
            if active.is_empty() {
                let _ = std::fs::remove_file(paths::lock_file());
            } else {
                // Conserver le startedAt d'origine des verrous survivants :
                // ne pas réinitialiser leur TTL.
                let survivors: HashMap<String, (String, u64)> = map
                    .into_iter()
                    .filter(|(_, (_, started_at))| now.saturating_sub(*started_at) <= LOCK_TTL.as_millis() as u64)
                    .collect();
                self.write_lock_file(&survivors);
            }
        }
        active
    }

    /// LECTURE PURE (utilisée par le watchdog) : ne réécrit JAMAIS le
    /// fichier — un scan de surveillance ne doit pas déclencher d'écriture.
    pub fn read_lock_snapshot(&self) -> HashMap<String, String> {
        let now = now_ms();
        self.read_lock_file()
            .into_iter()
            .filter(|(_, (_, started_at))| now.saturating_sub(*started_at) <= LOCK_TTL.as_millis() as u64)
            .map(|(alias, (action, _))| (alias, action))
            .collect()
    }

    // ── Délai de grâce anti-fausse-alerte ──────────────────────────────────

    /// À la fin d'une action power volontaire : le watchdog ignore la
    /// transition running → arrêt qui en découle pendant 5 minutes.
    pub fn record_lock_release(&self, alias: &str) {
        self.lock_releases
            .lock()
            .unwrap()
            .insert(alias.to_string(), Instant::now());
    }

    /// Temps écoulé depuis la dernière fin d'action volontaire sur ce
    /// service (0 si aucune).
    pub fn lock_release_elapsed(&self, alias: &str) -> Duration {
        self.lock_releases
            .lock()
            .unwrap()
            .get(alias)
            .map(|at| at.elapsed())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n;
    use serde_json::json;

    // NOTE : aucun test ici ne touche au disque. Les chemins d'état sont
    // fixes (parité JS) : la logique écrite sur disque est triviale
    // (sérialisation), toute la subtilité est dans la VALIDATION, testée
    // via `clean_user_list`.

    #[test]
    fn validates_and_dedups_user_lists() {
        let cat = &i18n::en::CATALOG;
        let users = clean_user_list(&json!(["123", "123", "456"]), cat).unwrap();
        assert_eq!(users, vec!["123", "456"]);
        // Entrée invalide ignorée (avec log), valides conservées.
        let users = clean_user_list(&json!(["123", "not-a-number", "789"]), cat).unwrap();
        assert_eq!(users, vec!["123", "789"]);
        // Pas un tableau → erreur.
        assert!(clean_user_list(&json!({}), cat).is_err());
    }

    #[test]
    fn owner_is_always_allowed() {
        let state = State::new("t", "42", &i18n::en::CATALOG).unwrap();
        assert!(state.is_allowed("42"));
        assert!(!state.is_allowed("7"));
    }

    #[test]
    fn missing_session_is_none() {
        let state = State::new("t", "42", &i18n::en::CATALOG).unwrap();
        assert_eq!(state.get_session("u1"), None);
    }

    #[test]
    fn lock_snapshot_tolerates_missing_file() {
        // La lecture pure tolère un fichier absent et n'écrit rien.
        let state = State::new("t", "42", &i18n::en::CATALOG).unwrap();
        assert!(state.read_lock_snapshot().is_empty());
        assert!(state.load_persisted_lock().is_empty());
    }
}
