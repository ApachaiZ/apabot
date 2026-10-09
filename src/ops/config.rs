//! Commande `config` : questionnaire interactif + PROFILS de configuration.
//!
//! - `apabot config` : relance le questionnaire du setup (langue, token,
//!   IDs Discord, clé provider, services) sur le `.env` ACTIF — valeurs
//!   existantes préservées, seules les réponses changées sont réécrites
//!   (même mécanique que le premier lancement) ;
//! - `apabot config save <nom>` : sauvegarde le `.env` courant en profil
//!   (`.config.d/profiles/<nom>.env`, mode 0600) ;
//! - `apabot config load <nom>` : remplace le `.env` courant par un
//!   profil (avec confirmation — le fichier actif est écrasé) ;
//! - `apabot config list` / `config delete <nom>` : gestion des profils.
//!
//! Après un changement (edit/load), si le daemon tourne, on PROPOSE de
//! redémarrer le bot pour appliquer la nouvelle configuration.

use dialoguer::Confirm;
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::i18n::{fill, Catalog};
use crate::paths;

fn profiles_dir() -> PathBuf {
    paths::config_dir().join("profiles")
}

fn profile_path(name: &str) -> PathBuf {
    profiles_dir().join(format!("{name}.env"))
}

/// Nom d'instance sûr : alphanumérique + `-`/`_`/`.`, sans `..`, < 64.
/// Partagé : profils de configuration ET instances de bots.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 64
        && name != "."
        && name != ".."
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Après un changement de config : si le daemon tourne, proposer un
/// redémarrage du bot (la config est relue au démarrage).
pub(crate) async fn offer_restart(catalog: &'static Catalog) {
    let Some(st) = crate::ops::state::read() else { return };
    if !crate::ops::state::pid_alive(st.pid) || !crate::ops::protocol::is_alive(&st).await {
        return;
    }
    if Confirm::new()
        .with_prompt(catalog.ops.config_restart_prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        let _ = crate::ops::protocol::request(&st, "restart").await;
    }
}

fn edit(catalog: &'static Catalog) -> i32 {
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", catalog.ops.config_no_tty);
        return 1;
    }
    match crate::setup::ensure_env_at(&paths::env_file(), false, catalog) {
        Ok(()) => {
            println!("{}", catalog.ops.config_updated);
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

async fn save(name: Option<&String>, catalog: &'static Catalog) -> i32 {
    let Some(name) = name else {
        eprintln!("{}", catalog.ops.config_usage);
        return 2;
    };
    if !valid_name(name) {
        eprintln!("{}", catalog.ops.profile_invalid_name);
        return 1;
    }
    if !paths::env_file().is_file() {
        eprintln!("{}", catalog.ops.config_no_env);
        return 1;
    }
    let dir = profiles_dir();
    if std::fs::create_dir_all(&dir).is_err() && !dir.is_dir() {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", "mkdir")]));
        return 1;
    }
    let target = profile_path(name);
    if let Err(e) = std::fs::copy(paths::env_file(), &target) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
    }
    println!(
        "{}",
        fill(catalog.ops.profile_saved, &[("name", name), ("file", &target.display().to_string())])
    );
    0
}

async fn load(name: Option<&String>, catalog: &'static Catalog) -> i32 {
    let Some(name) = name else {
        eprintln!("{}", catalog.ops.config_usage);
        return 2;
    };
    let source = profile_path(name);
    if !source.is_file() {
        eprintln!("{}", fill(catalog.ops.profile_missing, &[("name", name)]));
        return 1;
    }
    if !Confirm::new()
        .with_prompt(fill(catalog.ops.profile_confirm_load, &[("name", name)]))
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    if let Err(e) = std::fs::copy(&source, paths::env_file()) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(paths::env_file(), std::fs::Permissions::from_mode(0o600));
    }
    println!("{}", fill(catalog.ops.profile_loaded, &[("name", name)]));
    offer_restart(catalog).await;
    0
}

fn list(catalog: &'static Catalog) -> i32 {
    let dir = profiles_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("{}", catalog.ops.profile_none);
        return 0;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".env").map(String::from)
        })
        .collect();
    names.sort();
    if names.is_empty() {
        println!("{}", catalog.ops.profile_none);
    } else {
        println!("{}", catalog.ops.profile_list_header);
        for name in names {
            println!("  {name}");
        }
    }
    0
}

async fn delete(name: Option<&String>, catalog: &'static Catalog) -> i32 {
    let Some(name) = name else {
        eprintln!("{}", catalog.ops.config_usage);
        return 2;
    };
    let target = profile_path(name);
    if !target.is_file() {
        eprintln!("{}", fill(catalog.ops.profile_missing, &[("name", name)]));
        return 1;
    }
    if !Confirm::new()
        .with_prompt(fill(catalog.ops.profile_confirm_delete, &[("name", name)]))
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{}", catalog.ops.systemd_aborted);
        return 0;
    }
    if let Err(e) = std::fs::remove_file(&target) {
        eprintln!("{}", fill(catalog.ops.install_failed, &[("error", &e.to_string())]));
        return 1;
    }
    println!("{}", fill(catalog.ops.profile_deleted, &[("name", name)]));
    0
}

/// Point d'entrée de `apabot config …`.
pub async fn cmd_config(args: &[String], catalog: &'static Catalog) -> i32 {
    match args.get(2).map(String::as_str) {
        None | Some("edit") => edit(catalog),
        Some("save") => save(args.get(3), catalog).await,
        Some("load") => load(args.get(3), catalog).await,
        Some("list") | Some("ls") => list(catalog),
        Some("delete") | Some("rm") => delete(args.get(3), catalog).await,
        _ => {
            eprintln!("{}", catalog.ops.config_usage);
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_are_validated() {
        assert!(valid_name("main"));
        assert!(valid_name("main-2_prod"));
        assert!(!valid_name(""));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(64)));
    }

    #[test]
    fn profile_path_is_scoped_under_profiles_dir() {
        let p = profile_path("main");
        assert!(p.ends_with("profiles/main.env"));
    }
}
