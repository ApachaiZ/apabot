//! Chemins canoniques du bot.
//!
//! Tous les chemins sont relatifs au RÉPERTOIRE COURANT au lancement (mode
//! portable) — ou à la racine des données d'une installation (le binaire
//! installé s'y `chdir` au démarrage, voir `ops::install`).
//!
//! Pourquoi pas le chemin de l'exécutable (`current_exe`) ? Parce que sous
//! `cargo build`, le binaire vit dans `target/{debug,release}/` — les états
//! atterriraient au mauvais endroit.

use std::path::PathBuf;

/// Racine du projet = répertoire courant au lancement.
pub fn root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Dossier `.config.d/` (secrets, état, verrous, profils).
pub fn config_dir() -> PathBuf {
    root().join(".config.d")
}

pub fn env_file() -> PathBuf {
    config_dir().join(".env")
}

pub fn logs_dir() -> PathBuf {
    root().join("logs")
}

pub fn log_file() -> PathBuf {
    logs_dir().join("apabot.log")
}

pub fn users_file() -> PathBuf {
    config_dir().join("users.json")
}

/// Emplacement hérité d'avant la migration vers `.config.d/` (compatibilité).
pub fn legacy_users_file() -> PathBuf {
    root().join("apach-users.json")
}

pub fn sessions_file() -> PathBuf {
    config_dir().join("sessions.json")
}

pub fn lock_file() -> PathBuf {
    config_dir().join("active.lock")
}

pub fn commands_hash_file() -> PathBuf {
    config_dir().join("commands.hash")
}

pub fn members_state_file() -> PathBuf {
    config_dir().join("members.state.json")
}

pub fn members_roster_file() -> PathBuf {
    config_dir().join("members.roster.json")
}

pub fn stats_file() -> PathBuf {
    config_dir().join("stats.json")
}

/// État du superviseur embarqué (`mission control`) : pid, port du canal de
/// contrôle, jeton d'authentification, horodatage. Écrit par le superviseur,
/// lu par les commandes `start`/`stop`/`restart`/`status`.
pub fn daemon_state_file() -> PathBuf {
    config_dir().join("daemon.json")
}

/// GIF animé d'un état d'action power (`loading`, `waiting`, `success`, `error`).
pub fn gif_path(kind: &str) -> PathBuf {
    root().join("assets").join("emojis").join(format!("{kind}.gif"))
}
